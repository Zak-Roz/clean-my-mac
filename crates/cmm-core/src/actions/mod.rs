//! Clean planning and execution (owned by S3). See ARCHITECTURE.md §6 and §10 ("Safety fixes
//! after the delete-path review").
//!
//! **Never act on a stored report.** [`execute_fresh`] re-runs the scan for the rules of the
//! requested findings and acts only on findings that are still there with the same id, action,
//! path and command argv, and without blockers; everything else is `Skipped` ("no longer matches
//! the scan"). [`plan_fresh`] does the same for the confirmation step. The stored report (the
//! app's `last-report.json`, the CLI's first scan) only says which findings the user picked.
//!
//! The executor then re-validates every item right before acting on it: the path still exists and
//! passes the guard (no `.git` entry inside, except a worktree's own pointer file), the rule still
//! matches, `requires_quit` processes are not running (fresh snapshot), a command matches its exact
//! per-detector pattern, and a git worktree is still clean, unlocked, has no commits missing from
//! the remotes and no submodules. One failing item never stops the rest. A real (non-dry-run)
//! clean requires the history database: every acted-on item goes to its audit log.
//!
//! Git worktrees follow the clean mode: `Trash` moves the worktree folder **and** its own admin
//! dir (`<repo>/.git/worktrees/<id>`) to the Trash, so restoring both restores the worktree; there
//! is no repository-wide `git worktree prune` (not even as a finding of its own: stale entries are
//! information only). `Delete` runs the finding's `git -C <repo> worktree remove <path>` (never
//! `--force`). The branch stays in the repository.

mod command;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use command::COMMAND_ALLOWLIST;

use crate::config::Config;
use crate::error::CoreError;
use crate::guard::{self, canonical_entry, is_within, same_path};
use crate::history::History;
use crate::model::{
    ActionKind, CleanItemResult, CleanMode, CleanOutcome, CleanRequest, CleanResult, DetectorKind, Finding, RuleDef,
    ScanOptions, ScanReport,
};
use crate::process::ProcessSnapshot;
use crate::rules::expand_roots;
use crate::scan::scan_with;

const GIT_STATUS_TIMEOUT: Duration = Duration::from_secs(20);
const GIT_REMOVE_TIMEOUT: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// Prefix of the reason for findings that a fresh scan no longer confirms.
pub const NO_LONGER_MATCHES: &str = "no longer matches the scan";
/// Item message when a real clean is refused because there is no audit log.
pub const NO_AUDIT_LOG: &str = "refusing to clean without the audit log: the history database is not open";

/// What `execute` would do for a request. Returned to the desktop app by `plan_clean`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanPlan {
    pub items: Vec<Finding>,
    pub total_bytes: u64,
    pub warnings: Vec<String>,
}

/// Live-system dependencies of the executor. [`execute_fresh`] / [`plan_fresh`] use
/// [`ExecuteEnv::live`]; tests point `home` at a tempdir and inject rules, a process snapshot and
/// a fake Trash.
pub struct ExecuteEnv {
    pub home: PathBuf,
    /// The current catalog: what the fresh scan runs, "rule still matches" and `requires_quit`.
    pub rules: Vec<RuleDef>,
    /// Extra paths the guard must protect (the app's `data_dir`).
    pub extra_protected: Vec<PathBuf>,
    /// Captures a fresh process snapshot (for the fresh scan and right before each item that has
    /// `requires_quit`).
    pub processes: Box<dyn Fn() -> ProcessSnapshot + Send + Sync>,
    /// Moves a guard-checked path to the Trash ([`move_to_trash`] in [`ExecuteEnv::live`]; tests
    /// inject a fake so they never touch the real `~/.Trash`).
    pub trash: Box<TrashFn>,
}

/// Signature of [`ExecuteEnv::trash`]: move one path to the Trash, or say why not.
pub type TrashFn = dyn Fn(&Path) -> Result<(), String> + Send + Sync;

