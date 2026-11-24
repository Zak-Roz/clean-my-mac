//! Running-process snapshot used for `requires_quit` blockers, and the one bounded runner used for
//! every external tool, by detectors (`git`, `xcrun`, `docker`, `mdls`) and the clean executor alike
//! (owned by S1; contract v2).
//!
//! Tools are resolved to absolute paths in [`TOOL_DIRS`], never through `$PATH`: an app launched
//! from Finder has a minimal `PATH` without Homebrew or Docker, and a planted `~/bin/git` must not
//! be picked up by the executor.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

use crate::error::CoreError;

/// Upper bound for one external tool invocation (ARCHITECTURE.md §10).
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(20);

/// Where external tools are looked up, in order (ARCHITECTURE.md §10, contract v2).
pub const TOOL_DIRS: &[&str] =
    &["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin", "/Applications/Docker.app/Contents/Resources/bin"];

/// Absolute path of the external tool `name` (a bare file name such as `git`): the first
/// executable `<dir>/<name>` in [`TOOL_DIRS`]. `CoreError::Command("<name>: not found")` otherwise.
pub fn resolve_tool(name: &str) -> Result<PathBuf, CoreError> {
    resolve_tool_in(name, TOOL_DIRS.iter().map(Path::new))
}

fn resolve_tool_in<'a>(name: &str, dirs: impl IntoIterator<Item = &'a Path>) -> Result<PathBuf, CoreError> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(CoreError::Command(format!("{name:?}: not a bare program name")));
    }
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
        .ok_or_else(|| CoreError::Command(format!("{name}: not found")))
}

fn is_executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Names of the processes running when the snapshot was captured.
#[derive(Debug, Clone, Default)]
pub struct ProcessSnapshot {
    names: HashSet<String>,
}

impl ProcessSnapshot {
    /// Captures process names (and executable file names). Empty if the process list is unavailable.
    pub fn capture() -> Self {
        let mut names = HashSet::new();
        let refresh = ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet);
        if let Ok(mut system) = System::new_with_specifics(RefreshKind::nothing().with_processes(refresh)) {
            system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
            for process in system.processes().values() {
                names.insert(process.name().to_string_lossy().into_owned());
                if let Some(exe_name) = process.exe().and_then(|exe| exe.file_name()) {
                    names.insert(exe_name.to_string_lossy().into_owned());
                }
            }
        }
        Self { names }
    }

    /// Builds a snapshot from explicit names (tests, fixtures).
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { names: names.into_iter().map(Into::into).collect() }
    }

    /// Exact (case-sensitive) match against process or executable name.
    pub fn is_running(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}

/// Runs `program args...` with stdin closed and stdout/stderr captured, killing its whole process
/// group once `timeout` has passed. A bare `program` name is resolved with [`resolve_tool`]; a path
/// (containing `/`) is run as given. The environment is non-interactive and stable for parsing:
/// `LC_ALL=C`, `GIT_TERMINAL_PROMPT=0` and `GIT_OPTIONAL_LOCKS=0` (a `git status` never rewrites
/// the index, so scans stay read-only).
///
/// Returns the output whatever the exit status (check `output.status`). Errors
/// ([`CoreError::Command`]) only when the program cannot be found or started (e.g. "git: not found")
/// or did not finish in time. Never panics.
pub fn run_with_timeout<P, S>(program: P, args: &[S], timeout: Duration) -> Result<Output, CoreError>
where
    P: AsRef<OsStr>,
    S: AsRef<OsStr>,
{
    let program = program.as_ref();
    let shown = program.to_string_lossy().into_owned();
    let path = match program.to_str() {
        Some(name) if !name.contains('/') => resolve_tool(name)?,
        _ => PathBuf::from(program),
    };
    let mut child = Command::new(&path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .process_group(0)
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => CoreError::Command(format!("{shown}: not found")),
            _ => CoreError::Command(format!("{shown}: {e}")),
        })?;
    let program = shown;
    let deadline = Instant::now() + timeout;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let mut pause = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // SAFETY: plain syscall; the child leads its own process group (process_group(0)).
                unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
                let _ = child.kill();
                let _ = child.wait();
                return Err(CoreError::Command(format!("{program}: timed out after {} s", timeout.as_secs())));
            }
            Ok(None) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(25));
            }
            Err(e) => return Err(CoreError::Command(format!("{program}: {e}"))),
        }
    };
    // A leftover grandchild may keep a pipe open: never wait past the deadline for it.
    let grace = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(100));
    Ok(Output {
        status,
        stdout: stdout.recv_timeout(grace).unwrap_or_default(),
        stderr: stderr.recv_timeout(grace).unwrap_or_default(),
    })
}

/// Reads a pipe to the end on a helper thread (so neither pipe can fill up and block the child).
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_captures_output_and_status() {
        let out = run_with_timeout("/bin/sh", &["-c", "printf 'a\\0b'; echo err >&2; exit 3"], TOOL_TIMEOUT).unwrap();
        assert_eq!(out.stdout, b"a\0b");
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn run_times_out_and_kills_the_group() {
        let started = Instant::now();
        let err = run_with_timeout("/bin/sh", &["-c", "sleep 30 & sleep 30"], Duration::from_millis(200)).unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn missing_program_is_an_error_not_a_panic() {
        let err = run_with_timeout("definitely-not-a-real-tool-cmm", &[] as &[&str], TOOL_TIMEOUT).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn bare_names_resolve_in_tool_dirs_only() {
        assert_eq!(resolve_tool("env").unwrap(), Path::new("/usr/bin/env"));
        assert!(resolve_tool("sh").is_err(), "/bin is not a tool directory");
        for bad in ["", ".", "..", "./git", "/usr/bin/git", "a/b"] {
            assert!(resolve_tool(bad).is_err(), "{bad:?} must be refused");
        }
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("cmm-fake-tool");
        std::fs::write(&fake, "#!/bin/sh\n").unwrap();
        assert!(resolve_tool_in("cmm-fake-tool", [dir.path()]).is_err(), "not executable");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_tool_in("cmm-fake-tool", [Path::new("/nonexistent"), dir.path()]).unwrap(), fake);
        // `$PATH` is never consulted.
        assert!(resolve_tool("cmm-fake-tool").is_err());
    }

    #[test]
    fn runs_bare_names_with_a_stable_environment() {
        let out = run_with_timeout("env", &[] as &[&str], TOOL_TIMEOUT).unwrap();
        let env = String::from_utf8(out.stdout).unwrap();
        for var in ["LC_ALL=C", "GIT_TERMINAL_PROMPT=0", "GIT_OPTIONAL_LOCKS=0"] {
            assert!(env.lines().any(|l| l == var), "{var} missing");
        }
    }

    #[test]
    fn from_names_matches_exactly() {
        let snapshot = ProcessSnapshot::from_names(["Claude", "Docker Desktop"]);
        assert!(snapshot.is_running("Claude"));
        assert!(!snapshot.is_running("claude"));
        assert!(!snapshot.is_running("Docker"));
    }
}
