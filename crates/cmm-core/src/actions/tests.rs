//! Tempdir tests for plan/execute. Nothing here touches the real home directory: `ExecuteEnv::trash`
//! is a fake that moves items into `<tempdir>/Trash` (see [`fake_trash`]), except in the
//! `#[ignore]`d Trash test (NSFileManager moves the temp file into the real ~/.Trash).
//!
//! These tests drive the executor ([`execute_with`]) directly with hand-made reports, i.e. the
//! re-validation layer on its own; `regression_tests.rs` covers the fresh-scan layer on top.

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command as Proc;

use chrono::Utc;

use super::*;
use crate::model::{Category, CleanOutcome, DetectorKind, Risk, VolumeInfo};

/// `assert_eq!` on a result item's outcome, with optional context arguments.
macro_rules! assert_done {
    ($item:expr $(, $($arg:tt)+)?) => { assert_eq!($item.outcome, CleanOutcome::Done $(, $($arg)+)?) };
}
macro_rules! assert_failed {
    ($item:expr $(, $($arg:tt)+)?) => { assert_eq!($item.outcome, CleanOutcome::Failed $(, $($arg)+)?) };
}

struct Fx {
    tmp: tempfile::TempDir,
    home: PathBuf,
    env: ExecuteEnv,
}

fn rule(id: &str, roots: &[&str], action: ActionKind, requires_quit: &[&str]) -> RuleDef {
    let detector = match id {
        "dev.wt" | "dev.wt-cmd" => DetectorKind::GitWorktrees,
        "tool.docker" => DetectorKind::Docker,
        _ => DetectorKind::Paths,
    };
    RuleDef {
        id: id.into(),
        title: id.into(),
        description: String::new(),
        category: Category::AppCaches,
        risk: Risk::Safe,
        detector,
        roots: roots.iter().map(|s| s.to_string()).collect(),
        params: serde_json::json!({}),
        action,
        requires_quit: requires_quit.iter().map(|s| s.to_string()).collect(),
        regenerates: true,
        enabled: true,
    }
}

fn write_file(path: &Path, len: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; len]).unwrap();
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for rel in crate::guard::PROTECTED_RELATIVE {
        fs::create_dir_all(home.join(rel)).unwrap();
    }
    write_file(&home.join("Library/Caches/a/data/blob"), 1000);
    write_file(&home.join("Library/Caches/a/nested/blob"), 10);
    write_file(&home.join("Library/Caches/b/blob"), 500);
    write_file(&home.join("Library/Caches/c.bin"), 100);
    write_file(&home.join("vm/bundle/disk.img"), 2000);
    write_file(&tmp.path().join("outside/precious"), 1);
    let rules = vec![
        rule("cache.test", &["~/Library/Caches/*"], ActionKind::Trash, &[]),
        rule("app.vm", &["~/vm"], ActionKind::Trash, &["FakeApp"]),
        rule("dev.wt", &["~/Developer"], ActionKind::GitWorktreeRemove, &[]),
        rule("dev.wt-cmd", &["~/Developer"], ActionKind::Command, &[]),
        rule("tool.cmd", &[], ActionKind::Command, &[]),
        rule("tool.docker", &[], ActionKind::Command, &[]),
        rule("info.only", &["~/Library/Caches/*"], ActionKind::InfoOnly, &[]),
    ];
    let env = ExecuteEnv {
        home: home.clone(),
        rules,
        extra_protected: vec![],
        processes: Box::new(|| ProcessSnapshot::from_names(Vec::<String>::new())),
        trash: fake_trash(&tmp.path().join("Trash")),
    };
    Fx { tmp, home, env }
}

/// A Trash stand-in: moves into `dir`, renaming on name clashes like Finder does (`wt`, `wt 2`, …).
pub(super) fn fake_trash(dir: &Path) -> Box<TrashFn> {
    fs::create_dir_all(dir).unwrap();
    let dir = dir.to_path_buf();
    Box::new(move |p: &Path| {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let dest = (1..)
            .map(|n| if n == 1 { dir.join(&name) } else { dir.join(format!("{name} {n}")) })
            .find(|d| d.symlink_metadata().is_err())
            .unwrap();
        fs::rename(p, dest).map_err(|e| format!("fake trash: {e}"))
    })
}

impl Fx {
    /// Where the fake `ExecuteEnv::trash` puts things.
    fn trash(&self) -> PathBuf {
        self.tmp.path().join("Trash")
    }
}

fn finding(rule_id: &str, path: Option<&Path>, bytes: u64, action: ActionKind) -> Finding {
    let key = path.map_or_else(|| "key".to_string(), |p| p.to_string_lossy().into_owned());
    Finding {
        id: format!("{rule_id}:{key}"),
        rule_id: rule_id.into(),
        category: Category::AppCaches,
        risk: Risk::Safe,
        label: path.and_then(|p| p.file_name()).map_or_else(|| key.clone(), |n| n.to_string_lossy().into_owned()),
        path: path.map(|p| p.to_string_lossy().into_owned()),
        bytes,
        logical_bytes: bytes,
        files: 1,
        last_used: None,
        reason: "test".into(),
        action,
        command: None,
        selected_by_default: true,
        blockers: vec![],
        group: None,
    }
}

fn with_command(mut f: Finding, argv: &[&str]) -> Finding {
    f.command = Some(argv.iter().map(|s| s.to_string()).collect());
    f
}

fn report(mut findings: Vec<Finding>) -> ScanReport {
    findings.sort_by_key(|f| std::cmp::Reverse(f.bytes));
    let started_at = Utc::now();
    ScanReport {
        id: started_at.to_rfc3339(),
        started_at,
        duration_ms: 1,
        volume: VolumeInfo {
            total_bytes: 100,
            free_bytes: 10,
            available_important_bytes: None,
            swap_used_bytes: None,
            trash_bytes: None,
        },
        full_disk_access: true,
        findings,
        errors: vec![],
    }
}