impl ExecuteEnv {
    /// The real system: `rules` as given, `config.data_dir` protected, live process list, the
    /// real Trash (NSFileManager).
    pub fn live(rules: &[RuleDef], config: &Config, home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            rules: rules.to_vec(),
            extra_protected: vec![config.data_dir.clone()],
            processes: Box::new(ProcessSnapshot::capture),
            trash: Box::new(move_to_trash),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Fresh scan: never act on a stored report (safety fix 1)

/// Plans `req` against a **fresh** scan of the rules behind the requested findings (see the module
/// docs). `selected_from` is the report the user picked the ids from; an id that is not in it is
/// an error. Findings that no longer match are left out of `items` with a "Skipped …: no longer
/// matches the scan (…)" warning.
pub fn plan_fresh(
    rules: &[RuleDef],
    selected_from: &ScanReport,
    req: &CleanRequest,
    config: &Config,
    home: &Path,
) -> Result<CleanPlan, CoreError> {
    plan_fresh_with(&ExecuteEnv::live(rules, config, home), config, selected_from, req)
}

/// [`plan_fresh`] with explicit live-system dependencies.
pub fn plan_fresh_with(
    env: &ExecuteEnv,
    config: &Config,
    selected_from: &ScanReport,
    req: &CleanRequest,
) -> Result<CleanPlan, CoreError> {
    let known: HashSet<&str> = selected_from.findings.iter().map(|f| f.id.as_str()).collect();
    let unknown: Vec<&str> = req.finding_ids.iter().map(String::as_str).filter(|id| !known.contains(id)).collect();
    if !unknown.is_empty() {
        return Err(CoreError::NotFound(format!("unknown finding id(s): {}", unknown.join(", "))));
    }
    let fresh = rescan(env, config, selected_from, &req.finding_ids);
    let current = CleanRequest { finding_ids: fresh.current.clone(), mode: req.mode, dry_run: req.dry_run };
    let mut plan = plan_with(&fresh.report, &current, Some(&env.rules))?;
    let mut warnings: Vec<String> = fresh
        .stale
        .iter()
        .map(|s| format!("Skipped {}: {NO_LONGER_MATCHES} ({})", label_of(s.stored.as_ref()), s.why))
        .collect();
    warnings.append(&mut plan.warnings);
    plan.warnings = warnings;
    Ok(plan)
}

/// Re-scans, re-validates and executes `req` against the real system (safety fix 1): only findings
/// that a fresh scan still reports with the same id, action, path and argv, and without blockers,
/// are acted on; the rest are `Skipped` ("no longer matches the scan (…)"). `selected_from` is the
/// report the user picked the ids from (for display and selection only).
///
/// A real clean (`dry_run == false`) without `history` is refused: every item is `Skipped` with
/// [`NO_AUDIT_LOG`] and nothing is scanned or changed (safety fix 5).
pub fn execute_fresh(
    rules: &[RuleDef],
    selected_from: &ScanReport,
    req: &CleanRequest,
    config: &Config,
    home: &Path,
    history: Option<&History>,
    on_item: &dyn Fn(&CleanItemResult),
) -> CleanResult {
    execute_fresh_with(&ExecuteEnv::live(rules, config, home), config, selected_from, req, history, on_item)
}

/// [`execute_fresh`] with explicit live-system dependencies.
///
/// Result items: first the findings the fresh scan no longer confirms (`Skipped`), then what the
/// executor reports for the rest (see [`execute_with`]).
pub fn execute_fresh_with(
    env: &ExecuteEnv,
    config: &Config,
    selected_from: &ScanReport,
    req: &CleanRequest,
    history: Option<&History>,
    on_item: &dyn Fn(&CleanItemResult),
) -> CleanResult {
    let mut result =
        CleanResult { dry_run: req.dry_run, mode: req.mode, items: vec![], freed_bytes: 0, moved_to_trash_bytes: 0 };
    let skip = |id: &str, message: String, result: &mut CleanResult| {
        let item = CleanItemResult {
            finding_id: id.to_string(),
            outcome: CleanOutcome::Skipped,
            bytes: 0,
            message: Some(message),
        };
        on_item(&item);
        result.items.push(item);
    };

    if !req.dry_run && history.is_none() {
        for id in dedup(&req.finding_ids) {
            skip(id, format!("skipped: {NO_AUDIT_LOG}"), &mut result);
        }
        return result;
    }

    let fresh = rescan(env, config, selected_from, &req.finding_ids);
    for s in &fresh.stale {
        let reason = match s.stored {
            Some(_) => format!("{NO_LONGER_MATCHES} ({})", s.why),
            None => s.why.clone(),
        };
        skip(&s.id, format!("skipped: {reason}"), &mut result);
    }
    let current = CleanRequest { finding_ids: fresh.current, mode: req.mode, dry_run: req.dry_run };
    let done = execute_with(env, &fresh.report, &current, history, on_item);
    result.items.extend(done.items);
    result.freed_bytes += done.freed_bytes;
    result.moved_to_trash_bytes += done.moved_to_trash_bytes;
    result
}

/// A requested id the fresh scan does not confirm.
struct Stale {
    id: String,
    /// The finding as the user saw it (`None`: not in the stored report).
    stored: Option<Finding>,
    why: String,
}

struct Rescan {
    /// The fresh scan of the requested findings' rules.
    report: ScanReport,
    /// Requested ids confirmed by the fresh scan, in request order (deduplicated).
    current: Vec<String>,
    stale: Vec<Stale>,
}

fn dedup(ids: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    ids.iter().map(String::as_str).filter(|id| seen.insert(*id)).collect()
}

/// Scans the rules behind `ids` (as found in `selected_from`) and splits `ids` into confirmed and
/// stale ones.
fn rescan(env: &ExecuteEnv, config: &Config, selected_from: &ScanReport, ids: &[String]) -> Rescan {
    let stored: HashMap<&str, &Finding> = selected_from.findings.iter().map(|f| (f.id.as_str(), f)).collect();
    let ids = dedup(ids);
    let rule_ids: BTreeSet<String> = ids.iter().filter_map(|id| stored.get(id)).map(|f| f.rule_id.clone()).collect();
    let opts = ScanOptions { rule_ids: Some(rule_ids.into_iter().collect()), categories: None };
    let report = scan_with(&env.home, &env.rules, &opts, config, || (env.processes)(), &|_| {});

    let fresh: HashMap<&str, &Finding> = report.findings.iter().map(|f| (f.id.as_str(), f)).collect();
    let mut current = Vec::new();
    let mut stale = Vec::new();
    for id in ids {
        let Some(old) = stored.get(id) else {
            stale.push(Stale { id: id.to_string(), stored: None, why: "unknown finding id".into() });
            continue;
        };
        let why = match fresh.get(id) {
            None => {
                let failed = report.errors.iter().find(|e| e.path == old.rule_id);
                match failed {
                    Some(e) => format!("the new scan of rule {} failed: {}", old.rule_id, e.message),
                    None => "a new scan no longer finds it".to_string(),
                }
            }
            Some(new) => match mismatch(old, new) {
                None => {
                    current.push(id.to_string());
                    continue;
                }
                Some(why) => why,
            },
        };
        stale.push(Stale { id: id.to_string(), stored: Some((*old).clone()), why });
    }
    Rescan { report, current, stale }
}

/// Why a freshly scanned finding is not the one the user selected, if it is not.
fn mismatch(old: &Finding, new: &Finding) -> Option<String> {
    if new.action != old.action {
        return Some(format!("its action changed from {} to {}", action_name(old.action), action_name(new.action)));
    }
    if new.path != old.path {
        return Some("its path changed".into());
    }
    if new.command != old.command {
        return Some(format!(
            "its command changed to `{}`",
            command::display_argv(new.command.as_deref().unwrap_or_default())
        ));
    }
    if !new.blockers.is_empty() {
        return Some(format!("now blocked: {}", new.blockers.join("; ")));
    }
    None
}

fn action_name(action: ActionKind) -> &'static str {
    match action {
        ActionKind::Trash => "trash",
        ActionKind::Delete => "delete",
        ActionKind::GitWorktreeRemove => "git-worktree-remove",
        ActionKind::Command => "command",
        ActionKind::InfoOnly => "info-only",
    }
}

// ---------------------------------------------------------------------------------------------
// Plan and execute against one (fresh) report

/// Resolves `req.finding_ids` against `report` (pure: no rescan; [`plan_fresh`] is what the app
/// uses). Unknown ids are an error; info-only, blocked, malformed (including commands that do not
/// match their exact pattern) and covered-by-parent findings are excluded with a warning.
pub fn plan(report: &ScanReport, req: &CleanRequest) -> Result<CleanPlan, CoreError> {
    plan_with(report, req, None)
}

/// [`plan`]; with `rules`, commands are also checked against their rule's detector.
fn plan_with(report: &ScanReport, req: &CleanRequest, rules: Option<&[RuleDef]>) -> Result<CleanPlan, CoreError> {
    let sel = select(report, &req.finding_ids, rules);
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
    let in_trash_mode = req.mode == CleanMode::Trash;
    let (n_paths, path_bytes) = bytes_where(&|f| {
        matches!(f.action, ActionKind::Trash | ActionKind::Delete)
            || (in_trash_mode && f.action == ActionKind::GitWorktreeRemove)
    });
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
        warnings.push(match req.mode {
            CleanMode::Trash => format!(
                "{n_wt} git worktree folder(s) ({}) go to the Trash together with their git admin data \
                 (.git/worktrees/…); put both back to restore a worktree. The branches stay in the repository.",
                human(wt_bytes)
            ),
            CleanMode::Delete => format!(
                "{n_wt} git worktree(s) ({}) are removed with `git worktree remove`: permanent. \
                 Committed work stays in the repository; ignored files (.env, node_modules, …) are deleted.",
                human(wt_bytes)
            ),
        });
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

/// Re-validates and executes `req` against `report`, which must be a **fresh** scan (only
/// [`execute_fresh_with`] calls it; never pass a stored report).
///
/// Result items: first the excluded ids (`Skipped`, "skipped: …"), then the acted-on items in plan
/// order (`Done`, `Skipped` or `Failed`), then covered children (`Done` with `bytes: 0` when an
/// ancestor was done, otherwise tried on their own). When not a dry run, every acted-on item
/// (including re-validation refusals) is written to the history audit log; a failed write keeps the
/// outcome and adds "audit log write failed" to the message. `dry_run` performs every read-only
/// check but changes nothing (no history write either).
pub(crate) fn execute_with(
    env: &ExecuteEnv,
    report: &ScanReport,
    req: &CleanRequest,
    history: Option<&History>,
    on_item: &dyn Fn(&CleanItemResult),
) -> CleanResult {
    let sel = select(report, &req.finding_ids, Some(&env.rules));
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
            outcome: CleanOutcome::Skipped,
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
                    Op::Trash | Op::WorktreeTrash => result.moved_to_trash_bytes += f.bytes,
                    Op::Delete | Op::GitWorktreeRemove | Op::Command => result.freed_bytes += f.bytes,
                }
                CleanItemResult {
                    finding_id: f.id.clone(),
                    outcome: CleanOutcome::Done,
                    bytes: f.bytes,
                    message: Some(done.message),
                }
            }
            Err(stop) => {
                let (outcome, why) = match stop {
                    Stop::Skip(why) => (CleanOutcome::Skipped, format!("skipped: {why}")),
                    Stop::Fail(why) => (CleanOutcome::Failed, why),
                };
                CleanItemResult { finding_id: f.id.clone(), outcome, bytes: 0, message: Some(why) }
            }
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
        acted.push((f, item.outcome == CleanOutcome::Done));
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
                outcome: CleanOutcome::Done,
                bytes: 0,
                message: Some(format!("included in {}", ancestor.label)),
            },
            None => {
                let item = act_one(child, &mut result);
                acted.push((child, item.outcome == CleanOutcome::Done));
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

/// Why a finding cannot be acted on at all, judged from the report (and the rule's detector when
/// `rules` is given).
fn exclusion(f: &Finding, rules: Option<&[RuleDef]>) -> Option<String> {
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
            Some(argv) => {
                let detector = rules.and_then(|rules| detector_of(rules, &f.rule_id));
                if let Err(e) = command::check_argv(argv, f.action, detector) {
                    return Some(e);
                }
            }
        },
        _ => {}
    }
    None
}

