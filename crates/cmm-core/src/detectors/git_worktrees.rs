//! `git-worktrees` detector — linked and orphaned git worktrees (owned by S2).
//!
//! Repositories are found under the roots (depth ≤ 4, no hidden dirs, no node_modules); a linked
//! worktree's `.git` file also leads back to its main repository. Per repository:
//! - each linked worktree → `git-worktree-remove` finding with
//!   `command = ["git", "-C", <repo>, "worktree", "remove", <path>]` (ARCHITECTURE.md §10) and
//!   blockers for: locked, initialized submodules (`modules/` in its admin dir), uncommitted or
//!   untracked changes (the executor's exact `git status` flags, whatever the user's config),
//!   commits not on any remote branch (`git rev-list HEAD --not --remotes`), outside the home folder
//!   — the same checks the executor repeats right before acting (safety fix 3);
//! - directories under `<repo>/.claude/worktrees/` that git does not know → `trash` findings
//!   (blocked when they still hold a `.git` file: the guard refuses any `.git` entry);
//! - prunable entries (directory gone) → one `info-only` finding that lists them and tells the
//!   user to run `git -C <repo> worktree prune` themselves if they are really gone. It is never
//!   executable: a repository-wide prune frees nothing and also drops the admin data (index,
//!   the only ref to detached commits) of a worktree whose disk is merely unmounted.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::actions::WORKTREE_STATUS_ARGS;
use crate::detectors::external;
use crate::detectors::support::{days_since, for_each_root, list_dir, param_u64, walk_dirs};
use crate::detectors::{Detector, make_finding};
use crate::error::CoreError;
use crate::model::{ActionKind, Finding, Risk, RuleDef};
use crate::scan::ScanContext;
use crate::size::SizeInfo;

pub struct GitWorktreesDetector;

/// One record of `git worktree list --porcelain -z`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,
}

/// Parses `git worktree list --porcelain -z` (fields NUL-terminated, records end with an empty field).
pub fn parse_worktree_list(text: &str) -> Vec<WorktreeEntry> {
    let mut out = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for field in text.split('\0') {
        if field.is_empty() {
            out.extend(current.take());
            continue;
        }
        let (key, value) = field.split_once(' ').unwrap_or((field, ""));
        if key == "worktree" {
            out.extend(current.take());
            current = Some(WorktreeEntry { path: PathBuf::from(value), ..Default::default() });
            continue;
        }
        let Some(entry) = current.as_mut() else { continue };
        match key {
            "HEAD" => entry.head = Some(value.to_string()),
            "branch" => entry.branch = Some(value.to_string()),
            "bare" => entry.bare = true,
            "detached" => entry.detached = true,
            "locked" => entry.locked = Some(value.to_string()),
            "prunable" => entry.prunable = Some(value.to_string()),
            _ => {}
        }
    }
    out.extend(current);
    out
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn git(dir: &Path, args: &[&str]) -> Result<external::Output, CoreError> {
    let dir = path_str(dir);
    let mut argv = vec!["-C", dir.as_str()];
    argv.extend_from_slice(args);
    external::run_ok("git", &argv)
}

/// Main repository of a linked worktree, from its `.git` file (`gitdir: <main>/.git/worktrees/<name>`).
fn main_repo_of_worktree(dot_git_file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git_file).ok()?;
    let gitdir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    let gitdir = if gitdir.is_absolute() { gitdir } else { dot_git_file.parent()?.join(gitdir) };
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = gitdir.join(common.trim());
    let common = std::fs::canonicalize(common).ok()?;
    (common.file_name()? == ".git").then(|| common.parent().map(Path::to_path_buf))?
}

/// Main repositories (canonical paths) under `root`, down to depth 4.
fn find_repos(root: &Path, max_depth: usize, repos: &mut BTreeSet<PathBuf>) -> Result<(), CoreError> {
    walk_dirs(root, max_depth + 1, &mut |dir, entries| {
        if let Some(dot_git) = entries.iter().find(|e| e.name == ".git") {
            if dot_git.is_dir() {
                repos.insert(canonical(dir));
            } else if dot_git.is_file()
                && let Some(main) = main_repo_of_worktree(&dot_git.path)
            {
                repos.insert(main);
            }
        }
        Vec::new()
    })
}

