//! Allowlisted external commands (ARCHITECTURE.md §6.4). Programs are resolved and run by the
//! shared helpers in [`crate::process`] (one timed runner, one resolver).
//!
//! On top of the program allowlist, every argv must match one exact per-detector pattern
//! (ARCHITECTURE.md §10, safeguard 6, [`check_argv`]); anything else is refused even when `argv[0]`
//! is allowlisted. A repository-wide `git worktree prune` is deliberately not a pattern (safeguard
//! 3): it can drop the admin data of a worktree on an unmounted disk, so no finding, stored report
//! or hand-edited `last-report.json` can make the executor run it.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::model::{ActionKind, DetectorKind};
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

/// The exact command shapes the detectors produce (safeguard 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pattern {
    /// `docker system prune -f`
    DockerPrune,
    /// `xcrun simctl delete unavailable`
    SimctlDeleteUnavailable,
    /// `xcrun simctl runtime delete <UUID>`
    SimctlRuntimeDelete,
    /// `git -C <abs repo> worktree remove <abs path>`
    GitWorktreeRemove,
}

impl Pattern {
    /// The detector that produces this command.
    fn detector(self) -> DetectorKind {
        match self {
            Pattern::DockerPrune => DetectorKind::Docker,
            Pattern::SimctlDeleteUnavailable | Pattern::SimctlRuntimeDelete => DetectorKind::SimulatorRuntimes,
            Pattern::GitWorktreeRemove => DetectorKind::GitWorktrees,
        }
    }

    /// The finding action that carries this command.
    fn action(self) -> ActionKind {
        match self {
            Pattern::GitWorktreeRemove => ActionKind::GitWorktreeRemove,
            _ => ActionKind::Command,
        }
    }
}

/// Which exact pattern `argv` is, if any.
pub(crate) fn classify(argv: &[String]) -> Option<Pattern> {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["docker", "system", "prune", "-f"] => Some(Pattern::DockerPrune),
        ["xcrun", "simctl", "delete", "unavailable"] => Some(Pattern::SimctlDeleteUnavailable),
        ["xcrun", "simctl", "runtime", "delete", id] if is_uuid(id) => Some(Pattern::SimctlRuntimeDelete),
        ["git", "-C", repo, "worktree", "remove", path] if is_plain_absolute(repo) && is_plain_absolute(path) => {
            Some(Pattern::GitWorktreeRemove)
        }
        _ => None,
    }
}

/// Program allowlist, then the exact pattern for `action` (and for `detector`, when the rule is
/// known). Returns the matched pattern.
pub(crate) fn check_argv(
    argv: &[String],
    action: ActionKind,
    detector: Option<DetectorKind>,
) -> Result<Pattern, String> {
    let program = argv.first().ok_or("finding has no command")?;
    check_allowed(program)?;
    let refuse = || format!("command `{}` does not match an allowed pattern for this finding", display_argv(argv));
    let pattern = classify(argv).ok_or_else(refuse)?;
    if pattern.action() != action || detector.is_some_and(|d| d != pattern.detector()) {
        return Err(refuse());
    }
    Ok(pattern)
}

/// `8-4-4-4-12` hex digits (simulator runtime identifiers).
fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, len)| g.len() == len && g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Absolute, without `.`/`..` components, no NUL, not just `/`.
fn is_plain_absolute(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute()
        && !s.contains('\0')
        && p.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && p.components().count() > 1
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

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn only_exact_per_detector_patterns_are_accepted() {
        use ActionKind::{Command, GitWorktreeRemove};
        use DetectorKind::{Docker, GitWorktrees, Paths, SimulatorRuntimes};
        let uuid = "0F5B7C2E-1D3A-4B6C-8E9F-A1B2C3D4E5F6";
        let ok: &[(&[&str], ActionKind, DetectorKind)] = &[
            (&["docker", "system", "prune", "-f"], Command, Docker),
            (&["xcrun", "simctl", "delete", "unavailable"], Command, SimulatorRuntimes),
            (&["xcrun", "simctl", "runtime", "delete", uuid], Command, SimulatorRuntimes),
            (
                &["git", "-C", "/Users/x/repo", "worktree", "remove", "/Users/x/repo/wt"],
                GitWorktreeRemove,
                GitWorktrees,
            ),
        ];
        for (a, action, detector) in ok {
            assert!(check_argv(&argv(a), *action, Some(*detector)).is_ok(), "{a:?}");
            assert!(check_argv(&argv(a), *action, None).is_ok(), "{a:?} without a rule");
            assert!(check_argv(&argv(a), *action, Some(Paths)).is_err(), "{a:?} from another detector");
        }
        let bad: &[&[&str]] = &[
            &["git", "--version"],
            &["git", "gc"],
            &["docker", "system", "prune", "-a", "-f"],
            &["docker", "system", "prune", "-f", "--volumes"],
            &["docker", "volume", "prune", "-f"],
            &["xcrun", "simctl", "erase", "all"],
            &["xcrun", "simctl", "runtime", "delete", "all"],
            &["xcrun", "simctl", "runtime", "delete", "1234"],
            // Never executable, however well-formed (safeguard 3).
            &["git", "-C", "/Users/x/repo", "worktree", "prune"],
            &["git", "-C", "relative/repo", "worktree", "prune"],
            &["git", "-C", "/Users/x/../y", "worktree", "prune"],
            &["git", "-C", "/", "worktree", "prune"],
            &["git", "-C", "/r", "worktree", "remove", "--force", "/r/wt"],
            &["git", "-C", "/r", "worktree", "remove", "wt"],
            &["git", "-C", "/r", "clean", "-fdx", "/r/wt"],
            &["rm", "-rf", "/"],
            &[],
        ];
        for a in bad {
            for action in [Command, GitWorktreeRemove] {
                assert!(check_argv(&argv(a), action, None).is_err(), "{a:?} must be refused");
            }
        }
        // The action must fit the pattern too.
        assert!(check_argv(&argv(&["docker", "system", "prune", "-f"]), GitWorktreeRemove, None).is_err());
        assert!(check_argv(&argv(&["git", "-C", "/r", "worktree", "remove", "/r/w"]), Command, None).is_err());
        assert!(check_argv(&argv(&["git", "--version"]), Command, None).unwrap_err().contains("allowed pattern"));
        assert!(check_argv(&argv(&["rm"]), Command, None).unwrap_err().contains("not an allowed program"));
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
