//! Scan orchestration (owned by S1).
//!
//! [`scan_at`] runs the selected rules in parallel (rayon), turns every failing (or panicking)
//! detector into a [`ScanError`], adds `requires_quit` blockers, drops findings below
//! `min_finding_bytes` — or the rule's own `params.min_bytes` when it sets one (safety fix 4) —
//! (`Command` and `InfoOnly` findings are exempt), removes findings covered by an
//! unblocked ancestor finding (ARCHITECTURE.md §10) and sorts by allocated bytes.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use chrono::{DateTime, SecondsFormat, Utc};
use rayon::prelude::*;

use crate::config::{Config, home_dir};
use crate::detectors::detector_for;
use crate::error::CoreError;
use crate::model::{ActionKind, Finding, RuleDef, ScanError, ScanOptions, ScanPhase, ScanProgress, ScanReport};
use crate::process::ProcessSnapshot;
use crate::size::Sizer;
use crate::volume::{has_full_disk_access, volume_info};

/// Everything a detector may consult. Detectors must take `home` from here, never from the env,
/// so tests can point it at a tempdir.
pub struct ScanContext<'a> {
    pub home: PathBuf,
    pub now: DateTime<Utc>,
    pub sizer: &'a Sizer,
    pub processes: &'a ProcessSnapshot,
    pub config: &'a Config,
}

impl ScanContext<'_> {
    /// Records a non-fatal problem (it ends up in `ScanReport::errors`) without failing the rule,
    /// e.g. "mdls: timed out" or an unreadable sub-directory the detector skipped.
    pub fn warn(&self, path: impl Into<String>, message: impl Into<String>) {
        self.sizer.push_error(path, message);
    }
}

/// Runs the enabled rules selected by `opts` against the real home directory.
pub fn scan(
    rules: &[RuleDef],
    opts: &ScanOptions,
    config: &Config,
    on_progress: &(dyn Fn(ScanProgress) + Sync),
) -> ScanReport {
    scan_at(&home_dir(), rules, opts, config, on_progress)
}

/// [`scan`] with an explicit home directory (tests use a tempdir). Progress: one `Volume` event,
/// one `Rules` event before the first rule and after each finished rule, one `Done` event.
pub fn scan_at(
    home: &Path,
    rules: &[RuleDef],
    opts: &ScanOptions,
    config: &Config,
    on_progress: &(dyn Fn(ScanProgress) + Sync),
) -> ScanReport {
    scan_with(home, rules, opts, config, ProcessSnapshot::capture, on_progress)
}

/// Whether `opts` selects every enabled rule (no id and no category filter). Only such scans are
/// recorded to history (contract v2): growth compares whole scans, so a filtered one would make
/// every other finding look like it disappeared.
pub fn is_full_scan(opts: &ScanOptions) -> bool {
    opts.rule_ids.is_none() && opts.categories.is_none()
}

/// The rules `opts` selects: enabled, and matching the id / category filters when given.
pub fn select_rules<'r>(rules: &'r [RuleDef], opts: &ScanOptions) -> Vec<&'r RuleDef> {
    rules
        .iter()
        .filter(|r| r.enabled)
        .filter(|r| opts.rule_ids.as_ref().is_none_or(|ids| ids.contains(&r.id)))
        .filter(|r| opts.categories.as_ref().is_none_or(|cats| cats.contains(&r.category)))
        .collect()
}