fn short_branch(entry: &WorktreeEntry) -> String {
    match (&entry.branch, &entry.head) {
        (Some(b), _) => b.strip_prefix("refs/heads/").unwrap_or(b).to_string(),
        (None, Some(h)) => format!("detached at {}", &h[..h.len().min(8)]),
        _ => "unknown HEAD".to_string(),
    }
}

/// The admin dir (`<repo>/.git/worktrees/<id>`) a linked worktree's `.git` file points to.
fn admin_dir_of(worktree: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(worktree.join(".git")).ok()?;
    let pointer = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let admin = PathBuf::from(pointer);
    Some(if admin.is_absolute() { admin } else { worktree.join(admin) })
}

/// Blockers for removing one linked worktree (the executor re-checks the same things).
fn blockers_for(repo: &Path, entry: &WorktreeEntry, home: &Path) -> Vec<String> {
    let mut blockers = Vec::new();
    if let Some(reason) = &entry.locked {
        blockers.push(if reason.is_empty() {
            "Worktree is locked".into()
        } else {
            format!("Worktree is locked: {reason}")
        });
    }
    if admin_dir_of(&entry.path).is_some_and(|admin| admin.join("modules").symlink_metadata().is_ok()) {
        blockers.push("Has initialized submodules (their git data lives in the worktree's admin dir)".into());
    }
    match git(&entry.path, &WORKTREE_STATUS_ARGS) {
        Ok(out) => {
            let n = out.stdout.lines().filter(|l| !l.trim().is_empty()).count();
            if n > 0 {
                let files = if n == 1 { "file" } else { "files" };
                blockers.push(format!("Uncommitted changes in worktree ({n} {files})"));
            }
        }
        Err(e) => blockers.push(format!("Could not check worktree status: {e}")),
    }
    if let Some(head) = &entry.head {
        match git(repo, &["rev-list", "--count", head.as_str(), "--not", "--remotes"])
            .map(|o| o.stdout.trim().parse::<u64>())
        {
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => blockers.push(format!("Unpushed commits ({n} not on any remote branch)")),
            Ok(Err(_)) | Err(_) => blockers.push("Could not check for unpushed commits".into()),
        }
    }
    if !canonical(&entry.path).starts_with(canonical(home)) {
        blockers.push("Outside the home folder".into());
    }
    blockers
}

fn repo_findings(rule: &RuleDef, ctx: &ScanContext, repo: &Path) -> Result<Vec<Finding>, CoreError> {
    let listing = git(repo, &["worktree", "list", "--porcelain", "-z"])?;
    let entries = parse_worktree_list(&listing.stdout);
    let repo_name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let repo_s = path_str(repo);
    let group = Some(repo_s.clone());

    let linked: Vec<&WorktreeEntry> = entries.iter().skip(1).filter(|e| !e.bare).collect();
    let prunable: Vec<&&WorktreeEntry> = linked.iter().filter(|e| e.prunable.is_some()).collect();
    let live: Vec<&&WorktreeEntry> =
        linked.iter().filter(|e| e.prunable.is_none() && e.path.symlink_metadata().is_ok()).collect();

    let mut findings: Vec<Finding> = live
        .par_iter()
        .map(|entry| {
            let size = ctx.sizer.size_of(&entry.path);
            let mut reason = format!("Git worktree of {repo_name} on {}", short_branch(entry));
            if let Some(t) = size.newest_mtime {
                reason.push_str(&format!("; last modified {} days ago", days_since(ctx.now, t)));
            }
            let wt = path_str(&entry.path);
            let mut f = make_finding(rule, &wt, Some(&entry.path), &size, reason);
            f.action = ActionKind::GitWorktreeRemove;
            f.command = Some(vec!["git".into(), "-C".into(), repo_s.clone(), "worktree".into(), "remove".into(), wt]);
            f.group = group.clone();
            for blocker in blockers_for(repo, entry, &ctx.home) {
                f.add_blocker(blocker);
            }
            f
        })
        .collect();

    // Leftover directories under .claude/worktrees that git no longer knows about.
    let registered: BTreeSet<PathBuf> = entries.iter().map(|e| canonical(&e.path)).collect();
    if let Ok(children) = list_dir(&repo.join(".claude/worktrees")) {
        for child in children.iter().filter(|c| c.is_dir() && !registered.contains(&canonical(&c.path))) {
            let dot_git = child.path.join(".git");
            let (orphaned, has_git_file) = match dot_git.symlink_metadata() {
                Err(_) => (true, false),
                Ok(m) if m.is_file() => (main_repo_of_worktree(&dot_git).is_none(), true),
                Ok(_) => (false, false), // an independent clone, not a worktree
            };
            if !orphaned {
                continue;
            }
            let size = ctx.sizer.size_of(&child.path);
            let reason = format!("Leftover worktree folder: not registered in {repo_name}'s git worktrees");
            let mut f = make_finding(rule, &path_str(&child.path), Some(&child.path), &size, reason);
            f.action = ActionKind::Trash;
            f.group = group.clone();
            if has_git_file {
                f.add_blocker("Still has a .git file (a broken worktree link): check it and remove it by hand");
            }
            findings.push(f);
        }
    }

    if !prunable.is_empty() {
        findings.push(stale_entries_finding(rule, &repo_s, &repo_name, &prunable));
    }
    Ok(findings)
}

