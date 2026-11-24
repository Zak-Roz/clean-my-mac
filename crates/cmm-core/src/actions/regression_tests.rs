//! Regression tests for the cleaning safeguards (ARCHITECTURE.md §10, "Cleaning safeguards").
//! Every scenario here is one that loses data without them.
//!
//! Real `git` repositories in a tempdir "home" (global/system git config ignored), the catalog's
//! own `dev.git-worktrees` and `downloads.redundant-archives` rules, and a fake Trash folder in
//! the tempdir. Nothing touches the real home or `~/.Trash`.
//!
//! "Fresh" tests go through [`execute_fresh_with`] with a report scanned *before* the situation
//! changed (what the app's stored `last-report.json` is); "executor" tests call [`execute_with`]
//! directly with that stale finding, to prove the execute-time re-checks hold on their own.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as Proc;

use super::*;
use crate::model::{CleanOutcome, ScanOptions};

fn git_ok(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Proc::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(["-c", "protocol.file.allow=always", "-c", "init.defaultBranch=main"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text.trim().to_string())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (ok, text) = git_ok(dir, args);
    assert!(ok, "git {args:?} in {}: {text}", dir.display());
    text
}

struct Fx {
    _tmp: tempfile::TempDir,
    /// Canonical tempdir: holds `home`, `Trash`, `data` and "external disks".
    base: PathBuf,
    home: PathBuf,
    env: ExecuteEnv,
    config: Config,
    history: History,
}

impl Fx {
    fn trash(&self) -> PathBuf {
        self.base.join("Trash")
    }

    /// A full scan of the fixture's rules, as the app would have stored it.
    fn scan(&self) -> ScanReport {
        let report = crate::scan::scan_at(&self.home, &self.env.rules, &ScanOptions::default(), &self.config, &|_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        report
    }

    fn fresh(&self, stored: &ScanReport, ids: &[&str], mode: CleanMode) -> CleanResult {
        let req = CleanRequest { finding_ids: ids.iter().map(|s| s.to_string()).collect(), mode, dry_run: false };
        execute_fresh_with(&self.env, &self.config, stored, &req, Some(&self.history), &|_| {})
    }

    fn executor(&self, stored: &ScanReport, ids: &[&str], mode: CleanMode) -> CleanResult {
        let req = CleanRequest { finding_ids: ids.iter().map(|s| s.to_string()).collect(), mode, dry_run: false };
        execute_with(&self.env, stored, &req, Some(&self.history), &|_| {})
    }

    /// `~/Developer/repo` with one commit that is also on a (fake) `origin/main`.
    fn repo(&self) -> PathBuf {
        let repo = self.home.join("Developer/repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("README"), "hi").unwrap();
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        repo
    }
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("home");
    for rel in crate::guard::PROTECTED_RELATIVE {
        fs::create_dir_all(home.join(rel)).unwrap();
    }
    let rules: Vec<RuleDef> = crate::rules::builtin_rules()
        .unwrap()
        .into_iter()
        .filter(|r| r.id == "dev.git-worktrees" || r.id == "downloads.redundant-archives")
        .collect();
    assert_eq!(rules.len(), 2);
    let trash = base.join("Trash");
    fs::create_dir_all(&trash).unwrap();
    let env = ExecuteEnv {
        home: home.clone(),
        rules,
        extra_protected: vec![],
        processes: Box::new(|| ProcessSnapshot::from_names(Vec::<String>::new())),
        // Like Finder: a name clash gets " 2", " 3", …
        trash: Box::new(move |p: &Path| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let dest = (1..)
                .map(|n| if n == 1 { trash.join(&name) } else { trash.join(format!("{name} {n}")) })
                .find(|d| d.symlink_metadata().is_err())
                .unwrap();
            fs::rename(p, dest).map_err(|e| format!("fake trash: {e}"))
        }),
    };
    let config = Config { min_finding_bytes: 0, data_dir: base.join("data"), ..Config::default() };
    let history = History::open(&base.join("data/history.sqlite")).unwrap();
    Fx { _tmp: tmp, base, home, env, config, history }
}

fn finding_at<'a>(report: &'a ScanReport, path: &Path) -> &'a Finding {
    let p = path.to_string_lossy();
    report
        .findings
        .iter()
        .find(|f| f.path.as_deref() == Some(&*p))
        .unwrap_or_else(|| panic!("no finding for {p}: {:#?}", report.findings))
}