fn detector_of(rules: &[RuleDef], rule_id: &str) -> Option<DetectorKind> {
    rules.iter().find(|r| r.id == rule_id).map(|r| r.detector)
}

fn select<'a>(report: &'a ScanReport, ids: &[String], rules: Option<&[RuleDef]>) -> Selection<'a> {
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
            Some(&(i, f)) => match exclusion(f, rules) {
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
    /// Worktree folder and its admin dir to the Trash.
    WorktreeTrash,
    /// The finding's `git … worktree remove <path>` argv.
    GitWorktreeRemove,
    Command,
}

struct Done {
    op: Op,
    message: String,
}

/// Why an item was not acted on: `Skip` for a worktree that is no longer safe to remove (safety
/// fix 3), `Fail` for everything else (guard, re-validation, tool errors).
enum Stop {
    Skip(String),
    Fail(String),
}

impl From<String> for Stop {
    fn from(why: String) -> Self {
        Stop::Fail(why)
    }
}

impl From<&str> for Stop {
    fn from(why: &str) -> Self {
        Stop::Fail(why.to_string())
    }
}

/// `Trash`/`Delete` and worktree findings follow the clean mode (Trash mode never deletes
/// permanently); `Command` findings are what they are.
fn op_for(action: ActionKind, mode: CleanMode) -> Option<Op> {
    match (action, mode) {
        (ActionKind::Trash | ActionKind::Delete, CleanMode::Trash) => Some(Op::Trash),
        (ActionKind::Trash | ActionKind::Delete, CleanMode::Delete) => Some(Op::Delete),
        (ActionKind::GitWorktreeRemove, CleanMode::Trash) => Some(Op::WorktreeTrash),
        (ActionKind::GitWorktreeRemove, CleanMode::Delete) => Some(Op::GitWorktreeRemove),
        (ActionKind::Command, _) => Some(Op::Command),
        (ActionKind::InfoOnly, _) => None,
    }
}