/// At most this many stale entries are named in the reason.
const STALE_ENTRIES_SHOWN: usize = 10;

/// The stale worktree entries of one repository (their folders are gone), as an `info-only`
/// finding: never selectable, never executed (re-review follow-up after a28953e). The reason names
/// the entries and the command the user can run once they have checked them.
fn stale_entries_finding(rule: &RuleDef, repo_s: &str, repo_name: &str, prunable: &[&&WorktreeEntry]) -> Finding {
    let n = prunable.len();
    let shown: Vec<String> = prunable.iter().take(STALE_ENTRIES_SHOWN).map(|e| path_str(&e.path)).collect();
    let more = if n > shown.len() { format!(" and {} more", n - shown.len()) } else { String::new() };
    let (entries, folders, exist, these) = if n == 1 {
        ("entry", "folder", "exists", "this worktree is")
    } else {
        ("entries", "folders", "exist", "these worktrees are")
    };
    // Copy-pasteable: quote the repository path unless it is plain.
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+@,:%".contains(c);
    let repo_arg =
        if repo_s.chars().all(plain) { repo_s.to_string() } else { format!("'{}'", repo_s.replace('\'', r"'\''")) };
    let reason = format!(
        "Git still lists {n} worktree {entries} whose {folders} no longer {exist}: {}{more}. Run `git -C {repo_arg} \
         worktree prune` yourself if {these} really gone: a worktree on a disk that is only unmounted would \
         lose its git data (staged changes, detached commits).",
        shown.join(", ")
    );
    let mut f = make_finding(rule, &format!("{repo_s}#prune"), None, &SizeInfo::default(), reason);
    f.label = format!("{repo_name}: {n} stale worktree {entries}");
    f.risk = Risk::Review;
    f.selected_by_default = false;
    f.action = ActionKind::InfoOnly;
    f.command = None;
    f.group = Some(repo_s.to_string());
    f
}

