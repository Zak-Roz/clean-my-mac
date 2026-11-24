//! Allowlisted external commands (ARCHITECTURE.md §6.4). Programs are resolved and run by the
//! shared helpers in [`crate::process`] (contract v2: one timed runner, one resolver).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::process;

/// The only programs a `Command` / `GitWorktreeRemove` action may run. Matched exactly against
/// `argv[0]`, which must be a bare name (no path): `/tmp/x/git` is refused.
pub const COMMAND_ALLOWLIST: &[&str] = &["xcrun", "docker", "git", "tmutil", "brew", "npm", "pnpm", "yarn"];

/// Errors unless `program` is on the allowlist.
pub(crate) fn check_allowed(program: &str) -> Result<(), String> {
    if COMMAND_ALLOWLIST.contains(&program) {
        Ok(())
    } else {
        Err(format!("`{program}` is not an allowed program (allowed: {})", COMMAND_ALLOWLIST.join(", ")))
    }
}

/// Allowlist check, then the absolute path of the executable in [`process::TOOL_DIRS`]
/// (never `$PATH`).
pub(crate) fn resolve_allowed(program: &str) -> Result<PathBuf, String> {
    check_allowed(program)?;
    process::resolve_tool(program).map_err(|_| format!("`{program}` not found"))
}

pub(crate) struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs the absolute `program` with `args` through [`process::run_with_timeout`] (stdin closed, own
/// process group killed after `timeout`), with output as text.
pub(crate) fn run_with_timeout(program: &Path, args: &[String], timeout: Duration) -> Result<Output, String> {
    let out = process::run_with_timeout(program, args, timeout).map_err(|e| match e {
        crate::error::CoreError::Command(m) => m,
        other => other.to_string(),
    })?;
    Ok(Output {
        success: out.status.success(),
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
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
    use std::time::Instant;

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
