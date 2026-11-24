//! Clean planning and execution (owned by S3). See ARCHITECTURE.md §6.
//!
//! [`plan`] is pure (report + request → what would happen). [`execute`] re-plans, then re-validates
//! every item against the live system right before acting on it: the path still exists and passes
//! the guard, the rule still matches, `requires_quit` processes are not running (fresh snapshot),
//! and a git worktree is still clean (fresh `git status`). One failing item never stops the rest.

mod command;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use command::COMMAND_ALLOWLIST;

use crate::config::{Config, home_dir};
use crate::error::CoreError;
use crate::guard::{self, canonical_entry, is_within, same_path};
use crate::history::History;
use crate::model::{ActionKind, CleanItemResult, CleanMode, CleanRequest, CleanResult, Finding, RuleDef, ScanReport};
use crate::process::ProcessSnapshot;
use crate::rules::{expand_roots, load_rules};

const GIT_STATUS_TIMEOUT: Duration = Duration::from_secs(20);
const GIT_REMOVE_TIMEOUT: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// What `execute` would do for a request. Returned to the desktop app by `plan_clean`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanPlan {
    pub items: Vec<Finding>,
    pub total_bytes: u64,
    pub warnings: Vec<String>,
}

/// Live-system dependencies of [`execute_with`]. [`execute`] uses [`ExecuteEnv::system`]; tests
/// point `home` at a tempdir and inject rules and a process snapshot.
pub struct ExecuteEnv {
    pub home: PathBuf,
    /// The current catalog, for "rule still matches" and `requires_quit`.
    pub rules: Vec<RuleDef>,
    /// Extra paths the guard must protect (the app's `data_dir`).
    pub extra_protected: Vec<PathBuf>,
    /// Captures a fresh process snapshot (called right before each item that has `requires_quit`).
    pub processes: Box<dyn Fn() -> ProcessSnapshot + Send + Sync>,
}

impl ExecuteEnv {
    /// Real home, catalog from `Config::load()`'s `data_dir`, live process list. If the catalog
    /// cannot be loaded, `rules` is empty and every item fails re-validation (fail closed).
    pub fn system() -> Self {
        let config = Config::load();
        let rules = load_rules(Some(&config.data_dir.join("rules.d"))).unwrap_or_default();
        Self {
            home: home_dir(),
            rules,
            extra_protected: vec![config.data_dir],
            processes: Box::new(ProcessSnapshot::capture),
        }
    }
}

/// Resolves `req.finding_ids` against the report. Unknown ids are an error; info-only, blocked,
/// malformed and covered-by-parent findings are excluded with a warning.
pub fn plan(report: &ScanReport, req: &CleanRequest) -> Result<CleanPlan, CoreError> {
    let sel = select(report, &req.finding_ids);
    let unknown: Vec<&str> = sel.skipped.iter().filter(|s| s.finding.is_none()).map(|s| s.id.as_str()).collect();
    if !unknown.is_empty() {
        return Err(CoreError::NotFound(format!("unknown finding id(s): {}", unknown.join(", "))));
    }

    let mut warnings: Vec<String> =
        sel.skipped.iter().map(|s| format!("Skipped {}: {}", label_of(s.finding), s.reason)).collect();
    warnings.extend(
        sel.covered
            .iter()
            .map(|(child, parent)| format!("Skipped {}: inside {}, removed with it", child.label, parent.label)),
    );

    let bytes_where = |pred: &dyn Fn(&Finding) -> bool| -> (usize, u64) {
        sel.act.iter().filter(|f| pred(f)).fold((0, 0), |(n, b), f| (n + 1, b + f.bytes))
    };
    let (n_paths, path_bytes) = bytes_where(&|f| matches!(f.action, ActionKind::Trash | ActionKind::Delete));
    if n_paths > 0 {
        warnings.push(match req.mode {
            CleanMode::Trash => format!(
                "Moving to Trash frees no disk space until the Trash is emptied ({} in {n_paths} item(s)).",
                human(path_bytes)
            ),
            CleanMode::Delete => {
                format!("Permanent delete: {n_paths} item(s), {} — cannot be recovered.", human(path_bytes))
            }
        });
    }
    let (n_wt, wt_bytes) = bytes_where(&|f| f.action == ActionKind::GitWorktreeRemove);
    if n_wt > 0 {
        warnings.push(format!(
            "{n_wt} git worktree(s) ({}) are removed with `git worktree remove`: permanent, not via Trash. \
             Committed work stays in the repository; ignored files (.env, node_modules, …) are deleted.",
            human(wt_bytes)
        ));
    }
    for f in sel.act.iter().filter(|f| f.action == ActionKind::Command) {
        warnings.push(format!(
            "{}: runs `{}`",
            f.label,
            command::display_argv(f.command.as_deref().unwrap_or_default())
        ));
    }

    let items: Vec<Finding> = sel.act.iter().map(|f| (*f).clone()).collect();
    Ok(CleanPlan { total_bytes: items.iter().map(|f| f.bytes).sum(), items, warnings })
}