fn only(result: &CleanResult) -> &CleanItemResult {
    assert_eq!(result.items.len(), 1, "{:?}", result.items);
    &result.items[0]
}

fn not_done(result: &CleanResult) {
    let item = only(result);
    assert_ne!(item.outcome, CleanOutcome::Done, "{item:?}");
    assert_eq!((result.freed_bytes, result.moved_to_trash_bytes), (0, 0));
}

fn wt_finding(repo: &Path, wt: &Path) -> Finding {
    let wt_s = wt.to_string_lossy().into_owned();
    Finding {
        id: format!("dev.git-worktrees:{wt_s}"),
        rule_id: "dev.git-worktrees".into(),
        category: crate::model::Category::Worktrees,
        risk: crate::model::Risk::Review,
        label: wt.file_name().unwrap().to_string_lossy().into_owned(),
        path: Some(wt_s.clone()),
        bytes: 1,
        logical_bytes: 1,
        files: 1,
        is_dir: true,
        last_used: None,
        reason: "scan-time".into(),
        action: ActionKind::GitWorktreeRemove,
        command: Some(vec![
            "git".into(),
            "-C".into(),
            repo.to_string_lossy().into(),
            "worktree".into(),
            "remove".into(),
            wt_s,
        ]),
        selected_by_default: false,
        blockers: vec![],
        group: None,
    }
}

fn report_of(findings: Vec<Finding>) -> ScanReport {
    let started_at = chrono::Utc::now();
    ScanReport {
        id: started_at.to_rfc3339(),
        started_at,
        duration_ms: 0,
        volume: crate::model::VolumeInfo {
            total_bytes: 0,
            free_bytes: 0,
            available_important_bytes: None,
            swap_used_bytes: None,
            trash_bytes: None,
        },
        full_disk_access: true,
        findings,
        errors: vec![],
    }
}

// ---------------------------------------------------------------------------------------------
// A: stale report — an orphan folder that became a live worktree; an archive whose copy is gone

/// Scan: `.claude/worktrees/feature` is a leftover folder (plain `trash` finding). Later the same
/// path becomes a real worktree with uncommitted work. Delete mode must not `remove_dir_all` it.
fn orphan_then_live_worktree(fx: &Fx) -> (ScanReport, PathBuf) {
    let repo = fx.repo();
    let wt = repo.join(".claude/worktrees/feature");
    fs::create_dir_all(&wt).unwrap();
    fs::write(wt.join("scratch.txt"), "leftover").unwrap();
    let stored = fx.scan();
    let f = finding_at(&stored, &wt);
    assert_eq!((f.action, f.blockers.len()), (ActionKind::Trash, 0), "an orphan at scan time: {f:?}");

    fs::remove_dir_all(&wt).unwrap();
    git(&repo, &["worktree", "add", "-q", "-b", "feature", &wt.to_string_lossy()]);
    fs::write(wt.join("work-in-progress.txt"), "3 hours of work").unwrap();
    fs::write(wt.join("README"), "edited").unwrap();
    (stored, wt)
}

#[test]
fn a_fresh_stale_orphan_that_became_a_live_worktree_is_kept() {
    let fx = fixture();
    let (stored, wt) = orphan_then_live_worktree(&fx);
    let id = finding_at(&stored, &wt).id.clone();
    let res = fx.fresh(&stored, &[&id], CleanMode::Delete);
    not_done(&res);
    let item = only(&res);
    assert_eq!(item.outcome, CleanOutcome::Skipped);
    assert!(item.message.as_deref().unwrap().contains(NO_LONGER_MATCHES), "{item:?}");
    assert_eq!(fs::read_to_string(wt.join("work-in-progress.txt")).unwrap(), "3 hours of work");
    assert_eq!(fs::read_to_string(wt.join("README")).unwrap(), "edited");
}

#[test]
fn a_executor_refuses_a_trash_target_that_now_holds_a_git_file() {
    let fx = fixture();
    let (stored, wt) = orphan_then_live_worktree(&fx);
    let id = finding_at(&stored, &wt).id.clone();
    for mode in [CleanMode::Delete, CleanMode::Trash] {
        let res = fx.executor(&stored, &[&id], mode);
        not_done(&res);
        assert!(only(&res).message.as_deref().unwrap().contains(".git"), "{:?}", res.items);
    }
    assert!(wt.join("work-in-progress.txt").exists() && wt.join(".git").is_file());
}