fn req(findings: &[&Finding], mode: CleanMode, dry_run: bool) -> CleanRequest {
    CleanRequest { finding_ids: findings.iter().map(|f| f.id.clone()).collect(), mode, dry_run }
}

fn run(env: &ExecuteEnv, report: &ScanReport, req: &CleanRequest, history: Option<&History>) -> CleanResult {
    let seen = RefCell::new(Vec::new());
    let result = execute_with(env, report, req, history, &|item| seen.borrow_mut().push(item.finding_id.clone()));
    let streamed = seen.into_inner();
    assert_eq!(streamed, result.items.iter().map(|i| i.finding_id.clone()).collect::<Vec<_>>(), "on_item per item");
    result
}

fn item<'a>(result: &'a CleanResult, f: &Finding) -> &'a CleanItemResult {
    result.items.iter().find(|i| i.finding_id == f.id).unwrap_or_else(|| panic!("no result for {}", f.id))
}

fn msg(i: &CleanItemResult) -> &str {
    i.message.as_deref().unwrap_or("")
}

// ---------------------------------------------------------------------------------------------
// plan

#[test]
fn plan_excludes_info_only_blocked_malformed_and_covered() {
    let fx = fixture();
    let h = &fx.home;
    let a = finding("cache.test", Some(&h.join("Library/Caches/a")), 1010, ActionKind::Trash);
    let nested = finding("cache.test", Some(&h.join("Library/Caches/a/nested")), 10, ActionKind::Trash);
    let mut blocked = finding("app.vm", Some(&h.join("vm")), 2000, ActionKind::Trash);
    blocked.add_blocker("FakeApp is running");
    let info = finding("info.only", Some(&h.join("Library/Caches/b")), 500, ActionKind::InfoOnly);
    let no_path = finding("cache.test", None, 5, ActionKind::Trash);
    let cmd = with_command(finding("tool.cmd", None, 300, ActionKind::Command), &["docker", "system", "prune", "-f"]);
    let rep = report(vec![a.clone(), nested.clone(), blocked.clone(), info.clone(), no_path.clone(), cmd.clone()]);

    let plan = plan(&rep, &req(&[&nested, &a, &blocked, &info, &no_path, &cmd], CleanMode::Trash, true)).unwrap();
    let ids: Vec<&str> = plan.items.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, [a.id.as_str(), cmd.id.as_str()], "report order, exclusions removed");
    assert_eq!(plan.total_bytes, 1010 + 300);
    let w = plan.warnings.join("\n");
    assert!(w.contains("nested: inside a, removed with it"), "{w}");
    assert!(w.contains("blocked: FakeApp is running"), "{w}");
    assert!(w.contains("information only"), "{w}");
    assert!(w.contains("no path"), "{w}");
    assert!(w.contains("frees no disk space until the Trash is emptied"), "{w}");
    assert!(w.contains("runs `docker system prune -f`"), "{w}");
}

#[test]
fn plan_unknown_id_is_an_error() {
    let fx = fixture();
    let a = finding("cache.test", Some(&fx.home.join("Library/Caches/a")), 1, ActionKind::Trash);
    let rep = report(vec![a.clone()]);
    let r = CleanRequest { finding_ids: vec![a.id.clone(), "nope:/x".into()], mode: CleanMode::Trash, dry_run: true };
    let err = plan(&rep, &r).unwrap_err();
    assert!(matches!(err, CoreError::NotFound(ref m) if m.contains("nope:/x")), "{err}");
}

#[test]
fn plan_blocked_parent_keeps_child_and_dedupes() {
    let fx = fixture();
    let h = &fx.home;
    let mut parent = finding("cache.test", Some(&h.join("Library/Caches/a")), 1010, ActionKind::Trash);
    parent.add_blocker("in use");
    let child = finding("cache.test", Some(&h.join("Library/Caches/a/nested")), 10, ActionKind::Trash);
    // Same path reported by a second rule: the first (larger) one wins, the other is covered.
    let twin = finding("info2", Some(&h.join("Library/Caches/a/nested")), 9, ActionKind::Delete);
    let rep = report(vec![parent.clone(), child.clone(), twin.clone()]);
    let r = CleanRequest {
        finding_ids: vec![parent.id.clone(), child.id.clone(), child.id.clone(), twin.id.clone()],
        mode: CleanMode::Delete,
        dry_run: true,
    };
    let plan = plan(&rep, &r).unwrap();
    assert_eq!(plan.items.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), [child.id.as_str()]);
    assert_eq!(plan.total_bytes, 10);
    let w = plan.warnings.join("\n");
    assert!(w.contains("blocked: in use"), "{w}");
    assert!(w.contains("Permanent delete: 1 item(s)"), "{w}");
    assert_eq!(plan.warnings.iter().filter(|w| w.contains("removed with it")).count(), 1, "{w}");
}

#[test]
fn plan_rejects_non_allowlisted_commands() {
    let fx = fixture();
    let rm = with_command(finding("tool.cmd", None, 1, ActionKind::Command), &["rm", "-rf", "/"]);
    let abs = with_command(finding("tool.cmd2", None, 1, ActionKind::Command), &["/usr/bin/git", "gc"]);
    let empty = with_command(finding("tool.cmd3", None, 1, ActionKind::Command), &[]);
    let wt = with_command(
        finding("dev.wt", Some(&fx.home.join("Developer/x")), 1, ActionKind::GitWorktreeRemove),
        &["sh", "-c", "rm -rf ~"],
    );
    let rep = report(vec![rm.clone(), abs.clone(), empty.clone(), wt.clone()]);
    let plan = plan(&rep, &req(&[&rm, &abs, &empty, &wt], CleanMode::Trash, true)).unwrap();
    assert!(plan.items.is_empty());
    let w = plan.warnings.join("\n");
    assert!(w.contains("`rm` is not an allowed program"), "{w}");
    assert!(w.contains("`/usr/bin/git` is not an allowed program"), "{w}");
    assert!(w.contains("no command"), "{w}");
    assert!(w.contains("`sh` is not an allowed program"), "{w}");
}

