//! Allowlisted external commands with a timeout (ARCHITECTURE.md §6.4).

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The only programs a `Command` / `GitWorktreeRemove` action may run. Matched exactly against
/// `argv[0]`, which must be a bare name (no path): `/tmp/x/git` is refused.
pub const COMMAND_ALLOWLIST: &[&str] = &["xcrun", "docker", "git", "tmutil", "brew", "npm", "pnpm", "yarn"];

/// Searched after `$PATH`: GUI apps start with a minimal PATH that lacks Homebrew and Docker.
const FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

/// Errors unless `program` is on the allowlist.
pub(crate) fn check_allowed(program: &str) -> Result<(), String> {
    if COMMAND_ALLOWLIST.contains(&program) {
        Ok(())
    } else {
        Err(format!("`{program}` is not an allowed program (allowed: {})", COMMAND_ALLOWLIST.join(", ")))
    }
}

/// Allowlist check, then the absolute path of the executable.
pub(crate) fn resolve_allowed(program: &str) -> Result<PathBuf, String> {
    check_allowed(program)?;
    let path_dirs = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(FALLBACK_DIRS.iter().map(PathBuf::from))
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
        .ok_or_else(|| format!("`{program}` not found"))
}

fn is_executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

pub(crate) struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `program args…` (stdin closed, own process group), killing the whole group after `timeout`.
pub(crate) fn run_with_timeout(program: &Path, args: &[String], timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", program.display()))?;

    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let stdout = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                kill_group(&mut child);
                let _ = child.wait();
                break Err(format!("timed out after {}s", timeout.as_secs_f32()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                kill_group(&mut child);
                let _ = child.wait();
                break Err(format!("wait failed: {e}"));
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    let status = status?;
    Ok(Output { success: status.success(), code: status.code(), stdout, stderr })
}

fn kill_group(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: plain syscall; the child leads its own process group (process_group(0)).
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Shell-like rendering for messages: `docker system prune -f`.
pub(crate) fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| if a.is_empty() || a.contains(char::is_whitespace) { format!("'{a}'") } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Last `max` characters of trimmed output, for item messages.
pub(crate) fn tail(text: &str, max: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max { text.to_string() } else { format!("…{}", text.chars().skip(count - max).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_is_exact_and_bare() {
        assert!(check_allowed("git").is_ok());
        assert!(check_allowed("docker").is_ok());
        for bad in ["rm", "sh", "bash", "/usr/bin/git", "./git", "git ", "Git", "sudo", ""] {
            assert!(check_allowed(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(resolve_allowed("rm").is_err());
        assert!(resolve_allowed("git").unwrap().is_absolute());
    }

    #[test]
    fn captures_output_and_exit_code() {
        let sh = Path::new("/bin/sh");
        let out =
            run_with_timeout(sh, &["-c".into(), "echo out; echo err >&2; exit 3".into()], Duration::from_secs(10))
                .unwrap();
        assert!(!out.success);
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout.trim(), "out");
        assert_eq!(out.stderr.trim(), "err");
    }

    #[test]
    fn kills_on_timeout_including_grandchildren() {
        let started = Instant::now();
        // The grandchild `sleep` keeps the pipes open; killing only `sh` would hang the readers.
        let r = run_with_timeout(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 30 & sleep 30".into()],
            Duration::from_millis(200),
        );
        assert!(matches!(r, Err(ref m) if m.contains("timed out")), "{:?}", r.as_ref().err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn helpers() {
        assert_eq!(
            display_argv(&["docker".into(), "system".into(), "prune".into(), "-f".into()]),
            "docker system prune -f"
        );
        assert_eq!(display_argv(&["git".into(), "-C".into(), "/a b".into()]), "git -C '/a b'");
        assert_eq!(tail("  abcdef  ", 3), "…def");
        assert_eq!(tail("abc", 3), "abc");
    }
}