#[test]
fn a_fresh_archive_whose_extracted_copy_was_deleted_is_kept() {
    let fx = fixture();
    let zip = fx.home.join("Downloads/photos.zip");
    fs::write(&zip, "the only copy of the photos, soon").unwrap();
    fs::create_dir_all(fx.home.join("Downloads/photos")).unwrap();
    fs::write(fx.home.join("Downloads/photos/img.jpg"), "jpeg").unwrap();
    let stored = fx.scan();
    let id = finding_at(&stored, &zip).id.clone();

    // After the scan the extracted folder is deleted: the archive is the only copy now.
    fs::remove_dir_all(fx.home.join("Downloads/photos")).unwrap();
    for mode in [CleanMode::Delete, CleanMode::Trash] {
        let res = fx.fresh(&stored, &[&id], mode);
        not_done(&res);
        assert!(only(&res).message.as_deref().unwrap().contains(NO_LONGER_MATCHES), "{:?}", res.items);
    }
    assert_eq!(fs::read_to_string(&zip).unwrap(), "the only copy of the photos, soon");
}

// ---------------------------------------------------------------------------------------------
// B: Trash mode on a worktree — detached commits, other worktrees, restore from the Trash

#[test]
fn b_detached_commit_made_after_the_scan_blocks_trash_mode() {
    let fx = fixture();
    let repo = fx.repo();
    let wt = repo.join(".claude/worktrees/wt1");
    git(&repo, &["worktree", "add", "-q", "--detach", &wt.to_string_lossy()]);
    let stored = fx.scan();
    let f = finding_at(&stored, &wt);
    assert!(f.blockers.is_empty() && f.action == ActionKind::GitWorktreeRemove, "{f:?}");
    let id = f.id.clone();

    // After the scan: work committed on the detached HEAD (reachable only from this worktree).
    git(&wt, &["commit", "-q", "--allow-empty", "-m", "detached work after scan"]);
    let commit = git(&wt, &["rev-parse", "HEAD"]);

    let res = fx.executor(&stored, &[&id], CleanMode::Trash);
    not_done(&res);
    assert!(only(&res).message.as_deref().unwrap().contains("not on any remote branch"), "{:?}", res.items);
    let res = fx.fresh(&stored, &[&id], CleanMode::Trash);
    not_done(&res);
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), commit, "the worktree and its HEAD are intact");
    assert!(git(&repo, &["worktree", "list"]).contains("wt1"));
}

#[test]
fn b_trash_mode_never_prunes_another_worktree_whose_disk_is_unmounted() {
    let fx = fixture();
    let repo = fx.repo();
    let wt1 = repo.join(".claude/worktrees/wt1");
    git(&repo, &["worktree", "add", "-q", "--detach", &wt1.to_string_lossy()]);
    // Another worktree on an external disk (a folder outside home) with staged work.
    let disk = fx.base.join("ExternalDisk");
    fs::create_dir_all(&disk).unwrap();
    let wt2 = disk.join("wt2");
    git(&repo, &["worktree", "add", "-q", "-b", "b2", &wt2.to_string_lossy()]);
    fs::write(wt2.join("staged.txt"), "staged on the other worktree").unwrap();
    git(&wt2, &["add", "staged.txt"]);
    let stored = fx.scan();
    let id = finding_at(&stored, &wt1).id.clone();

    // The disk is ejected while wt1 is cleaned.
    fs::rename(&disk, fx.base.join("ExternalDisk-ejected")).unwrap();
    let res = fx.fresh(&stored, &[&id], CleanMode::Trash);
    assert_eq!(only(&res).outcome, CleanOutcome::Done, "{:?}", res.items);
    fs::rename(fx.base.join("ExternalDisk-ejected"), &disk).unwrap();

    let (ok, status) = git_ok(&wt2, &["status", "--porcelain"]);
    assert!(ok, "the other worktree still works after re-mounting: {status}");
    assert_eq!(status, "A  staged.txt");
    assert!(git(&repo, &["worktree", "list"]).contains("wt2"));
    assert!(repo.join(".git/worktrees/wt2/index").is_file());
}