#[test]
fn plan_warns_about_worktree_removal() {
    let fx = fixture();
    let p = fx.home.join("Developer/repo/.claude/worktrees/x");
    let wt = with_command(
        finding("dev.wt", Some(&p), 7, ActionKind::GitWorktreeRemove),
        &["git", "-C", "/r", "worktree", "remove", &p.to_string_lossy()],
    );
    let rep = report(vec![wt.clone()]);
    // Worktrees follow the mode: Trash mode → folder and admin dir to the Trash, frees nothing yet.
    let trash = plan(&rep, &req(&[&wt], CleanMode::Trash, true)).unwrap();
    assert_eq!(trash.items.len(), 1);
    let w = trash.warnings.join("\n");
    assert!(
        w.contains("Trash is emptied (7 B in 1 item(s))") && w.contains("together with their git admin data"),
        "{w}"
    );
    assert!(!w.contains("git worktree remove") && !w.contains("prune"), "{w}");
    // Delete mode → `git worktree remove`, permanent.
    let delete = plan(&rep, &req(&[&wt], CleanMode::Delete, true)).unwrap();
    let w = delete.warnings.join("\n");
    assert!(w.contains("removed with `git worktree remove`: permanent"), "{w}");
    assert!(!w.contains("Trash is emptied") && !w.contains("Permanent delete:"), "no plain path items: {w}");
}

// ---------------------------------------------------------------------------------------------
// execute

#[test]
fn dry_run_has_no_side_effects() {
    let fx = fixture();
    let h = &fx.home;
    let (repo, wt_path) = repo_with_worktree(h);
    let a = finding("cache.test", Some(&h.join("Library/Caches/a")), 1010, ActionKind::Trash);
    let c = finding("cache.test", Some(&h.join("Library/Caches/c.bin")), 100, ActionKind::Trash);
    let wt = worktree_finding(&repo, &wt_path, 50);
    let rep = report(vec![a.clone(), c.clone(), wt.clone()]);
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();

    for mode in [CleanMode::Delete, CleanMode::Trash] {
        let res = run(&fx.env, &rep, &req(&[&a, &c, &wt], mode, true), Some(&history));
        assert!(res.dry_run);
        assert!(res.items.iter().all(|i| i.outcome == CleanOutcome::Done), "{:?}", res.items);
        assert!(msg(item(&res, &a)).starts_with("dry run"));
        match mode {
            CleanMode::Delete => assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (1010 + 100 + 50, 0)),
            CleanMode::Trash => assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (0, 1010 + 100 + 50)),
        }
    }
    assert!(h.join("Library/Caches/a/data/blob").exists());
    assert!(h.join("Library/Caches/c.bin").exists());
    assert!(wt_path.join("README").exists());
    assert!(git(&repo, &["worktree", "list"]).contains("worktrees/wt"));
    assert!(history.recent_actions(10).unwrap().is_empty(), "dry run writes no audit log");
}

#[test]
fn delete_removes_items_records_them_and_reports_covered_children() {
    let fx = fixture();
    let h = &fx.home;
    symlink(fx.tmp.path().join("outside"), h.join("Library/Caches/link")).unwrap();
    let a = finding("cache.test", Some(&h.join("Library/Caches/a")), 1010, ActionKind::Trash);
    let nested = finding("cache.test", Some(&h.join("Library/Caches/a/nested")), 10, ActionKind::Trash);
    let c = finding("cache.test", Some(&h.join("Library/Caches/c.bin")), 100, ActionKind::Delete);
    let link = finding("cache.test", Some(&h.join("Library/Caches/link")), 1, ActionKind::Trash);
    let rep = report(vec![a.clone(), nested.clone(), c.clone(), link.clone()]);
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();

    let res = run(&fx.env, &rep, &req(&[&nested, &a, &c, &link], CleanMode::Delete, false), Some(&history));
    assert!(!res.dry_run);
    assert!(res.items.iter().all(|i| i.outcome == CleanOutcome::Done), "{:?}", res.items);
    assert_eq!(res.freed_bytes, 1010 + 100 + 1);
    assert_eq!(res.moved_to_trash_bytes, 0);
    assert_eq!(res.items.last().unwrap().finding_id, nested.id, "covered child reported after its parent");
    assert_eq!((item(&res, &nested).bytes, msg(item(&res, &nested))), (0, "included in a"));
    assert!(!h.join("Library/Caches/a").exists());
    assert!(!h.join("Library/Caches/c.bin").exists());
    assert!(h.join("Library/Caches/link").symlink_metadata().is_err(), "link removed");
    assert!(fx.tmp.path().join("outside/precious").exists(), "link target untouched");
    assert!(h.join("Library/Caches/b/blob").exists(), "unselected item untouched");

    let log = history.recent_actions(10).unwrap();
    assert_eq!(log.len(), 3, "acted-on items are audited, covered child is not");
    assert!(log.iter().all(|r| r.outcome == CleanOutcome::Done && r.mode == "delete"));
}

