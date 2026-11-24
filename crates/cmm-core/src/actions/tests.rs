//! Tempdir tests for plan/execute. Nothing here touches the real home directory, except the
//! `#[ignore]`d Trash test (NSFileManager moves the temp file into the real ~/.Trash).

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
    RuleDef {
        id: id.into(),
        title: id.into(),
        description: String::new(),
        category: Category::AppCaches,
        risk: Risk::Safe,
        detector: DetectorKind::Paths,
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
        rule("tool.cmd", &[], ActionKind::Command, &[]),
        rule("info.only", &["~/Library/Caches/*"], ActionKind::InfoOnly, &[]),
    ];
    let env = ExecuteEnv {
        home: home.clone(),
        rules,
        extra_protected: vec![],
        processes: Box::new(|| ProcessSnapshot::from_names(Vec::<String>::new())),
    };
    Fx { tmp, home, env }
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
    let plan = plan(&rep, &req(&[&wt], CleanMode::Trash, true)).unwrap();
    assert_eq!(plan.items.len(), 1);
    let w = plan.warnings.join("\n");
    assert!(w.contains("git worktree remove") && w.contains("not via Trash"), "{w}");
    assert!(!w.contains("Trash is emptied"), "no path items → no Trash warning: {w}");
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
            CleanMode::Trash => assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (50, 1010 + 100)),
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
    let ok = with_command(finding("tool.cmd", None, 300, ActionKind::Command), &["git", "--version"]);
    let mut bad = with_command(finding("tool.cmd", None, 200, ActionKind::Command), &["git", "no-such-subcommand"]);
    bad.id = "tool.cmd:bad".into();
    let rep = report(vec![ok.clone(), bad.clone()]);
    let res = run(&fx.env, &rep, &req(&[&ok, &bad], CleanMode::Trash, false), None);
    assert_done!(item(&res, &ok), "{:?}", item(&res, &ok));
    assert_eq!(msg(item(&res, &ok)), "ran `git --version`");
    assert_failed!(item(&res, &bad));
    assert!(msg(item(&res, &bad)).contains("not a git command"), "{}", msg(item(&res, &bad)));
    assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (300, 0), "commands free space directly");
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

/// `~/Developer/repo` with one commit and a linked worktree at `.claude/worktrees/wt`.
fn repo_with_worktree(home: &Path) -> (PathBuf, PathBuf) {
    let repo = home.join("Developer/repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("README"), "hi").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = repo.join(".claude/worktrees/wt");
    git(&repo, &["worktree", "add", "-q", "-b", "wt", &wt.to_string_lossy()]);
    (repo, wt)
}

fn worktree_finding(repo: &Path, wt: &Path, bytes: u64) -> Finding {
    let wt_s = wt.to_string_lossy();
    with_command(
        finding("dev.wt", Some(wt), bytes, ActionKind::GitWorktreeRemove),
        &["git", "-C", &repo.to_string_lossy(), "worktree", "remove", &wt_s],
    )
}

#[test]
fn worktree_that_became_dirty_is_refused_clean_one_is_removed() {
    let fx = fixture();
    let (repo, wt_path) = repo_with_worktree(&fx.home);
    let wt = worktree_finding(&repo, &wt_path, 50);
    let rep = report(vec![wt.clone()]);
    let history = History::open(&fx.tmp.path().join("data/history.sqlite")).unwrap();

    // Dirty after the scan: an untracked file and a modified tracked file.
    fs::write(wt_path.join("new.txt"), "work in progress").unwrap();
    fs::write(wt_path.join("README"), "changed").unwrap();
    let res = run(&fx.env, &rep, &req(&[&wt], CleanMode::Trash, false), Some(&history));
    assert_failed!(item(&res, &wt));
    assert!(msg(item(&res, &wt)).contains("uncommitted changes (2 path(s))"), "{}", msg(item(&res, &wt)));
    assert!(wt_path.join("new.txt").exists());

    // Clean again → removed via `git worktree remove`, counted as freed (not Trash).
    fs::remove_file(wt_path.join("new.txt")).unwrap();
    git(&wt_path, &["checkout", "-q", "--", "README"]);
    let res = run(&fx.env, &rep, &req(&[&wt], CleanMode::Trash, false), Some(&history));
    assert_done!(item(&res, &wt), "{:?}", item(&res, &wt));
    assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (50, 0));
    assert!(!wt_path.exists());
    assert!(!git(&repo, &["worktree", "list"]).contains("worktrees/wt"));
    assert!(git(&repo, &["branch", "--list", "wt"]).contains("wt"), "branch (and its commits) kept");

    let log = history.recent_actions(10).unwrap();
    assert_eq!(
        log.iter().map(|r| (r.mode.as_str(), r.outcome)).collect::<Vec<_>>(),
        [("git-worktree-remove", CleanOutcome::Done), ("git-worktree-remove", CleanOutcome::Failed)]
    );
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
        assert_failed!(item(&res, &wt), "{name}: {:?}", item(&res, &wt));
        assert!(msg(item(&res, &wt)).contains("worktree command"), "{name}: {}", msg(item(&res, &wt)));
    }
    assert!(wt_path.join("README").exists());
}

/// Moves a temp file to the real Trash via NSFileManager. Ignored by default because it modifies
/// `~/.Trash`; run with `cargo test -p cmm-core trash_moves_item -- --ignored`.
#[test]
#[ignore = "modifies the real ~/.Trash"]
fn trash_moves_item() {
    let fx = fixture();
    let c = finding("cache.test", Some(&fx.home.join("Library/Caches/c.bin")), 100, ActionKind::Delete);
    let rep = report(vec![c.clone()]);
    let res = run(&fx.env, &rep, &req(&[&c], CleanMode::Trash, false), None);
    assert_done!(item(&res, &c), "{:?}", item(&res, &c));
    assert_eq!(msg(item(&res, &c)), "moved to Trash", "Delete-action finding still goes to Trash in Trash mode");
    assert_eq!((res.freed_bytes, res.moved_to_trash_bytes), (0, 100));
    assert!(!fx.home.join("Library/Caches/c.bin").exists());
}
