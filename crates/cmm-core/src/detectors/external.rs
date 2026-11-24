//! Read-only external tool invocation with a hard timeout (owned by S2).
//!
//! Every detector that shells out (`git`, `xcrun`, `docker`) goes through [`run`]: stdin is closed,
//! output is captured, the child is killed after the timeout, and a missing tool is a
//! `CoreError::Command` (never a panic). `GIT_OPTIONAL_LOCKS=0` keeps `git status` from rewriting
//! the index, so scans stay read-only.

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::CoreError;

/// Upper bound for any single external command (ARCHITECTURE.md §10).
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Captured result of a finished command.
#[derive(Debug, Clone)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status.success()
    }
}

fn describe(program: &str, args: &[&str]) -> String {
    let mut text = program.to_string();
    for arg in args {
        text.push(' ');
        text.push_str(arg);
    }
    text
}

fn read_all(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// Runs `program args…` (optionally in `cwd`) and waits at most `timeout`. A non-zero exit is
/// returned as `Ok` — callers decide what it means; spawn failures and timeouts are errors.
pub fn run_in(program: &str, args: &[&str], cwd: Option<&Path>, timeout: Duration) -> Result<Output, CoreError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CoreError::Command(format!("`{program}` not found")),
        _ => CoreError::Command(format!("cannot run `{}`: {e}", describe(program, args))),
    })?;
    let stdout = child.stdout.take().map(read_all);
    let stderr = child.stderr.take().map(read_all);

    let started = Instant::now();
    let mut pause = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CoreError::Command(format!(
                    "`{}` timed out after {} s",
                    describe(program, args),
                    timeout.as_secs()
                )));
            }
            Ok(None) => {
                thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(25));
            }
            Err(e) => return Err(CoreError::Command(format!("waiting for `{}`: {e}", describe(program, args)))),
        }
    };
    let collect = |h: Option<thread::JoinHandle<Vec<u8>>>| {
        h.and_then(|h| h.join().ok()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
    };
    Ok(Output { status, stdout: collect(stdout), stderr: collect(stderr) })
}

/// [`run_in`] without a working directory, with the default [`TIMEOUT`].
pub fn run(program: &str, args: &[&str]) -> Result<Output, CoreError> {
    run_in(program, args, None, TIMEOUT)
}

/// Like [`run`], but a non-zero exit becomes `CoreError::Command` with the first stderr line.
pub fn run_ok(program: &str, args: &[&str]) -> Result<Output, CoreError> {
    let out = run(program, args)?;
    if out.success() {
        Ok(out)
    } else {
        let first = out.stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        Err(CoreError::Command(format!("`{}` failed ({}): {first}", describe(program, args), out.status)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_status() {
        let out = run("sh", &["-c", "echo hi; echo err >&2; exit 3"]).unwrap();
        assert_eq!(out.stdout, "hi\n");
        assert_eq!(out.stderr, "err\n");
        assert_eq!(out.status.code(), Some(3));
        let err = run_ok("sh", &["-c", "echo boom >&2; exit 1"]).unwrap_err().to_string();
        assert!(err.contains("boom"), "{err}");
    }

    #[test]
    fn missing_tool_is_an_error_not_a_panic() {
        let err = run("cmm-definitely-not-a-real-tool", &[]).unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn timeout_kills_the_child() {
        let started = Instant::now();
        let err = run_in("sleep", &["5"], None, Duration::from_millis(200)).unwrap_err().to_string();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn large_output_does_not_deadlock() {
        let out = run("sh", &["-c", "yes x | head -c 1000000"]).unwrap();
        assert_eq!(out.stdout.len(), 1_000_000);
    }
}