#[test]
fn revalidation_failures_do_not_stop_the_rest() {
    let fx = fixture();
    let h = &fx.home;
    fs::create_dir_all(h.join("Library/Caches/gone")).unwrap();
    let gone = finding("cache.test", Some(&h.join("Library/Caches/gone")), 9, ActionKind::Trash);
    fs::remove_dir(h.join("Library/Caches/gone")).unwrap();
    let protected = finding("cache.test", Some(&h.join("Library")), 8, ActionKind::Trash);
    let outside = finding("cache.test", Some(&fx.tmp.path().join("outside")), 7, ActionKind::Trash);
    let wrong_rule = finding("cache.test", Some(&h.join("vm/bundle")), 6, ActionKind::Trash);
    let no_rule = finding("removed.rule", Some(&h.join("Library/Caches/b")), 5, ActionKind::Trash);
    let good = finding("cache.test", Some(&h.join("Library/Caches/c.bin")), 1, ActionKind::Trash);
    let all = [&gone, &protected, &outside, &wrong_rule, &no_rule, &good];
    let rep = report(all.iter().map(|f| (*f).clone()).collect());
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();

    let res = run(&fx.env, &rep, &req(&all, CleanMode::Delete, false), Some(&history));
    assert_eq!(msg(item(&res, &gone)), "no longer exists");
    assert!(msg(item(&res, &protected)).contains("protected"), "{}", msg(item(&res, &protected)));
    assert!(msg(item(&res, &outside)).contains("outside the home directory"));
    assert!(msg(item(&res, &wrong_rule)).contains("outside its roots"));
    assert!(msg(item(&res, &no_rule)).contains("no longer in the catalog"));
    assert_done!(item(&res, &good), "{:?}", item(&res, &good));
    assert_eq!(res.items.iter().filter(|i| i.outcome == CleanOutcome::Failed).count(), 5);
    assert_eq!(res.freed_bytes, 1);
    assert!(!h.join("Library/Caches/c.bin").exists());
    assert!(h.join("Library").exists() && fx.tmp.path().join("outside/precious").exists());
    assert!(h.join("vm/bundle/disk.img").exists() && h.join("Library/Caches/b/blob").exists());

    let log = history.recent_actions(10).unwrap();
    assert_eq!(log.len(), 6, "refusals are audited too");
    assert_eq!(log.iter().filter(|r| r.outcome == CleanOutcome::Done).count(), 1);
}

#[test]
fn history_data_dir_is_protected() {
    let mut fx = fixture();
    let h = fx.home.clone();
    fx.env.rules.push(rule("dot.local", &["~/.local/*"], ActionKind::Trash, &[]));
    let history = History::open(&h.join(".local/cmm/history.sqlite")).unwrap();
    let f = finding("dot.local", Some(&h.join(".local/cmm")), 1, ActionKind::Trash);
    let rep = report(vec![f.clone()]);
    let res = run(&fx.env, &rep, &req(&[&f], CleanMode::Delete, false), Some(&history));
    assert_failed!(item(&res, &f));
    assert!(msg(item(&res, &f)).contains("protected"), "{}", msg(item(&res, &f)));
    assert!(h.join(".local/cmm/history.sqlite").exists());
}

#[test]
fn revalidation_catches_a_process_that_started_after_the_scan() {
    let mut fx = fixture();
    let vm = finding("app.vm", Some(&fx.home.join("vm")), 2000, ActionKind::Trash);
    assert!(vm.blockers.is_empty(), "FakeApp was not running at scan time");
    let rep = report(vec![vm.clone()]);
    fx.env.processes = Box::new(|| ProcessSnapshot::from_names(["FakeApp"]));
    let res = run(&fx.env, &rep, &req(&[&vm], CleanMode::Delete, false), None);
    assert_failed!(item(&res, &vm));
    assert_eq!(msg(item(&res, &vm)), "FakeApp is running — quit it first");
    assert!(fx.home.join("vm/bundle/disk.img").exists());
    assert_eq!(res.freed_bytes, 0);
}

#[test]
fn revalidation_uses_a_live_process_snapshot() {
    let mut fx = fixture();
    fx.env.rules.push(rule("needs.quit", &["~/vm"], ActionKind::Trash, &["sleep"]));
    fx.env.processes = Box::new(ProcessSnapshot::capture);
    let vm = finding("needs.quit", Some(&fx.home.join("vm")), 2000, ActionKind::Trash);
    let rep = report(vec![vm.clone()]);

    let mut child = Proc::new("/bin/sleep").arg("30").spawn().unwrap();
    let res = run(&fx.env, &rep, &req(&[&vm], CleanMode::Delete, true), None);
    let _ = child.kill();
    let _ = child.wait();
    assert_failed!(item(&res, &vm), "{:?}", item(&res, &vm));
    assert!(msg(item(&res, &vm)).contains("sleep is running"));
}

#[test]
fn command_allowlist_is_enforced_at_execution() {
    let fx = fixture();
    let victim = fx.home.join("Library/Caches/b");
    let rm = with_command(finding("tool.cmd", None, 1, ActionKind::Command), &["rm", "-rf", &victim.to_string_lossy()]);
    let sh = with_command(
        finding("tool.cmd", Some(&fx.home.join("x")), 1, ActionKind::Command),
        &["sh", "-c", &format!("rm -rf {}", victim.display())],
    );
    let rep = report(vec![rm.clone(), sh.clone()]);
    let res = run(&fx.env, &rep, &req(&[&rm, &sh], CleanMode::Delete, false), None);
    assert!(
        res.items.iter().all(|i| i.outcome == CleanOutcome::Skipped && msg(i).contains("is not an allowed program")),
        "{:?}",
        res.items
    );
    assert!(victim.join("blob").exists());
}