pub(crate) fn scan_with(
    home: &Path,
    rules: &[RuleDef],
    opts: &ScanOptions,
    config: &Config,
    capture_processes: impl FnOnce() -> ProcessSnapshot,
    on_progress: &(dyn Fn(ScanProgress) + Sync),
) -> ScanReport {
    let started = Instant::now();
    let started_at = Utc::now();
    let selected = select_rules(rules, opts);
    let total_rules = selected.len() as u32;
    let progress = |phase, rule_id: Option<&str>, done_rules, findings| {
        on_progress(ScanProgress { phase, rule_id: rule_id.map(str::to_string), done_rules, total_rules, findings })
    };

    progress(ScanPhase::Volume, None, 0, 0);
    let volume = volume_info(home);
    let full_disk_access = has_full_disk_access(home);
    let processes = capture_processes();
    let sizer = Sizer::new();
    let ctx = ScanContext { home: home.to_path_buf(), now: started_at, sizer: &sizer, processes: &processes, config };

    progress(ScanPhase::Rules, None, 0, 0);
    let done = AtomicU32::new(0);
    let found = AtomicU32::new(0);
    let results: Vec<(&RuleDef, Result<Vec<Finding>, CoreError>)> = selected
        .par_iter()
        .map(|&rule| {
            let result = run_rule(rule, &ctx);
            let count = result.as_ref().map_or(0, |f| f.len() as u32);
            let found_now = found.fetch_add(count, Ordering::Relaxed) + count;
            let done_now = done.fetch_add(1, Ordering::Relaxed) + 1;
            progress(ScanPhase::Rules, Some(&rule.id), done_now, found_now);
            (rule, result)
        })
        .collect();

    let mut findings = Vec::new();
    let mut errors = Vec::new();
    for (rule, result) in results {
        match result {
            Ok(found) => findings.extend(found),
            Err(e) => errors.push(ScanError { path: rule.id.clone(), message: e.to_string() }),
        }
    }
    let min_bytes: HashMap<&str, u64> = selected.iter().map(|r| (r.id.as_str(), min_bytes_for(r, config))).collect();
    findings
        .retain(|f| above_min_size(f, min_bytes.get(f.rule_id.as_str()).copied().unwrap_or(config.min_finding_bytes)));
    let mut findings = drop_covered_findings(findings);
    findings.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.id.cmp(&b.id)));

    let mut walk_errors = sizer.take_errors();
    walk_errors.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.message.cmp(&b.message)));
    walk_errors.dedup_by(|a, b| a.path == b.path && a.message == b.message);
    errors.extend(walk_errors);

    progress(ScanPhase::Done, None, total_rules, findings.len() as u32);
    ScanReport {
        id: started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        started_at,
        duration_ms: started.elapsed().as_millis() as u64,
        volume,
        full_disk_access,
        findings,
        errors,
    }
}

/// The size threshold for `rule`'s findings: its `params.min_bytes` when set (e.g. 5 MiB for Claude
/// Code transcripts), otherwise the global `min_finding_bytes`.
pub fn min_bytes_for(rule: &RuleDef, config: &Config) -> u64 {
    rule.params.get("min_bytes").and_then(serde_json::Value::as_u64).unwrap_or(config.min_finding_bytes)
}

/// `min_finding_bytes` filter (contract v2): `Command` and `InfoOnly` findings are kept whatever
/// their size (the zero-byte list of stale worktree entries, a blocked simulator runtime).
fn above_min_size(f: &Finding, min_bytes: u64) -> bool {
    f.bytes >= min_bytes || matches!(f.action, ActionKind::Command | ActionKind::InfoOnly)
}

/// One rule: detector (a panic becomes an error) + `requires_quit` blockers.
fn run_rule(rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
    let detector = detector_for(rule.detector);
    let mut findings = catch_unwind(AssertUnwindSafe(|| detector.detect(rule, ctx))).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| String::from("unknown panic"));
        Err(CoreError::Rule(format!("detector panicked: {message}")))
    })?;
    let running: Vec<String> = rule
        .requires_quit
        .iter()
        .filter(|name| ctx.processes.is_running(name))
        .map(|name| format!("{name} is running"))
        .collect();
    for finding in &mut findings {
        for blocker in &running {
            if !finding.blockers.contains(blocker) {
                finding.add_blocker(blocker.clone());
            }
        }
    }
    Ok(findings)
}

