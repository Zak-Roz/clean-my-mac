//! Read-only external tool invocation for the S2 detectors (owned by S2).
//!
//! A thin wrapper over the one timed runner, [`crate::process::run_with_timeout`] (contract v2):
//! tools are resolved to absolute paths in [`crate::process::TOOL_DIRS`], stdin is closed, output
//! is captured as text, the process group is killed after [`TIMEOUT`], and a missing tool is a
//! `CoreError::Command` (never a panic). `GIT_OPTIONAL_LOCKS=0` keeps `git status` from rewriting
//! the index, so scans stay read-only.

use std::process::ExitStatus;
use std::time::Duration;

use crate::error::CoreError;
use crate::process::{TOOL_TIMEOUT, run_with_timeout};

/// Upper bound for any single external command (ARCHITECTURE.md §10).
pub const TIMEOUT: Duration = TOOL_TIMEOUT;

/// Captured result of a finished command, as (lossy UTF-8) text.
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
    std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>().join(" ")
}

/// Runs `program args…` with the default [`TIMEOUT`]. A non-zero exit is returned as `Ok` —
/// callers decide what it means; a missing tool, spawn failures and timeouts are errors.
pub fn run(program: &str, args: &[&str]) -> Result<Output, CoreError> {
    let out = run_with_timeout(program, args, TIMEOUT).map_err(|e| match e {
        CoreError::Command(m) if m.contains("timed out") => {
            CoreError::Command(format!("`{}` timed out after {} s", describe(program, args), TIMEOUT.as_secs()))
        }
        other => other,
    })?;
    Ok(Output {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
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
        let out = run("/bin/sh", &["-c", "echo hi; echo err >&2; exit 3"]).unwrap();
        assert_eq!(out.stdout, "hi\n");
        assert_eq!(out.stderr, "err\n");
        assert_eq!(out.status.code(), Some(3));
        let err = run_ok("/bin/sh", &["-c", "echo boom >&2; exit 1"]).unwrap_err().to_string();
        assert!(err.contains("boom"), "{err}");
    }

    #[test]
    fn missing_tool_is_an_error_not_a_panic() {
        let err = run("cmm-definitely-not-a-real-tool", &[]).unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn bare_names_resolve_to_tool_dirs() {
        // `git` is found in /usr/bin (or Homebrew), not through $PATH.
        let out = run("git", &["--version"]).unwrap();
        assert!(out.success() && out.stdout.starts_with("git version"), "{out:?}");
    }

    #[test]
    fn large_output_does_not_deadlock() {
        let out = run("/bin/sh", &["-c", "yes x | head -c 1000000"]).unwrap();
        assert_eq!(out.stdout.len(), 1_000_000);
    }
}