#[test]
fn allowlisted_command_runs_and_failures_capture_stderr() {
    let fx = fixture();
    let (repo, _) = repo_with_worktree(&fx.home);
    let repo_s = repo.to_string_lossy().into_owned();
    let ok = with_command(
        finding("dev.wt-cmd", None, 300, ActionKind::Command),
        &["git", "-C", &repo_s, "worktree", "prune"],
    );
    let missing = fx.home.join("Developer/missing-repo").to_string_lossy().into_owned();
    let mut bad = with_command(
        finding("dev.wt-cmd", None, 200, ActionKind::Command),
        &["git", "-C", &missing, "worktree", "prune"],
    );
    bad.id = "dev.wt-cmd:bad".into();
    let rep = report(vec![ok.clone(), bad.clone()]);
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();
    let res = run(&fx.env, &rep, &req(&[&ok, &bad], CleanMode::Trash, false), Some(&history));
    assert_done!(item(&res, &ok), "{:?}", item(&res, &ok));
    assert_eq!(msg(item(&res, &ok)), format!("ran `git -C {repo_s} worktree prune`"));
    assert_failed!(item(&res, &bad));
    assert!(msg(item(&res, &bad)).contains("cannot change to"), "{}", msg(item(&res, &bad)));
    assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (300, 0), "commands free space directly");
}

/// Safety fix 6: an allowlisted program with any other argv, or a pattern from another detector,
/// is refused by the plan and by the executor.
#[test]
fn commands_must_match_their_exact_per_detector_pattern() {
    let fx = fixture();
    let victim = fx.home.join("Library/Caches/b");
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("tool.docker", ["docker", "system", "prune", "-a", "-f", "--volumes"].map(String::from).to_vec()),
        ("tool.docker", ["git", "-C", &victim.to_string_lossy(), "worktree", "prune"].map(String::from).to_vec()),
        ("dev.wt-cmd", ["git", "--version"].map(String::from).to_vec()),
        ("dev.wt-cmd", ["git", "-C", "relative", "worktree", "prune"].map(String::from).to_vec()),
        ("dev.wt-cmd", ["docker", "system", "prune", "-f"].map(String::from).to_vec()),
        ("tool.cmd", ["xcrun", "simctl", "runtime", "delete", "all"].map(String::from).to_vec()),
    ];
    let findings: Vec<Finding> = cases
        .iter()
        .enumerate()
        .map(|(i, (rule_id, argv))| {
            let mut f = finding(rule_id, None, 1, ActionKind::Command);
            f.id = format!("{rule_id}:{i}");
            f.command = Some(argv.clone());
            f
        })
        .collect();
    let rep = report(findings.clone());
    let refs: Vec<&Finding> = findings.iter().collect();
    let res = run(&fx.env, &rep, &req(&refs, CleanMode::Delete, false), None);
    for f in &findings {
        let i = item(&res, f);
        assert_eq!(i.outcome, CleanOutcome::Skipped, "{:?}: {i:?}", f.command);
        assert!(msg(i).contains("does not match an allowed pattern"), "{:?}: {}", f.command, msg(i));
    }
    assert!(victim.join("blob").exists());
    // The pure plan (no rules at hand) still refuses everything that is no known pattern at all.
    let plan = plan(&rep, &req(&refs, CleanMode::Delete, true)).unwrap();
    let planned: Vec<&Option<Vec<String>>> = plan.items.iter().map(|f| &f.command).collect();
    assert_eq!(planned.len(), 2, "only the two well-formed patterns pass without rules: {planned:?}");
}