impl Detector for GitWorktreesDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let depth = param_u64(rule, "depth").unwrap_or(4) as usize;
        let mut repos = BTreeSet::new();
        for_each_root(rule, ctx, |root| find_repos(root, depth, &mut repos))?;

        // A failing repository never hides the others (contract v2): each failure is warned; only
        // when every repository failed does the rule fail (with the first error).
        let results: Vec<(&PathBuf, Result<Vec<Finding>, CoreError>)> =
            repos.par_iter().map(|repo| (repo, repo_findings(rule, ctx, repo))).collect();
        let any_ok = results.iter().any(|(_, r)| r.is_ok());
        let mut findings = Vec::new();
        let mut fatal = None;
        for (repo, result) in results {
            match result {
                Ok(found) => findings.extend(found),
                Err(e) if !any_ok && fatal.is_none() => fatal = Some((repo, e)),
                Err(e) => ctx.warn(repo.to_string_lossy(), format!("{}: {e}", rule.id)),
            }
        }
        match fatal {
            Some((repo, e)) => Err(CoreError::Command(format!("{}: {e}", repo.display()))),
            None => Ok(findings),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::support::testutil::{TestEnv, file, rule};
    use crate::model::DetectorKind;

    fn git_cmd(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn parses_porcelain_z() {
        let text = "worktree /r\0HEAD aaa\0branch refs/heads/main\0\0worktree /r/.claude/worktrees/x\0HEAD bbb\0detached\0locked\0\0worktree /tmp/gone\0HEAD ccc\0branch refs/heads/g\0prunable gitdir file points to non-existent location\0\0";
        let entries = parse_worktree_list(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].branch.as_deref(), Some("refs/heads/main"));
        assert!(entries[1].detached && entries[1].locked.as_deref() == Some(""));
        assert_eq!(entries[2].prunable.as_deref(), Some("gitdir file points to non-existent location"));
        assert_eq!(short_branch(&entries[1]), "detached at bbb");
    }

    #[test]
    fn worktrees_blockers_orphans_and_stale_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let repo = home.join("Developer/acme/repo");
        let remote = home.join("remote.git");
        std::fs::create_dir_all(&repo).unwrap();
        git_cmd(&home, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
        git_cmd(&repo, &["init", "-q", "-b", "main"]);
        file(&repo.join("README.md"), 100, 0);
        std::fs::write(repo.join(".gitignore"), "node_modules\n").unwrap();
        git_cmd(&repo, &["add", "."]);
        git_cmd(&repo, &["commit", "-qm", "init"]);
        git_cmd(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git_cmd(&repo, &["push", "-q", "origin", "main"]);

        let wt = |name: &str| repo.join(".claude/worktrees").join(name);
        let add = |name: &str| git_cmd(&repo, &["worktree", "add", "-q", "-b", name, wt(name).to_str().unwrap()]);
        // clean: same commit as main; ignored node_modules does not make it dirty.
        add("clean");
        file(&wt("clean").join("node_modules/x.js"), 50_000, 0);
        // dirty: an untracked file.
        add("dirty");
        file(&wt("dirty").join("notes.txt"), 10, 0);
        // ahead: a local-only commit.
        add("ahead");
        git_cmd(&wt("ahead"), &["commit", "-q", "--allow-empty", "-m", "wip"]);
        // pushed: a commit that exists on the remote.
        add("pushed");
        git_cmd(&wt("pushed"), &["commit", "-q", "--allow-empty", "-m", "feature"]);
        git_cmd(&wt("pushed"), &["push", "-q", "origin", "pushed"]);
        // locked.
        add("locked");
        git_cmd(&repo, &["worktree", "lock", "--reason", "in use", wt("locked").to_str().unwrap()]);
        // prunable: the folder vanished.
        add("gone");
        std::fs::remove_dir_all(wt("gone")).unwrap();
        // orphan: a plain leftover folder.
        file(&wt("orphan").join("big.bin"), 70_000, 0);

        let env = TestEnv::new(&home);
        let r = rule(
            "dev.worktrees",
            DetectorKind::GitWorktrees,
            ActionKind::GitWorktreeRemove,
            &["~/Developer"],
            serde_json::json!({}),
        );
        let mut found = GitWorktreesDetector.detect(&r, &env.ctx()).unwrap();
        found.sort_by(|a, b| a.id.cmp(&b.id));
        let by_label = |l: &str| found.iter().find(|f| f.label == l).unwrap_or_else(|| panic!("no finding {l}"));
        let repo_s = repo.to_string_lossy().into_owned();

        assert_eq!(found.len(), 7, "{:#?}", found.iter().map(|f| &f.label).collect::<Vec<_>>());
        let clean = by_label("clean");
        assert_eq!(clean.action, ActionKind::GitWorktreeRemove);
        assert!(clean.blockers.is_empty(), "{:?}", clean.blockers);
        assert_eq!(
            clean.command.as_ref().unwrap(),
            &vec!["git", "-C", &repo_s, "worktree", "remove", &wt("clean").to_string_lossy()]
        );
        assert_eq!(clean.group.as_deref(), Some(repo_s.as_str()));
        assert!(clean.bytes >= 50_000);
        assert!(clean.reason.starts_with("Git worktree of repo on clean"), "{}", clean.reason);

        assert_eq!(by_label("dirty").blockers, vec!["Uncommitted changes in worktree (1 file)"]);
        assert_eq!(by_label("ahead").blockers, vec!["Unpushed commits (1 not on any remote branch)"]);
        assert!(by_label("pushed").blockers.is_empty(), "{:?}", by_label("pushed").blockers);
        assert_eq!(by_label("locked").blockers, vec!["Worktree is locked: in use"]);

        let orphan = by_label("orphan");
        assert_eq!(orphan.action, ActionKind::Trash);
        assert!(orphan.command.is_none() && orphan.bytes >= 70_000);

        // Stale entries are information only: listed, with the command to run by hand.
        let stale = by_label("repo: 1 stale worktree entry");
        assert_eq!(stale.action, ActionKind::InfoOnly);
        assert!(stale.path.is_none() && stale.command.is_none() && stale.bytes == 0);
        assert_eq!(stale.risk, Risk::Review);
        assert!(!stale.selected_by_default);
        assert_eq!(
            stale.reason,
            format!(
                "Git still lists 1 worktree entry whose folder no longer exists: {}. Run `git -C {repo_s} worktree \
                 prune` yourself if this worktree is really gone: a worktree on a disk that is only unmounted \
                 would lose its git data (staged changes, detached commits).",
                wt("gone").display()
            )
        );
        assert_eq!(stale.id, format!("dev.worktrees:{repo_s}#prune"));
        assert_eq!(stale.group.as_deref(), Some(repo_s.as_str()));
    }

    #[test]
    fn stale_entries_name_ten_and_quote_the_repository() {
        let r = rule(
            "dev.worktrees",
            DetectorKind::GitWorktrees,
            ActionKind::GitWorktreeRemove,
            &["~/Developer"],
            serde_json::json!({}),
        );
        let entries: Vec<WorktreeEntry> = (0..12)
            .map(|i| WorktreeEntry {
                path: PathBuf::from(format!("/Volumes/X/wt{i}")),
                prunable: Some(String::new()),
                ..Default::default()
            })
            .collect();
        let refs: Vec<&WorktreeEntry> = entries.iter().collect();
        let f = stale_entries_finding(&r, "/Users/me/it's mine", "it's mine", &refs.iter().collect::<Vec<_>>());
        assert_eq!(f.label, "it's mine: 12 stale worktree entries");
        assert_eq!((f.action, f.command.as_ref(), f.selected_by_default), (ActionKind::InfoOnly, None, false));
        assert!(f.reason.contains("no longer exist: /Volumes/X/wt0, "), "{}", f.reason);
        assert!(f.reason.contains("/Volumes/X/wt9 and 2 more. "), "{}", f.reason);
        assert!(!f.reason.contains("wt10"), "{}", f.reason);
        assert!(
            f.reason.contains(r"Run `git -C '/Users/me/it'\''s mine' worktree prune` yourself if these worktrees are"),
            "{}",
            f.reason
        );
    }

    #[test]
    fn worktree_outside_roots_leads_back_to_main_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let repo = home.join("elsewhere/repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_cmd(&repo, &["init", "-q", "-b", "main"]);
        git_cmd(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git_cmd(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let wt = home.join("Developer/wt");
        git_cmd(&repo, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);

        let env = TestEnv::new(&home);
        let r = rule(
            "dev.worktrees",
            DetectorKind::GitWorktrees,
            ActionKind::GitWorktreeRemove,
            &["~/Developer"],
            serde_json::json!({}),
        );
        let found = GitWorktreesDetector.detect(&r, &env.ctx()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "wt");
        assert!(found[0].reason.contains("detached at"), "{}", found[0].reason);
        assert!(found[0].blockers.is_empty(), "{:?}", found[0].blockers);
    }

    /// Safety fix 3: the detector blocks what the executor would refuse, whatever the user's config.
    #[test]
    fn hidden_untracked_files_submodules_and_local_only_commits_block() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let repo = home.join("Developer/repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_cmd(&repo, &["init", "-q", "-b", "main"]);
        git_cmd(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let wt = |name: &str| repo.join(".claude/worktrees").join(name);
        let add = |name: &str| git_cmd(&repo, &["worktree", "add", "-q", "-b", name, wt(name).to_str().unwrap()]);

        git_cmd(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        add("hidden");
        git_cmd(&repo, &["config", "status.showUntrackedFiles", "no"]);
        file(&wt("hidden").join("notes.md"), 10, 0);
        add("subs");
        std::fs::create_dir_all(repo.join(".git/worktrees/subs/modules/lib")).unwrap();
        add("clean");
        // A commit merged into the local main branch but on no remote branch still blocks.
        add("local");
        git_cmd(&wt("local"), &["commit", "-q", "--allow-empty", "-m", "local work"]);
        git_cmd(&repo, &["merge", "-q", "--ff-only", "local"]);
        // A leftover folder that still has a .git file pointing nowhere.
        file(&wt("broken").join("big.bin"), 1000, 0);
        std::fs::write(wt("broken").join(".git"), "gitdir: /nowhere/.git/worktrees/broken\n").unwrap();

        let env = TestEnv::new(&home);
        let r = rule(
            "dev.worktrees",
            DetectorKind::GitWorktrees,
            ActionKind::GitWorktreeRemove,
            &["~/Developer"],
            serde_json::json!({}),
        );
        let found = GitWorktreesDetector.detect(&r, &env.ctx()).unwrap();
        let by_label = |l: &str| found.iter().find(|f| f.label == l).unwrap_or_else(|| panic!("no finding {l}"));
        assert_eq!(by_label("local").blockers, vec!["Unpushed commits (1 not on any remote branch)"]);
        assert_eq!(by_label("hidden").blockers, vec!["Uncommitted changes in worktree (1 file)"]);
        assert_eq!(
            by_label("subs").blockers,
            vec!["Has initialized submodules (their git data lives in the worktree's admin dir)"]
        );
        assert!(by_label("clean").blockers.is_empty(), "{:?}", by_label("clean").blockers);
        let broken = by_label("broken");
        assert_eq!(broken.action, ActionKind::Trash);
        assert!(broken.blockers[0].starts_with("Still has a .git file"), "{:?}", broken.blockers);
    }

    #[test]
    fn a_broken_repository_is_warned_not_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let good = home.join("Developer/good");
        std::fs::create_dir_all(&good).unwrap();
        git_cmd(&good, &["init", "-q", "-b", "main"]);
        git_cmd(&good, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let broken = home.join("Developer/broken");
        std::fs::create_dir_all(broken.join(".git")).unwrap(); // an empty .git: not a repository

        let env = TestEnv::new(&home);
        let r = rule(
            "dev.worktrees",
            DetectorKind::GitWorktrees,
            ActionKind::GitWorktreeRemove,
            &["~/Developer"],
            serde_json::json!({}),
        );
        assert!(GitWorktreesDetector.detect(&r, &env.ctx()).unwrap().is_empty());
        let warned = env.sizer.take_errors();
        assert_eq!(warned.len(), 1, "{warned:?}");
        assert_eq!(warned[0].path, broken.to_string_lossy());
        assert!(warned[0].message.starts_with("dev.worktrees: "), "{warned:?}");

        // Only broken repositories: the rule itself fails.
        std::fs::remove_dir_all(&good).unwrap();
        let err = GitWorktreesDetector.detect(&r, &env.ctx()).unwrap_err().to_string();
        assert!(err.contains("broken"), "{err}");
    }

    #[test]
    fn no_repos_no_findings() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("Developer/x")).unwrap();
        let env = TestEnv::new(tmp.path());
        let r = rule(
            "dev.worktrees",
            DetectorKind::GitWorktrees,
            ActionKind::GitWorktreeRemove,
            &["~/Developer"],
            serde_json::json!({}),
        );
        assert!(GitWorktreesDetector.detect(&r, &env.ctx()).unwrap().is_empty());
    }
}