#[test]
fn b_a_worktree_trashed_with_its_admin_dir_can_be_restored() {
    let fx = fixture();
    let repo = fx.repo();
    let wt = repo.join(".claude/worktrees/wt1");
    git(&repo, &["worktree", "add", "-q", "-b", "wt1", &wt.to_string_lossy()]);
    fs::write(wt.join(".env"), "SECRET=1").unwrap();
    fs::write(wt.join(".gitignore"), ".env\n").unwrap();
    git(&wt, &["add", ".gitignore"]);
    git(&wt, &["commit", "-q", "-m", "ignore .env"]);
    git(&wt, &["update-ref", "refs/remotes/origin/wt1", "HEAD"]);
    let head = git(&wt, &["rev-parse", "HEAD"]);
    let stored = fx.scan();
    let id = finding_at(&stored, &wt).id.clone();

    let res = fx.fresh(&stored, &[&id], CleanMode::Trash);
    assert_eq!(only(&res).outcome, CleanOutcome::Done, "{:?}", res.items);
    let admin = repo.join(".git/worktrees/wt1");
    assert!(!wt.exists() && !admin.exists());

    // Put both back from the Trash: a working worktree again.
    fs::rename(fx.trash().join("wt1"), &wt).unwrap();
    fs::rename(fx.trash().join("wt1 2"), &admin).unwrap();
    let (ok, status) = git_ok(&wt, &["status", "--porcelain"]);
    assert!(ok && status.is_empty(), "restored worktree is clean and usable: {status}");
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read_to_string(wt.join(".env")).unwrap(), "SECRET=1");
    assert!(git(&repo, &["worktree", "list"]).contains("wt1"));
}

// ---------------------------------------------------------------------------------------------
// C: a worktree with an initialized submodule (its git data lives in the worktree's admin dir)