// ---------------------------------------------------------------------------------------------
// git worktrees

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Proc::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `~/Developer/repo` with one commit (also on a fake `origin/main` remote-tracking ref, so it
/// counts as pushed) and a linked worktree at `.claude/worktrees/wt`.
fn repo_with_worktree(home: &Path) -> (PathBuf, PathBuf) {
    let repo = home.join("Developer/repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("README"), "hi").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let wt = repo.join(".claude/worktrees/wt");
    git(&repo, &["worktree", "add", "-q", "-b", "wt", &wt.to_string_lossy()]);
    (repo, wt)
}

/// Marks the worktree's current commit as pushed (`refs/remotes/origin/<branch>`).
fn pretend_pushed(wt: &Path, branch: &str) {
    git(wt, &["update-ref", &format!("refs/remotes/origin/{branch}"), "HEAD"]);
}

fn worktree_finding(repo: &Path, wt: &Path, bytes: u64) -> Finding {
    let wt_s = wt.to_string_lossy();
    with_command(
        finding("dev.wt", Some(wt), bytes, ActionKind::GitWorktreeRemove),
        &["git", "-C", &repo.to_string_lossy(), "worktree", "remove", &wt_s],
    )
}

/// Commits a `.gitignore` for `.env` and puts an ignored `.env` into the worktree.
fn ignored_env(repo: &Path, wt: &Path) {
    fs::write(wt.join(".gitignore"), ".env\n").unwrap();
    git(wt, &["add", ".gitignore"]);
    git(wt, &["commit", "-q", "-m", "ignore .env"]);
    pretend_pushed(wt, "wt");
    fs::write(wt.join(".env"), "SECRET=1").unwrap();
    assert!(git(wt, &["status", "--porcelain"]).is_empty(), "ignored files keep the worktree clean");
    assert!(repo.join(".git").is_dir() && wt.join(".git").is_file());
}

#[test]
fn worktree_in_trash_mode_moves_folder_and_admin_dir_without_pruning() {
    let fx = fixture();
    let (repo, wt_path) = repo_with_worktree(&fx.home);
    ignored_env(&repo, &wt_path);
    let head = git(&wt_path, &["rev-parse", "HEAD"]);
    let wt = worktree_finding(&repo, &wt_path, 50);
    let rep = report(vec![wt.clone()]);
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();
    let admin = repo.join(".git/worktrees/wt");

    let plan = plan(&rep, &req(&[&wt], CleanMode::Trash, false)).unwrap();
    assert!(plan.warnings.iter().any(|w| w.contains("together with their git admin data")), "{plan:?}");

    let dry = run(&fx.env, &rep, &req(&[&wt], CleanMode::Trash, true), Some(&history));
    assert_done!(item(&dry, &wt));
    assert!(msg(item(&dry, &wt)).starts_with("dry run: would move to Trash with its git admin dir"), "{dry:?}");
    assert!(wt_path.exists() && admin.exists() && history.recent_actions(10).unwrap().is_empty());

    let res = run(&fx.env, &rep, &req(&[&wt], CleanMode::Trash, false), Some(&history));
    assert_done!(item(&res, &wt), "{:?}", item(&res, &wt));
    assert!(msg(item(&res, &wt)).contains("put both back to restore the worktree"), "{}", msg(item(&res, &wt)));
    assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (0, 50), "Trash frees nothing yet");
    assert!(!wt_path.exists() && !admin.exists());
    assert_eq!(fs::read_to_string(fx.trash().join("wt/.env")).unwrap(), "SECRET=1", "ignored files recoverable");
    assert!(fx.trash().join("wt 2/HEAD").is_file(), "admin dir (HEAD, index, logs) in the Trash");
    assert!(!git(&repo, &["worktree", "list", "--porcelain"]).contains("worktrees/wt"));
    assert!(git(&repo, &["branch", "--list", "wt"]).contains("wt"), "branch (and its commits) kept");
    assert!(repo.join("README").exists(), "main checkout untouched");

    // Putting both back restores a working worktree.
    fs::rename(fx.trash().join("wt"), &wt_path).unwrap();
    fs::rename(fx.trash().join("wt 2"), &admin).unwrap();
    assert_eq!(git(&wt_path, &["rev-parse", "HEAD"]), head);
    assert!(git(&wt_path, &["status", "--porcelain"]).is_empty());
    assert!(git(&repo, &["worktree", "list", "--porcelain"]).contains("worktrees/wt"));

    let log = history.recent_actions(10).unwrap();
    assert_eq!(log.iter().map(|r| (r.mode.as_str(), r.outcome)).collect::<Vec<_>>(), [("trash", CleanOutcome::Done)]);
}

#[test]
fn worktree_in_delete_mode_runs_git_worktree_remove() {
    let fx = fixture();
    let (repo, wt_path) = repo_with_worktree(&fx.home);
    ignored_env(&repo, &wt_path);
    let wt = worktree_finding(&repo, &wt_path, 50);
    let rep = report(vec![wt.clone()]);
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();

    let plan = plan(&rep, &req(&[&wt], CleanMode::Delete, false)).unwrap();
    assert!(plan.warnings.iter().any(|w| w.contains("removed with `git worktree remove`: permanent")), "{plan:?}");

    let res = run(&fx.env, &rep, &req(&[&wt], CleanMode::Delete, false), Some(&history));
    assert_done!(item(&res, &wt), "{:?}", item(&res, &wt));
    assert_eq!(msg(item(&res, &wt)), "removed git worktree");
    assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (50, 0));
    assert!(!wt_path.exists());
    assert_eq!(fs::read_dir(fx.trash()).unwrap().count(), 0, "nothing went through the Trash");
    assert!(!git(&repo, &["worktree", "list", "--porcelain"]).contains("worktrees/wt"));
    assert!(git(&repo, &["branch", "--list", "wt"]).contains("wt"), "branch (and its commits) kept");

    let log = history.recent_actions(10).unwrap();
    assert_eq!(
        log.iter().map(|r| (r.mode.as_str(), r.outcome)).collect::<Vec<_>>(),
        [("git-worktree-remove", CleanOutcome::Done)]
    );
}

#[test]
fn worktree_that_became_dirty_is_refused_in_both_modes() {
    let fx = fixture();
    let (repo, wt_path) = repo_with_worktree(&fx.home);
    let wt = worktree_finding(&repo, &wt_path, 50);
    let rep = report(vec![wt.clone()]);
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();

    // Dirty after the scan: an untracked file and a modified tracked file.
    fs::write(wt_path.join("new.txt"), "work in progress").unwrap();
    fs::write(wt_path.join("README"), "changed").unwrap();
    for mode in [CleanMode::Trash, CleanMode::Delete] {
        let res = run(&fx.env, &rep, &req(&[&wt], mode, false), Some(&history));
        assert_eq!(item(&res, &wt).outcome, CleanOutcome::Skipped);
        assert!(msg(item(&res, &wt)).contains("uncommitted changes (2 path(s))"), "{}", msg(item(&res, &wt)));
        assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (0, 0));
    }
    assert!(wt_path.join("new.txt").exists() && git(&repo, &["worktree", "list"]).contains("worktrees/wt"));
    assert_eq!(fs::read_dir(fx.trash()).unwrap().count(), 0);

    let log = history.recent_actions(10).unwrap();
    assert_eq!(
        log.iter().map(|r| (r.mode.as_str(), r.outcome)).collect::<Vec<_>>(),
        [("git-worktree-remove", CleanOutcome::Skipped), ("trash", CleanOutcome::Skipped)]
    );
}