/// Plans, re-validates and executes `req` against the real system ([`ExecuteEnv::system`]).
pub fn execute(
    report: &ScanReport,
    req: &CleanRequest,
    history: Option<&History>,
    on_item: &dyn Fn(&CleanItemResult),
) -> CleanResult {
    execute_with(&ExecuteEnv::system(), report, req, history, on_item)
}

/// [`execute`] with explicit live-system dependencies.
///
/// Result items: first the excluded ids (`ok: false`, "skipped: …"), then the acted-on items in plan
/// order, then covered children (`ok` mirrors their parent, `bytes: 0`). When not a dry run, every
/// acted-on item (including re-validation failures) is written to the history audit log.
/// `dry_run` performs every read-only check but changes nothing (no history write either).
pub fn execute_with(
    env: &ExecuteEnv,
    report: &ScanReport,
    req: &CleanRequest,
    history: Option<&History>,
    on_item: &dyn Fn(&CleanItemResult),
) -> CleanResult {
    let sel = select(report, &req.finding_ids);
    let mut protected = env.extra_protected.clone();
    if let Some(dir) = history.and_then(|h| h.path().parent()).filter(|d| !d.as_os_str().is_empty()) {
        protected.push(dir.to_path_buf());
    }

    let mut result =
        CleanResult { dry_run: req.dry_run, mode: req.mode, items: vec![], freed_bytes: 0, moved_to_trash_bytes: 0 };
    let emit = |item: CleanItemResult, result: &mut CleanResult| {
        on_item(&item);
        result.items.push(item);
    };

    for s in &sel.skipped {
        let item = CleanItemResult {
            finding_id: s.id.clone(),
            ok: false,
            bytes: 0,
            message: Some(format!("skipped: {}", s.reason)),
        };
        emit(item, &mut result);
    }

    // Items acted on so far and whether they succeeded (a covered child mirrors a successful
    // ancestor; if every ancestor failed, the child is tried on its own).
    let mut acted: Vec<(&Finding, bool)> = Vec::new();
    let act_one = |f: &Finding, result: &mut CleanResult| -> CleanItemResult {
        let mut item = match act(env, f, req, &protected) {
            Ok(done) => {
                match done.op {
                    Op::Trash => result.moved_to_trash_bytes += f.bytes,
                    Op::Delete | Op::GitWorktreeRemove | Op::Command => result.freed_bytes += f.bytes,
                }
                CleanItemResult { finding_id: f.id.clone(), ok: true, bytes: f.bytes, message: Some(done.message) }
            }
            Err(why) => CleanItemResult { finding_id: f.id.clone(), ok: false, bytes: 0, message: Some(why) },
        };
        if !req.dry_run
            && let Some(h) = history
            && let Err(e) = h.record_action(f, req.mode, &item)
        {
            let msg = item.message.take().unwrap_or_default();
            item.message = Some(format!("{msg} (audit log write failed: {e})"));
        }
        item
    };

    for f in &sel.act {
        let item = act_one(f, &mut result);
        acted.push((f, item.ok));
        emit(item, &mut result);
    }

    // `covered` is ordered shallowest first, so a retried child is settled before its own children.
    for (child, _) in &sel.covered {
        let child_path = Path::new(child.path.as_deref().unwrap_or_default());
        let done_by =
            acted.iter().find(|(a, ok)| *ok && a.path.as_deref().is_some_and(|p| is_within(child_path, Path::new(p))));
        let item = match done_by {
            Some((ancestor, _)) => CleanItemResult {
                finding_id: child.id.clone(),
                ok: true,
                bytes: 0,
                message: Some(format!("included in {}", ancestor.label)),
            },
            None => {
                let item = act_one(child, &mut result);
                acted.push((child, item.ok));
                item
            }
        };
        emit(item, &mut result);
    }
    result
}