/// Nested findings (ARCHITECTURE.md §10): a finding whose path is inside (or equal to) the path of
/// another finding **without** blockers is dropped — cleaning the ancestor covers it. When every
/// enclosing finding is blocked, the nested one is kept so it can still be cleaned on its own.
/// Among unblocked findings with the very same path, the one with the smallest id is kept.
/// Findings without a path are never nested. Order is preserved.
pub fn drop_covered_findings(findings: Vec<Finding>) -> Vec<Finding> {
    // path -> index of the unblocked finding (smallest id) that covers it
    let mut covers: HashMap<&Path, usize> = HashMap::new();
    for (i, f) in findings.iter().enumerate() {
        if let (true, Some(path)) = (f.blockers.is_empty(), f.path.as_deref()) {
            covers
                .entry(Path::new(path))
                .and_modify(|best| {
                    if f.id < findings[*best].id {
                        *best = i;
                    }
                })
                .or_insert(i);
        }
    }
    let dropped: HashSet<usize> = findings
        .iter()
        .enumerate()
        .filter(|(i, f)| {
            let Some(path) = f.path.as_deref().map(Path::new) else { return false };
            covers.get(path).is_some_and(|best| best != i) || path.ancestors().skip(1).any(|a| covers.contains_key(a))
        })
        .map(|(i, _)| i)
        .collect();
    findings.into_iter().enumerate().filter(|(i, _)| !dropped.contains(i)).map(|(_, f)| f).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use super::*;
    use crate::model::{Category, DetectorKind, Risk};

    const MIB: usize = 1024 * 1024;

    fn rule(id: &str, roots: &[&str]) -> RuleDef {
        RuleDef {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            category: Category::DevArtifacts,
            risk: Risk::Safe,
            detector: DetectorKind::Paths,
            roots: roots.iter().map(|r| r.to_string()).collect(),
            params: serde_json::json!({}),
            action: ActionKind::Trash,
            requires_quit: vec![],
            regenerates: true,
            enabled: true,
        }
    }

    fn config(min_bytes: u64) -> Config {
        Config { min_finding_bytes: min_bytes, ..Config::default() }
    }

    fn write(path: &Path, len: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1u8; len]).unwrap();
    }

    fn finding(id: &str, path: Option<&str>, blocked: bool) -> Finding {
        let mut f = Finding {
            id: id.into(),
            rule_id: "r".into(),
            category: Category::Worktrees,
            risk: Risk::Review,
            label: id.into(),
            path: path.map(str::to_string),
            bytes: 1,
            logical_bytes: 1,
            files: 1,
            last_used: None,
            reason: String::new(),
            action: ActionKind::Trash,
            command: None,
            selected_by_default: false,
            blockers: vec![],
            group: None,
        };
        if blocked {
            f.add_blocker("Uncommitted changes in worktree");
        }
        f
    }

    fn ids(findings: &[Finding]) -> Vec<&str> {
        findings.iter().map(|f| f.id.as_str()).collect()
    }

    #[test]
    fn only_unfiltered_options_are_a_full_scan() {
        assert!(is_full_scan(&ScanOptions::default()));
        assert!(!is_full_scan(&ScanOptions { rule_ids: Some(vec!["cache.npm".into()]), categories: None }));
        assert!(!is_full_scan(&ScanOptions { rule_ids: None, categories: Some(vec![Category::AiTools]) }));
        // An explicit empty filter still filters (selects nothing): not a full scan.
        assert!(!is_full_scan(&ScanOptions { rule_ids: Some(vec![]), categories: None }));
    }

    #[test]
    fn min_size_exempts_command_and_info_only_findings() {
        let finding = |action, bytes| Finding { action, bytes, ..finding("r:x", Some("/x"), false) };
        let min = 50 * MIB as u64;
        assert!(!above_min_size(&finding(ActionKind::Trash, min - 1), min));
        assert!(!above_min_size(&finding(ActionKind::Delete, 0), min));
        assert!(!above_min_size(&finding(ActionKind::GitWorktreeRemove, 1), min));
        assert!(above_min_size(&finding(ActionKind::Trash, min), min));
        assert!(above_min_size(&finding(ActionKind::Command, 0), min));
        assert!(above_min_size(&finding(ActionKind::InfoOnly, 0), min));
    }

    #[test]
    fn nested_finding_dropped_under_unblocked_parent() {
        let out = drop_covered_findings(vec![
            finding("wt", Some("/h/wt"), false),
            finding("nm", Some("/h/wt/app/node_modules"), false),
            finding("sibling", Some("/h/wt-2/node_modules"), false),
            finding("docker", None, false),
        ]);
        assert_eq!(ids(&out), vec!["wt", "sibling", "docker"]);
    }

    #[test]
    fn nested_finding_kept_under_blocked_parent() {
        let out = drop_covered_findings(vec![
            finding("wt", Some("/h/wt"), true),
            finding("nm", Some("/h/wt/app/node_modules"), false),
            finding("nm-blocked", Some("/h/wt/lib/node_modules"), true),
        ]);
        assert_eq!(ids(&out), vec!["wt", "nm", "nm-blocked"]);
    }

    #[test]
    fn unblocked_grandparent_covers_through_blocked_parent() {
        let out = drop_covered_findings(vec![
            finding("root", Some("/h/dev"), false),
            finding("wt", Some("/h/dev/wt"), true),
            finding("nm", Some("/h/dev/wt/node_modules"), false),
        ]);
        assert_eq!(ids(&out), vec!["root"]);
    }

    #[test]
    fn same_path_keeps_one_unblocked_finding() {
        let out = drop_covered_findings(vec![
            finding("b:/h/x", Some("/h/x"), false),
            finding("a:/h/x", Some("/h/x"), false),
            finding("c:/h/x", Some("/h/x"), true),
        ]);
        assert_eq!(ids(&out), vec!["a:/h/x"]);
        // Prefix of the string but not a path ancestor: not nested.
        let out = drop_covered_findings(vec![finding("a", Some("/h/x"), false), finding("b", Some("/h/xy"), false)]);
        assert_eq!(ids(&out), vec!["a", "b"]);
    }

    #[test]
    fn scan_at_runs_rules_filters_sorts_and_reports_progress() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join(".npm/_cacache/blob"), 3 * MIB);
        write(&home.path().join("Library/Caches/big/blob"), 5 * MIB);
        write(&home.path().join("Library/Caches/tiny/blob"), 1000);
        let mut rules = vec![
            rule("cache.npm", &["~/.npm/_cacache"]),
            rule("cache.apps", &["~/Library/Caches/*"]),
            rule("cache.missing", &["~/nope"]),
        ];
        let mut disabled = rule("cache.disabled", &["~/.npm"]);
        disabled.enabled = false;
        rules.push(disabled);

        let events = Mutex::new(Vec::new());
        let report = scan_at(home.path(), &rules, &ScanOptions::default(), &config(MIB as u64), &|p| {
            events.lock().unwrap().push(p)
        });

        let paths: Vec<_> = report.findings.iter().map(|f| f.path.clone().unwrap()).collect();
        let expect = |p: &str| home.path().join(p).to_string_lossy().into_owned();
        assert_eq!(paths, vec![expect("Library/Caches/big"), expect(".npm/_cacache")], "sorted, tiny dropped");
        assert!(report.findings[0].selected_by_default);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.volume.total_bytes > 0);

        let events = events.into_inner().unwrap();
        assert_eq!(events.first().unwrap().phase, ScanPhase::Volume);
        let rule_events: Vec<_> = events.iter().filter(|e| e.rule_id.is_some()).collect();
        assert_eq!(rule_events.len(), 3, "one per selected rule");
        assert!(rule_events.iter().all(|e| e.phase == ScanPhase::Rules && e.total_rules == 3));
        let last = events.last().unwrap();
        assert_eq!((last.phase, last.done_rules, last.findings), (ScanPhase::Done, 3, 2));
    }

    #[test]
    fn rule_level_min_bytes_replaces_the_global_minimum() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("small/blob"), MIB);
        write(&home.path().join("tiny/blob"), 1000);
        write(&home.path().join("other/blob"), MIB);
        let mut small = rule("rule.small", &["~/small", "~/tiny"]);
        small.params = serde_json::json!({ "min_bytes": 512 * 1024 });
        let rules = vec![small, rule("rule.other", &["~/other"])];
        let report = scan_at(home.path(), &rules, &ScanOptions::default(), &config(50 * MIB as u64), &|_| {});
        let paths: Vec<_> = report.findings.iter().map(|f| f.path.clone().unwrap()).collect();
        let expect = |p: &str| home.path().join(p).to_string_lossy().into_owned();
        assert_eq!(paths, vec![expect("small")], "1 MiB passes the rule's 512 KiB, not the global 50 MiB");
        assert_eq!(min_bytes_for(&rules[0], &config(7)), 512 * 1024);
        assert_eq!(min_bytes_for(&rules[1], &config(7)), 7);
    }

    #[test]
    fn scan_at_filters_by_rule_id_and_category() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("a/blob"), MIB);
        write(&home.path().join("b/blob"), MIB);
        let mut b = rule("rule.b", &["~/b"]);
        b.category = Category::AiTools;
        let rules = vec![rule("rule.a", &["~/a"]), b];
        let run = |opts: ScanOptions| {
            let report = scan_at(home.path(), &rules, &opts, &config(0), &|_| {});
            report.findings.into_iter().map(|f| f.rule_id).collect::<Vec<_>>()
        };
        assert_eq!(run(ScanOptions { rule_ids: Some(vec!["rule.b".into()]), categories: None }), vec!["rule.b"]);
        assert_eq!(run(ScanOptions { rule_ids: None, categories: Some(vec![Category::DevArtifacts]) }), vec!["rule.a"]);
        assert_eq!(run(ScanOptions::default()).len(), 2);
    }

    #[test]
    fn scan_with_adds_requires_quit_blockers() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("vm/blob"), MIB);
        let mut vm = rule("ai.vm", &["~/vm"]);
        vm.requires_quit = vec!["Claude".into(), "NotRunning".into()];
        let report = scan_with(
            home.path(),
            &[vm],
            &ScanOptions::default(),
            &config(0),
            || ProcessSnapshot::from_names(["Claude"]),
            &|_| {},
        );
        let f = &report.findings[0];
        assert_eq!(f.blockers, vec!["Claude is running".to_string()]);
        assert!(!f.selected_by_default, "blocked findings are never preselected");
    }

    #[test]
    fn scan_with_applies_nested_rule_both_ways() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("wt/app/node_modules/blob"), 2 * MIB);
        write(&home.path().join("wt/src.txt"), MIB);
        let mut worktree = rule("worktrees.wt", &["~/wt"]);
        worktree.requires_quit = vec!["Claude".into()];
        let nm = rule("dev.node-modules", &["~/wt/app/node_modules"]);
        let rules = [worktree, nm];
        let run = |running: &'static [&'static str]| {
            let report = scan_with(
                home.path(),
                &rules,
                &ScanOptions::default(),
                &config(0),
                || ProcessSnapshot::from_names(running.iter().copied()),
                &|_| {},
            );
            report.findings.into_iter().map(|f| f.rule_id).collect::<Vec<_>>()
        };
        assert_eq!(run(&[]), vec!["worktrees.wt"], "unblocked parent covers the child");
        assert_eq!(run(&["Claude"]), vec!["worktrees.wt", "dev.node-modules"], "blocked parent keeps the child");
    }

    #[test]
    fn failing_rule_becomes_scan_error() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("ok/blob"), MIB);
        let mut bad = rule("downloads.bad", &["~"]);
        bad.detector = DetectorKind::OlderThan;
        bad.params = serde_json::json!({ "days": "soon" });
        let report = scan_at(home.path(), &[bad, rule("ok", &["~/ok"])], &ScanOptions::default(), &config(0), &|_| {});
        assert_eq!(ids(&report.findings).len(), 1);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(report.errors[0].path, "downloads.bad");
        assert!(report.errors[0].message.contains("days"), "{}", report.errors[0].message);
    }

    #[test]
    fn walk_errors_reach_the_report() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("cache/locked/blob"), 1000);
        let locked = home.path().join("cache/locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let report = scan_at(home.path(), &[rule("c", &["~/cache"])], &ScanOptions::default(), &config(0), &|_| {});
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(report.errors[0].path, locked.to_string_lossy());
    }
}