#[test]
fn guard_allows_a_worktree_git_file_but_not_a_git_directory() {
    let fx = fixture();
    let (repo, wt_path) = repo_with_worktree(&fx.home);
    assert!(wt_path.join(".git").is_file());
    assert!(
        guard::check_worktree_target_with(&wt_path, &fx.home, &[]).is_ok(),
        "a worktree's own .git file is a pointer, not a repo"
    );
    let err = guard::check_target(&wt_path, &fx.home).unwrap_err().to_string();
    assert!(err.contains("contains a .git file"), "plain trash/delete targets may hold no .git entry: {err}");
    let err = guard::check_target(&repo, &fx.home).unwrap_err().to_string();
    assert!(err.contains("contains a git repository") || err.contains("contains a .git file"), "{err}");

    // A worktree that holds a repository of its own (a .git directory, ignored so the worktree is
    // clean) is refused in both modes.
    fs::write(wt_path.join(".gitignore"), "vendor/\n").unwrap();
    git(&wt_path, &["add", ".gitignore"]);
    git(&wt_path, &["commit", "-q", "-m", "ignore vendor"]);
    pretend_pushed(&wt_path, "wt");
    let nested = wt_path.join("vendor/lib");
    fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    let wt = worktree_finding(&repo, &wt_path, 50);
    let rep = report(vec![wt.clone()]);
    for mode in [CleanMode::Trash, CleanMode::Delete] {
        let res = run(&fx.env, &rep, &req(&[&wt], mode, false), None);
        assert_failed!(item(&res, &wt));
        assert!(msg(item(&res, &wt)).contains("contains a git repository"), "{}", msg(item(&res, &wt)));
    }
    assert!(nested.join(".git").is_dir() && wt_path.join("README").exists());
    assert_eq!(fs::read_dir(fx.trash()).unwrap().count(), 0);

    // A finding that points at a full clone (its .git is a directory) is never trashed either.
    let clone = fx.home.join("Developer/clone");
    fs::create_dir_all(&clone).unwrap();
    git(&clone, &["init", "-q"]);
    let fake = worktree_finding(&repo, &clone, 50);
    let rep = report(vec![fake.clone()]);
    let res = run(&fx.env, &rep, &req(&[&fake], CleanMode::Trash, false), None);
    assert_failed!(item(&res, &fake));
    assert!(clone.join(".git").is_dir());
}

#[test]
fn worktree_command_must_match_the_finding() {
    let fx = fixture();
    let (repo, wt_path) = repo_with_worktree(&fx.home);
    let repo_s = repo.to_string_lossy().into_owned();
    let other = fx.home.join("Developer/repo/README");
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "other path",
            vec![
                "git".into(),
                "-C".into(),
                repo_s.clone(),
                "worktree".into(),
                "remove".into(),
                other.to_string_lossy().into(),
            ],
        ),
        (
            "force",
            vec![
                "git".into(),
                "-C".into(),
                repo_s.clone(),
                "worktree".into(),
                "remove".into(),
                "--force".into(),
                wt_path.to_string_lossy().into(),
            ],
        ),
        (
            "different subcommand",
            vec![
                "git".into(),
                "-C".into(),
                repo_s.clone(),
                "clean".into(),
                "-fdx".into(),
                wt_path.to_string_lossy().into(),
            ],
        ),
    ];
    for (name, argv) in cases {
        let mut wt = finding("dev.wt", Some(&wt_path), 50, ActionKind::GitWorktreeRemove);
        wt.command = Some(argv);
        let rep = report(vec![wt.clone()]);
        let res = run(&fx.env, &rep, &req(&[&wt], CleanMode::Trash, false), None);
        let i = item(&res, &wt);
        if name == "other path" {
            assert_failed!(i, "{name}: {i:?}");
            assert!(msg(i).contains("worktree command targets"), "{name}: {}", msg(i));
        } else {
            assert_eq!(i.outcome, CleanOutcome::Skipped, "{name}: {i:?}");
            assert!(msg(i).contains("does not match an allowed pattern"), "{name}: {}", msg(i));
        }
    }
    assert!(wt_path.join("README").exists());
}

// ---------------------------------------------------------------------------------------------
// fresh scan, audit log (safety fixes 1 and 5)

impl Fx {
    fn config(&self) -> Config {
        Config { min_finding_bytes: 0, data_dir: self.tmp.path().join("data"), ..Config::default() }
    }

    /// A real scan of the `cache.test` rule, as a stored report.
    fn stored(&self) -> ScanReport {
        let opts = crate::model::ScanOptions { rule_ids: Some(vec!["cache.test".into()]), categories: None };
        crate::scan::scan_at(&self.home, &self.env.rules, &opts, &self.config(), &|_| {})
    }
}

/// The finding whose path ends in `name`.
fn by_label<'a>(report: &'a ScanReport, name: &str) -> &'a Finding {
    report
        .findings
        .iter()
        .find(|f| f.path.as_deref().is_some_and(|p| p.ends_with(&format!("/{name}"))))
        .unwrap_or_else(|| panic!("no {name}: {report:?}"))
}

#[test]
fn a_real_clean_without_the_audit_log_is_refused() {
    let fx = fixture();
    let stored = fx.stored();
    let c = by_label(&stored, "c.bin").clone();
    let seen = RefCell::new(0);
    let res =
        execute_fresh_with(&fx.env, &fx.config(), &stored, &req(&[&c, &c], CleanMode::Delete, false), None, &|_| {
            *seen.borrow_mut() += 1
        });
    assert_eq!(res.items.len(), 1, "{:?}", res.items);
    assert_eq!(*seen.borrow(), 1);
    assert_eq!(res.items[0].outcome, CleanOutcome::Skipped);
    assert_eq!(msg(&res.items[0]), format!("skipped: {NO_AUDIT_LOG}"));
    assert!(fx.home.join("Library/Caches/c.bin").exists());

    // A dry run needs no audit log.
    let dry = execute_fresh_with(&fx.env, &fx.config(), &stored, &req(&[&c], CleanMode::Delete, true), None, &|_| {});
    assert_done!(dry.items[0], "{:?}", dry.items);
    assert!(fx.home.join("Library/Caches/c.bin").exists());
}