#[test]
fn c_worktree_with_an_initialized_submodule_is_refused_in_both_modes() {
    let fx = fixture();
    let lib = fx.base.join("lib-origin");
    fs::create_dir_all(&lib).unwrap();
    git(&lib, &["init", "-q", "-b", "main"]);
    git(&lib, &["commit", "-q", "--allow-empty", "-m", "lib0"]);
    let repo = fx.repo();
    git(&repo, &["submodule", "add", "-q", &lib.to_string_lossy(), "lib"]);
    git(&repo, &["commit", "-q", "-m", "add lib"]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let wt = repo.join(".claude/worktrees/wt");
    git(&repo, &["worktree", "add", "-q", "-b", "wt", &wt.to_string_lossy()]);
    git(&wt, &["submodule", "update", "--init", "-q"]);
    git(&wt.join("lib"), &["commit", "-q", "--allow-empty", "-m", "unpushed submodule work"]);
    let commit = git(&wt.join("lib"), &["rev-parse", "HEAD"]);
    git(&wt, &["add", "lib"]);
    git(&wt, &["commit", "-q", "-m", "bump lib"]);
    git(&wt, &["update-ref", "refs/remotes/origin/wt", "HEAD"]);
    let (ok, status) = git_ok(&wt, &["status", "--porcelain", "--untracked-files=all", "--ignore-submodules=none"]);
    assert!(ok && status.is_empty(), "clean by every status flag: {status}");

    let stored = report_of(vec![wt_finding(&repo, &wt)]);
    let id = stored.findings[0].id.clone();
    for mode in [CleanMode::Trash, CleanMode::Delete] {
        let res = fx.executor(&stored, &[&id], mode);
        not_done(&res);
        assert!(only(&res).message.as_deref().unwrap().contains("submodules"), "{:?}", res.items);
    }
    let modules = repo.join(".git/worktrees/wt/modules/lib");
    assert!(modules.is_dir() && wt.join("lib").is_dir());
    assert!(git_ok(&modules, &["cat-file", "-e", &commit]).0, "the unpushed submodule commit survives");
}

// ---------------------------------------------------------------------------------------------
// D: `status.showUntrackedFiles=no` must not hide untracked files from the re-check

#[test]
fn d_untracked_files_hidden_by_config_block_worktree_removal() {
    let fx = fixture();
    let repo = fx.repo();
    git(&repo, &["config", "status.showUntrackedFiles", "no"]);
    let wt = repo.join(".claude/worktrees/wt");
    git(&repo, &["worktree", "add", "-q", "-b", "wt", &wt.to_string_lossy()]);
    let stored = fx.scan();
    let f = finding_at(&stored, &wt);
    assert!(f.blockers.is_empty(), "{f:?}");
    let id = f.id.clone();

    // After the scan: untracked notes the user's config hides from `git status`.
    fs::write(wt.join("notes.md"), "untracked notes").unwrap();
    for mode in [CleanMode::Delete, CleanMode::Trash] {
        let res = fx.executor(&stored, &[&id], mode);
        not_done(&res);
        assert!(only(&res).message.as_deref().unwrap().contains("uncommitted changes"), "{:?}", res.items);
        let res = fx.fresh(&stored, &[&id], mode);
        not_done(&res);
    }
    assert_eq!(fs::read_to_string(wt.join("notes.md")).unwrap(), "untracked notes");
    assert!(git(&repo, &["worktree", "list"]).contains("worktrees/wt"));
}

// ---------------------------------------------------------------------------------------------
// E: a repository-wide `git worktree prune` (safeguard 3)

/// Scan: one stale entry (its folder deleted by hand) and a worktree on an external disk with a
/// detached commit and staged work. The disk is ejected after the user picked the stale-entries
/// finding, so a fresh `git worktree prune` would also drop the external worktree's admin dir: its
/// index and the only ref to its commit. Nothing may run a prune — not the fresh path, not the
/// executor, and not a hand-crafted `Command` finding with the old argv (an older
/// `last-report.json`).
#[test]
fn e_no_repo_wide_prune_runs_even_after_another_worktree_disk_is_ejected() {
    let fx = fixture();
    let repo = fx.repo();
    let repo_s = repo.to_string_lossy().into_owned();
    let gone = repo.join(".claude/worktrees/gone");
    git(&repo, &["worktree", "add", "-q", "-b", "gone", &gone.to_string_lossy()]);
    fs::remove_dir_all(&gone).unwrap();
    let disk = fx.base.join("ExternalDisk");
    fs::create_dir_all(&disk).unwrap();
    let wt2 = disk.join("wt2");
    git(&repo, &["worktree", "add", "-q", "--detach", &wt2.to_string_lossy()]);
    git(&wt2, &["commit", "-q", "--allow-empty", "-m", "detached work on the external disk"]);
    let commit = git(&wt2, &["rev-parse", "HEAD"]);
    fs::write(wt2.join("staged.txt"), "staged on the external disk").unwrap();
    git(&wt2, &["add", "staged.txt"]);

    let stored = fx.scan();
    let stale = stored
        .findings
        .iter()
        .find(|f| f.id.ends_with("#prune"))
        .unwrap_or_else(|| panic!("no stale-entries finding: {:#?}", stored.findings))
        .clone();
    let mut crafted = stale.clone();
    crafted.action = ActionKind::Command;
    crafted.command = Some(["git", "-C", repo_s.as_str(), "worktree", "prune"].map(String::from).to_vec());
    let crafted = report_of(vec![crafted]);

    fs::rename(&disk, fx.base.join("ExternalDisk-ejected")).unwrap();
    for mode in [CleanMode::Trash, CleanMode::Delete] {
        for report in [&stored, &crafted] {
            not_done(&fx.fresh(report, &[&stale.id], mode));
            not_done(&fx.executor(report, &[&stale.id], mode));
            let req = CleanRequest { finding_ids: vec![stale.id.clone()], mode, dry_run: true };
            assert!(plan_fresh_with(&fx.env, &fx.config, report, &req).unwrap().items.is_empty());
            assert!(plan(report, &req).unwrap().items.is_empty());
        }
    }
    fs::rename(fx.base.join("ExternalDisk-ejected"), &disk).unwrap();

    // The external worktree works again once its disk is back, with its staged file and commit.
    let (ok, status) = git_ok(&wt2, &["status", "--porcelain"]);
    assert!(ok, "the external worktree still works after re-mounting: {status}");
    assert_eq!(status, "A  staged.txt");
    assert_eq!(git(&wt2, &["rev-parse", "HEAD"]), commit);
    assert!(repo.join(".git/worktrees/wt2/index").is_file());
    let listed = git(&repo, &["worktree", "list", "--porcelain"]);
    assert!(listed.contains(&*wt2.to_string_lossy()) && listed.contains(&*gone.to_string_lossy()), "{listed}");

    // The finding only informs: it lists the stale entry and the command to run by hand.
    assert_eq!(stale.action, ActionKind::InfoOnly, "{stale:?}");
    assert!(stale.command.is_none() && !stale.selected_by_default && stale.blockers.is_empty(), "{stale:?}");
    assert!(stale.reason.contains(&*gone.to_string_lossy()), "{}", stale.reason);
    assert!(stale.reason.contains(&format!("`git -C {repo_s} worktree prune`")), "{}", stale.reason);
}