// ---------------------------------------------------------------------------------------------
// Selection (shared by plan and execute)

struct Skip<'a> {
    id: String,
    finding: Option<&'a Finding>,
    reason: String,
}

struct Selection<'a> {
    /// Findings to act on, in report order.
    act: Vec<&'a Finding>,
    /// (child, parent): child's path is inside an acted-on parent's path.
    covered: Vec<(&'a Finding, &'a Finding)>,
    /// In request order. `finding: None` = unknown id.
    skipped: Vec<Skip<'a>>,
}

fn label_of(f: Option<&Finding>) -> &str {
    f.map_or("?", |f| f.label.as_str())
}

/// Why a finding cannot be acted on at all, judged from the report alone.
fn exclusion(f: &Finding) -> Option<String> {
    if f.action == ActionKind::InfoOnly {
        return Some("information only, nothing to clean".into());
    }
    if !f.blockers.is_empty() {
        return Some(format!("blocked: {}", f.blockers.join("; ")));
    }
    match f.action {
        ActionKind::Trash | ActionKind::Delete | ActionKind::GitWorktreeRemove if f.path.is_none() => {
            return Some("finding has no path".into());
        }
        ActionKind::Command | ActionKind::GitWorktreeRemove => match f.command.as_deref() {
            None | Some([]) => return Some("finding has no command".into()),
            Some([program, ..]) => {
                if let Err(e) = command::check_allowed(program) {
                    return Some(e);
                }
            }
        },
        _ => {}
    }
    None
}

fn select<'a>(report: &'a ScanReport, ids: &[String]) -> Selection<'a> {
    let index: HashMap<&str, (usize, &Finding)> =
        report.findings.iter().enumerate().map(|(i, f)| (f.id.as_str(), (i, f))).collect();
    let mut seen = HashSet::new();
    let mut skipped = Vec::new();
    let mut candidates: Vec<(usize, &Finding)> = Vec::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        match index.get(id.as_str()) {
            None => skipped.push(Skip { id: id.clone(), finding: None, reason: "unknown finding id".into() }),
            Some(&(i, f)) => match exclusion(f) {
                Some(reason) => skipped.push(Skip { id: id.clone(), finding: Some(f), reason }),
                None => candidates.push((i, f)),
            },
        }
    }
    candidates.sort_by_key(|(i, _)| *i);

    // Shallowest paths first, so a parent is accepted before anything inside it (equal paths:
    // the earlier report entry wins).
    let depth = |f: &Finding| f.path.as_deref().map_or(0, |p| Path::new(p).components().count());
    let mut by_depth: Vec<(usize, &Finding)> = candidates.clone();
    by_depth.sort_by_key(|(i, f)| (depth(f), *i));
    let mut accepted: Vec<(usize, &Finding)> = Vec::new();
    let mut covered = Vec::new();
    for (i, f) in by_depth {
        let parent = f.path.as_deref().and_then(|p| {
            accepted.iter().find(|(_, a)| a.path.as_deref().is_some_and(|q| is_within(Path::new(p), Path::new(q))))
        });
        match parent {
            Some((_, parent)) => covered.push((f, *parent)),
            None => accepted.push((i, f)),
        }
    }
    accepted.sort_by_key(|(i, _)| *i);
    Selection { act: accepted.into_iter().map(|(_, f)| f).collect(), covered, skipped }
}