fn guard_err(e: CoreError) -> String {
    match e {
        CoreError::NotFound(_) => "no longer exists".to_string(),
        other => other.to_string(),
    }
}

fn act(env: &ExecuteEnv, f: &Finding, req: &CleanRequest, protected: &[PathBuf]) -> Result<Done, Stop> {
    let op = op_for(f.action, req.mode).ok_or("information only, nothing to clean")?;
    let path = || f.path.as_deref().map(Path::new).ok_or("finding has no path");

    // The rule must still be in the catalog and enabled.
    let rule = env
        .rules
        .iter()
        .find(|r| r.id == f.rule_id)
        .ok_or_else(|| format!("rule {} is no longer in the catalog", f.rule_id))?;

    match op {
        Op::Trash | Op::Delete => {
            // Path still exists, passes the guard (no `.git` entry at all) and is under the rule's roots.
            let target = guard::check_target_with(path()?, &env.home, protected).map_err(guard_err)?;
            ensure_enabled(rule)?;
            check_rule_roots(rule, &target, &env.home)?;
            ensure_not_running(env, rule)?;
            if op == Op::Trash {
                if req.dry_run {
                    return Ok(Done { op, message: "dry run: would move to Trash".into() });
                }
                (env.trash)(&target)?;
                Ok(Done { op, message: "moved to Trash".into() })
            } else {
                if req.dry_run {
                    return Ok(Done { op, message: "dry run: would delete permanently".into() });
                }
                delete_permanently(&target).map_err(|e| format!("delete failed: {e}"))?;
                Ok(Done { op, message: "deleted permanently".into() })
            }
        }
        Op::WorktreeTrash | Op::GitWorktreeRemove => {
            ensure_enabled(rule)?;
            act_worktree(env, f, rule, op, req.dry_run, protected)
        }
        Op::Command => {
            ensure_enabled(rule)?;
            let argv = f.command.as_deref().unwrap_or_default();
            command::check_argv(argv, ActionKind::Command, Some(rule.detector))?;
            ensure_not_running(env, rule)?;
            let program = command::resolve_allowed(&argv[0])?;
            if req.dry_run {
                return Ok(Done { op, message: format!("dry run: would run `{}`", command::display_argv(argv)) });
            }
            let out = run_checked(&program, argv, COMMAND_TIMEOUT)?;
            let mut message = format!("ran `{}`", command::display_argv(argv));
            if !out.stderr.trim().is_empty() {
                message.push_str(&format!(" (stderr: {})", command::tail(&out.stderr, 300)));
            }
            Ok(Done { op, message })
        }
    }
}

