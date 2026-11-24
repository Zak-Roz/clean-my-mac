//! Running-process snapshot used for `requires_quit` blockers, and the bounded runner every
//! detector uses for external tools (`git`, `xcrun`, `docker`, `mdls`) (owned by S1).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

use crate::error::CoreError;

/// Upper bound for one external tool invocation (ARCHITECTURE.md §10).
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(20);

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
/// group once `timeout` has passed.
///
/// Returns the output whatever the exit status (check `output.status`). Errors
/// ([`CoreError::Command`]) only when the program cannot be started (e.g. "git: not found") or did
/// not finish in time. Never panics.
pub fn run_with_timeout<S: AsRef<OsStr>>(program: &str, args: &[S], timeout: Duration) -> Result<Output, CoreError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => CoreError::Command(format!("{program}: not found")),
            _ => CoreError::Command(format!("{program}: {e}")),
        })?;
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
    fn from_names_matches_exactly() {
        let snapshot = ProcessSnapshot::from_names(["Claude", "Docker Desktop"]);
        assert!(snapshot.is_running("Claude"));
        assert!(!snapshot.is_running("claude"));
        assert!(!snapshot.is_running("Docker"));
    }
}