// ---------------------------------------------------------------------------------------------
// Execution of one item

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Trash,
    Delete,
    GitWorktreeRemove,
    Command,
}

struct Done {
    op: Op,
    message: String,
}

/// `Trash`/`Delete` findings follow the clean mode (Trash mode never deletes permanently).
fn op_for(action: ActionKind, mode: CleanMode) -> Option<Op> {
    match (action, mode) {
        (ActionKind::Trash | ActionKind::Delete, CleanMode::Trash) => Some(Op::Trash),
        (ActionKind::Trash | ActionKind::Delete, CleanMode::Delete) => Some(Op::Delete),
        (ActionKind::GitWorktreeRemove, _) => Some(Op::GitWorktreeRemove),
        (ActionKind::Command, _) => Some(Op::Command),
        (ActionKind::InfoOnly, _) => None,
    }
}

fn act(env: &ExecuteEnv, f: &Finding, req: &CleanRequest, protected: &[PathBuf]) -> Result<Done, String> {
    let op = op_for(f.action, req.mode).ok_or("information only, nothing to clean")?;

    // Path still exists and passes the guard (Command findings: the tool does the removal).
    let target = match op {
        Op::Trash | Op::Delete | Op::GitWorktreeRemove => {
            let path = f.path.as_deref().ok_or("finding has no path")?;
            Some(guard::check_target_with(Path::new(path), &env.home, protected).map_err(|e| match e {
                CoreError::NotFound(_) => "no longer exists".to_string(),
                other => other.to_string(),
            })?)
        }
        Op::Command => None,
    };

    // Rule still matches.
    let rule = env
        .rules
        .iter()
        .find(|r| r.id == f.rule_id)
        .ok_or_else(|| format!("rule {} is no longer in the catalog", f.rule_id))?;
    if !rule.enabled {
        return Err(format!("rule {} is disabled", rule.id));
    }
    if let (Op::Trash | Op::Delete, Some(target)) = (op, &target) {
        check_rule_roots(rule, target, &env.home)?;
    }

    // Processes that must not run (fresh snapshot).
    if !rule.requires_quit.is_empty() {
        let snapshot = (env.processes)();
        let running: Vec<&str> =
            rule.requires_quit.iter().filter(|name| snapshot.is_running(name)).map(String::as_str).collect();
        if !running.is_empty() {
            return Err(format!("{} is running — quit it first", running.join(", ")));
        }
    }

    let dry = req.dry_run;
    match (op, target) {
        (Op::Trash, Some(target)) => {
            if dry {
                return Ok(Done { op, message: "dry run: would move to Trash".into() });
            }
            move_to_trash(&target)?;
            Ok(Done { op, message: "moved to Trash".into() })
        }
        (Op::Delete, Some(target)) => {
            if dry {
                return Ok(Done { op, message: "dry run: would delete permanently".into() });
            }
            delete_permanently(&target).map_err(|e| format!("delete failed: {e}"))?;
            Ok(Done { op, message: "deleted permanently".into() })
        }
        (Op::GitWorktreeRemove, Some(target)) => {
            let argv = f.command.as_deref().unwrap_or_default();
            check_worktree_argv(argv, &target)?;
            let git = command::resolve_allowed(&argv[0])?;
            ensure_worktree_clean(&git, &target)?;
            if dry {
                return Ok(Done { op, message: format!("dry run: would run `{}`", command::display_argv(argv)) });
            }
            run_checked(&git, argv, GIT_REMOVE_TIMEOUT)?;
            Ok(Done { op, message: "removed git worktree".into() })
        }
        (Op::Command, _) => {
            let argv = f.command.as_deref().unwrap_or_default();
            let program = argv.first().ok_or("finding has no command")?;
            let program = command::resolve_allowed(program)?;
            if dry {
                return Ok(Done { op, message: format!("dry run: would run `{}`", command::display_argv(argv)) });
            }
            let out = run_checked(&program, argv, COMMAND_TIMEOUT)?;
            let mut message = format!("ran `{}`", command::display_argv(argv));
            if !out.stderr.trim().is_empty() {
                message.push_str(&format!(" (stderr: {})", command::tail(&out.stderr, 300)));
            }
            Ok(Done { op, message })
        }
        (_, None) => Err("finding has no path".into()),
    }
}