fn ensure_enabled(rule: &RuleDef) -> Result<(), String> {
    if rule.enabled { Ok(()) } else { Err(format!("rule {} is disabled", rule.id)) }
}

/// `requires_quit` processes must not be running (fresh snapshot).
fn ensure_not_running(env: &ExecuteEnv, rule: &RuleDef) -> Result<(), String> {
    if rule.requires_quit.is_empty() {
        return Ok(());
    }
    let snapshot = (env.processes)();
    let running: Vec<&str> =
        rule.requires_quit.iter().filter(|name| snapshot.is_running(name)).map(String::as_str).collect();
    if running.is_empty() { Ok(()) } else { Err(format!("{} is running — quit it first", running.join(", "))) }
}

/// A linked worktree, re-checked right before acting (safety fix 3):
/// 1. the argv is exactly `git -C <repo> worktree remove <the finding's path>`;
/// 2. the folder has a `.git` **file** whose admin dir is `<repo>/.git/worktrees/<id>` and points back;
/// 3. not locked, no `modules/` in the admin dir, clean `git status` (all untracked files, all
///    submodules, whatever the user's config), no commits unreachable from the remotes —
///    any failure here is `Skipped`;
/// 4. the guard (only the folder's own `.git` file is allowed) for the folder and, in Trash mode,
///    for the admin dir.
fn act_worktree(
    env: &ExecuteEnv,
    f: &Finding,
    rule: &RuleDef,
    op: Op,
    dry_run: bool,
    protected: &[PathBuf],
) -> Result<Done, Stop> {
    let path = Path::new(f.path.as_deref().ok_or("finding has no path")?);
    let argv = f.command.as_deref().unwrap_or_default();
    command::check_argv(argv, ActionKind::GitWorktreeRemove, Some(rule.detector))?;
    if std::fs::symlink_metadata(path).is_err() {
        return Err("no longer exists".into());
    }
    let repo = check_worktree_argv(argv, path)?;
    ensure_linked_worktree(path)?;
    let admin = worktree_admin_dir(path, Path::new(repo))?;
    let git = command::resolve_allowed(&argv[0])?;
    recheck_worktree(&git, path, &admin).map_err(Stop::Skip)?;

    let target = guard::check_worktree_target_with(path, &env.home, protected).map_err(guard_err)?;
    ensure_not_running(env, rule)?;
    match op {
        Op::WorktreeTrash => {
            let admin = guard::check_target_with(&admin, &env.home, protected)
                .map_err(|e| format!("worktree admin dir: {}", guard_err(e)))?;
            if dry_run {
                return Ok(Done {
                    op,
                    message: format!("dry run: would move to Trash with its git admin dir {}", admin.display()),
                });
            }
            (env.trash)(&target)?;
            let message = match (env.trash)(&admin) {
                Ok(()) => format!(
                    "moved to Trash with its git admin dir {} (put both back to restore the worktree)",
                    admin.display()
                ),
                // The folder is already in the Trash: the admin dir stays as a stale entry, which the
                // next scan lists (information only; the user prunes it by hand).
                Err(e) => format!("moved to Trash; its git admin dir {} stayed ({e})", admin.display()),
            };
            Ok(Done { op, message })
        }
        _ => {
            if dry_run {
                return Ok(Done { op, message: format!("dry run: would run `{}`", command::display_argv(argv)) });
            }
            run_checked(&git, argv, GIT_REMOVE_TIMEOUT)?;
            Ok(Done { op, message: "removed git worktree".into() })
        }
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

/// The worktree argv (already pattern-checked) names the finding's path. Returns `<repo>`.
fn check_worktree_argv<'a>(argv: &'a [String], path: &Path) -> Result<&'a str, String> {
    match argv {
        [_, _, repo, _, _, wt] => {
            let resolved = canonical_entry(Path::new(wt)).ok_or_else(|| format!("cannot resolve {wt}"))?;
            let target = canonical_entry(path).ok_or_else(|| format!("cannot resolve {}", path.display()))?;
            if same_path(&resolved, &target) {
                Ok(repo)
            } else {
                Err(format!("worktree command targets {wt}, not the finding's path"))
            }
        }
        _ => Err(format!("unexpected worktree command `{}`", command::display_argv(argv))),
    }
}