#[test]
fn a_failed_audit_write_keeps_the_item_done_and_says_so() {
    let fx = fixture();
    let stored = fx.stored();
    let c = by_label(&stored, "c.bin").clone();
    let db = fx.tmp.path().join("data/history.sqlite");
    let history = History::open(&db).unwrap();
    rusqlite::Connection::open(&db).unwrap().execute("DROP TABLE actions", []).unwrap();
    let res = execute_fresh_with(
        &fx.env,
        &fx.config(),
        &stored,
        &req(&[&c], CleanMode::Delete, false),
        Some(&history),
        &|_| {},
    );
    assert_done!(res.items[0], "{:?}", res.items);
    assert!(msg(&res.items[0]).starts_with("deleted permanently (audit log write failed: "), "{}", msg(&res.items[0]));
    assert!(!fx.home.join("Library/Caches/c.bin").exists());
}

#[test]
fn fresh_execution_acts_only_on_findings_that_still_match() {
    let fx = fixture();
    let mut stored = fx.stored();
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();
    // As the user saw them: `a` unchanged, `b` with another action, `c.bin` gone since, plus an
    // id the stored report never had.
    stored.findings.iter_mut().find(|f| f.path.as_deref().unwrap().ends_with("/b")).unwrap().action =
        ActionKind::Delete;
    let (a, b, c) =
        (by_label(&stored, "a").clone(), by_label(&stored, "b").clone(), by_label(&stored, "c.bin").clone());
    fs::remove_file(fx.home.join("Library/Caches/c.bin")).unwrap();
    let mut r = req(&[&a, &b, &c], CleanMode::Delete, false);
    r.finding_ids.push("cache.test:/nope".into());

    let res = execute_fresh_with(&fx.env, &fx.config(), &stored, &r, Some(&history), &|_| {});
    assert_done!(item(&res, &a), "{:?}", res.items);
    assert_eq!(item(&res, &b).outcome, CleanOutcome::Skipped);
    assert_eq!(msg(item(&res, &b)), "skipped: no longer matches the scan (its action changed from delete to trash)");
    assert_eq!(msg(item(&res, &c)), "skipped: no longer matches the scan (a new scan no longer finds it)");
    let unknown = res.items.iter().find(|i| i.finding_id == "cache.test:/nope").unwrap();
    assert_eq!((unknown.outcome, msg(unknown)), (CleanOutcome::Skipped, "skipped: unknown finding id"));
    assert!(!fx.home.join("Library/Caches/a").exists() && fx.home.join("Library/Caches/b/blob").exists());
    assert_eq!(res.freed_bytes, item(&res, &a).bytes);

    // The plan says the same before anything happens.
    let mut stored = fx.stored();
    stored.findings.iter_mut().find(|f| f.path.as_deref().unwrap().ends_with("/b")).unwrap().command =
        Some(vec!["git".into()]);
    let b = by_label(&stored, "b").clone();
    let plan = plan_fresh_with(&fx.env, &fx.config(), &stored, &req(&[&b], CleanMode::Trash, true)).unwrap();
    assert!(plan.items.is_empty());
    assert_eq!(plan.warnings, [format!("Skipped {}: no longer matches the scan (its command changed to ``)", b.label)]);
    let err = plan_fresh_with(
        &fx.env,
        &fx.config(),
        &stored,
        &CleanRequest { finding_ids: vec!["cache.test:/nope".into()], mode: CleanMode::Trash, dry_run: true },
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)), "{err}");
}

#[test]
fn fresh_blockers_skip_the_item() {
    let mut fx = fixture();
    fx.env.rules.push(rule("app.vm2", &["~/vm"], ActionKind::Trash, &["FakeApp"]));
    let opts = crate::model::ScanOptions { rule_ids: Some(vec!["app.vm2".into()]), categories: None };
    let stored = crate::scan::scan_at(&fx.home, &fx.env.rules, &opts, &fx.config(), &|_| {});
    let vm = stored.findings[0].clone();
    assert!(vm.blockers.is_empty());
    fx.env.processes = Box::new(|| ProcessSnapshot::from_names(["FakeApp"]));
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();
    let res = execute_fresh_with(
        &fx.env,
        &fx.config(),
        &stored,
        &req(&[&vm], CleanMode::Delete, false),
        Some(&history),
        &|_| {},
    );
    assert_eq!(res.items[0].outcome, CleanOutcome::Skipped);
    assert_eq!(msg(&res.items[0]), "skipped: no longer matches the scan (now blocked: FakeApp is running)");
    assert!(fx.home.join("vm/bundle/disk.img").exists());
}

/// Moves a temp file to the real Trash via NSFileManager. Ignored by default because it modifies
/// `~/.Trash`; run with `cargo test -p cmm-core trash_moves_item -- --ignored`.
#[test]
#[ignore = "modifies the real ~/.Trash"]
fn trash_moves_item() {
    let mut fx = fixture();
    fx.env.trash = Box::new(move_to_trash);
    let c = finding("cache.test", Some(&fx.home.join("Library/Caches/c.bin")), 100, ActionKind::Delete);
    let rep = report(vec![c.clone()]);
    let res = run(&fx.env, &rep, &req(&[&c], CleanMode::Trash, false), None);
    assert_done!(item(&res, &c), "{:?}", item(&res, &c));
    assert_eq!(msg(item(&res, &c)), "moved to Trash", "Delete-action finding still goes to Trash in Trash mode");
    assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (0, 100));
    assert!(!fx.home.join("Library/Caches/c.bin").exists());
}