/// The target must still be one of the rule's (expanded, existing) roots or inside one.
fn check_rule_roots(rule: &RuleDef, target: &Path, home: &Path) -> Result<(), String> {
    let roots = expand_roots(rule, home);
    let inside = roots.iter().any(|root| {
        let root = canonical_entry(root).unwrap_or_else(|| root.clone());
        is_within(target, &root)
    });
    if inside { Ok(()) } else { Err(format!("no longer matches rule {} (outside its roots)", rule.id)) }
}

/// `["git", "-C", <repo>, "worktree", "remove", <worktree path>]` and the path is the finding's.
fn check_worktree_argv(argv: &[String], target: &Path) -> Result<(), String> {
    match argv {
        [git, c, repo, wt, rm, path]
            if git == "git" && c == "-C" && wt == "worktree" && rm == "remove" && Path::new(repo).is_absolute() =>
        {
            let resolved = canonical_entry(Path::new(path)).ok_or_else(|| format!("cannot resolve {path}"))?;
            if same_path(&resolved, target) {
                Ok(())
            } else {
                Err(format!("worktree command targets {path}, not the finding's path"))
            }
        }
        _ => Err(format!("unexpected worktree command `{}`", command::display_argv(argv))),
    }
}

/// Fresh `git status --porcelain` (without taking the index lock): anything listed means work
/// that `git worktree remove` would lose or refuse.
fn ensure_worktree_clean(git: &Path, worktree: &Path) -> Result<(), String> {
    let args: Vec<String> = vec![
        "--no-optional-locks".into(),
        "-C".into(),
        worktree.to_string_lossy().into_owned(),
        "status".into(),
        "--porcelain".into(),
    ];
    let out = command::run_with_timeout(git, &args, GIT_STATUS_TIMEOUT).map_err(|e| format!("git status: {e}"))?;
    if !out.success {
        return Err(format!("git status failed: {}", command::tail(&out.stderr, 300)));
    }
    let changed = out.stdout.lines().filter(|l| !l.trim().is_empty()).count();
    if changed > 0 {
        return Err(format!("worktree has uncommitted changes ({changed} path(s))"));
    }
    Ok(())
}

fn run_checked(program: &Path, argv: &[String], timeout: Duration) -> Result<command::Output, String> {
    let shown = command::display_argv(argv);
    let out = command::run_with_timeout(program, &argv[1..], timeout).map_err(|e| format!("`{shown}`: {e}"))?;
    if out.success {
        Ok(out)
    } else {
        let code = out.code.map_or_else(|| "signal".to_string(), |c| c.to_string());
        Err(format!("`{shown}` failed (exit {code}): {}", command::tail(&out.stderr, 500)))
    }
}

/// NSFileManager `trashItemAtURL` (the `trash` crate's default on macOS is a Finder AppleScript,
/// which needs Automation permission). The final symlink, if any, is trashed as a link.
fn move_to_trash(target: &Path) -> Result<(), String> {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx.delete(target).map_err(|e| format!("move to Trash failed: {e}"))
}

/// Never follows symlinks: a link is unlinked; `remove_dir_all` does not traverse links inside.
fn delete_permanently(target: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(target)?.is_dir() {
        std::fs::remove_dir_all(target)
    } else {
        std::fs::remove_file(target)
    }
}

/// Decimal units, like Finder.
fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests;