/// A linked worktree has a `.git` **file** (gitdir pointer); a `.git` directory would be a
/// repository of its own.
fn ensure_linked_worktree(target: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(target.join(".git")) {
        Ok(m) if m.is_file() => Ok(()),
        _ => Err("not a linked git worktree (no .git file)".into()),
    }
}

/// The worktree's own admin dir, from its `.git` file (`gitdir: <repo>/.git/worktrees/<id>`),
/// checked both ways: it is a direct child of `<repo>/.git/worktrees` and its `gitdir` file points
/// back at this worktree's `.git`.
fn worktree_admin_dir(worktree: &Path, repo: &Path) -> Result<PathBuf, String> {
    let dot_git = worktree.join(".git");
    let text = std::fs::read_to_string(&dot_git).map_err(|e| format!("cannot read {}: {e}", dot_git.display()))?;
    let pointer = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| format!("{} has no gitdir line", dot_git.display()))?;
    let admin = if Path::new(pointer).is_absolute() { PathBuf::from(pointer) } else { worktree.join(pointer) };
    let admin =
        std::fs::canonicalize(&admin).map_err(|e| format!("worktree admin dir {} is missing: {e}", admin.display()))?;
    let worktrees = std::fs::canonicalize(repo.join(".git/worktrees"))
        .map_err(|e| format!("{} has no .git/worktrees: {e}", repo.display()))?;
    if !admin.parent().is_some_and(|p| same_path(p, &worktrees)) || !admin.is_dir() {
        return Err(format!("worktree admin dir {} is not inside {}", admin.display(), worktrees.display()));
    }
    let back = std::fs::read_to_string(admin.join("gitdir"))
        .map_err(|e| format!("cannot read {}/gitdir: {e}", admin.display()))?;
    let back = canonical_entry(Path::new(back.trim()));
    let own = canonical_entry(&dot_git);
    match (back, own) {
        (Some(back), Some(own)) if same_path(&back, &own) => Ok(admin),
        _ => Err(format!("worktree admin dir {} belongs to another worktree", admin.display())),
    }
}

/// Fresh re-check of a worktree that is about to be removed (never takes the index lock). The
/// error says why it is no longer safe.
fn recheck_worktree(git: &Path, worktree: &Path, admin: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(admin.join("locked")).is_ok() {
        let reason = std::fs::read_to_string(admin.join("locked")).unwrap_or_default();
        let reason = reason.trim();
        return Err(if reason.is_empty() {
            "worktree is locked".into()
        } else {
            format!("worktree is locked: {reason}")
        });
    }
    if std::fs::symlink_metadata(admin.join("modules")).is_ok() {
        return Err(format!(
            "worktree has initialized submodules (their git data is in {}/modules); remove it by hand",
            admin.display()
        ));
    }
    let wt = worktree.to_string_lossy().into_owned();
    let git_in_worktree = |args: &[&str]| -> Result<String, String> {
        let mut argv: Vec<String> = vec!["--no-optional-locks".into(), "-C".into(), wt.clone()];
        argv.extend(args.iter().map(|a| a.to_string()));
        let out =
            command::run_with_timeout(git, &argv, GIT_STATUS_TIMEOUT).map_err(|e| format!("git {}: {e}", args[0]))?;
        if out.success {
            Ok(out.stdout)
        } else {
            Err(format!("git {} failed: {}", args[0], command::tail(&out.stderr, 300)))
        }
    };
    let status = git_in_worktree(&WORKTREE_STATUS_ARGS)?;
    let changed = status.lines().filter(|l| !l.trim().is_empty()).count();
    if changed > 0 {
        return Err(format!("worktree has uncommitted changes ({changed} path(s))"));
    }
    let unpushed = git_in_worktree(&["rev-list", "--count", "HEAD", "--not", "--remotes"])?;
    match unpushed.trim().parse::<u64>() {
        Ok(0) => Ok(()),
        Ok(n) => Err(format!("worktree has {n} commit(s) that are not on any remote branch")),
        Err(_) => Err(format!("unexpected `git rev-list --count` output: {}", unpushed.trim())),
    }
}

/// `git status` arguments for "is this worktree clean?", shared with the `git-worktrees` detector:
/// every untracked file and every submodule counts, whatever the user's or repository's config
/// (`status.showUntrackedFiles=no` must not hide files from the check).
pub const WORKTREE_STATUS_ARGS: [&str; 4] =
    ["status", "--porcelain", "--untracked-files=all", "--ignore-submodules=none"];

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
pub fn move_to_trash(target: &Path) -> Result<(), String> {
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
mod regression_tests;
#[cfg(test)]
mod tests;
