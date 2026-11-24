//! `git-worktrees` detector — linked and orphaned git worktrees (owned by S2).
//!
//! Repositories are found under the roots (depth ≤ 4, no hidden dirs, no node_modules); a linked
//! worktree's `.git` file also leads back to its main repository. Per repository:
//! - each linked worktree → `git-worktree-remove` finding with
//!   `command = ["git", "-C", <repo>, "worktree", "remove", <path>]` (ARCHITECTURE.md §10) and
//!   blockers for: locked, uncommitted/untracked changes, HEAD not reachable from any remote branch
//!   nor merged into the default branch, outside the home folder;
//! - directories under `<repo>/.claude/worktrees/` that git does not know → `trash` findings;
//! - prunable entries (directory gone) → one `command` finding running `git worktree prune`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;

use crate::detectors::external;
use crate::detectors::support::{days_since, for_each_root, list_dir, param_u64, walk_dirs};
use crate::detectors::{Detector, make_finding};
use crate::error::CoreError;
use crate::model::{ActionKind, Finding, RuleDef};
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

/// The local default branch: origin/HEAD's target if checked out locally, else `main`, else `master`.
fn default_branch(repo: &Path) -> Option<String> {
    let out =
        git(repo, &["for-each-ref", "--format=%(refname)%09%(symref)", "refs/heads/", "refs/remotes/origin/HEAD"])
            .ok()?;
    let mut locals = BTreeSet::new();
    let mut origin_head = None;
    for line in out.stdout.lines() {
        let (name, symref) = line.split_once('\t').unwrap_or((line, ""));
        if name == "refs/remotes/origin/HEAD" {
            origin_head = symref.strip_prefix("refs/remotes/origin/").map(str::to_string);
        } else if let Some(short) = name.strip_prefix("refs/heads/") {
            locals.insert(short.to_string());
        }
    }
    let candidates = origin_head.into_iter().chain(["main".to_string(), "master".to_string()]);
    candidates.into_iter().find(|b| locals.contains(b)).map(|b| format!("refs/heads/{b}"))
}

fn short_branch(entry: &WorktreeEntry) -> String {
    match (&entry.branch, &entry.head) {
        (Some(b), _) => b.strip_prefix("refs/heads/").unwrap_or(b).to_string(),
        (None, Some(h)) => format!("detached at {}", &h[..h.len().min(8)]),
        _ => "unknown HEAD".to_string(),
    }
}

/// Blockers for removing one linked worktree.
fn blockers_for(repo: &Path, entry: &WorktreeEntry, default: Option<&str>, home: &Path) -> Vec<String> {
    let mut blockers = Vec::new();
    if let Some(reason) = &entry.locked {
        blockers.push(if reason.is_empty() {
            "Worktree is locked".into()
        } else {
            format!("Worktree is locked: {reason}")
        });
    }
    match git(&entry.path, &["status", "--porcelain", "--untracked-files=normal", "--ignore-submodules=none"]) {
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
        let mut args = vec!["rev-list", "--count", head.as_str(), "--not", "--remotes"];
        if let Some(default) = default {
            args.push(default);
        }
        match git(repo, &args).map(|o| o.stdout.trim().parse::<u64>()) {
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => {
                let target = default.map(|d| d.trim_start_matches("refs/heads/")).unwrap_or("the default branch");
                blockers.push(format!("Unpushed commits ({n} not on any remote branch or merged into {target})"));
            }
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
    let default = default_branch(repo);

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
            for blocker in blockers_for(repo, entry, default.as_deref(), &ctx.home) {
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
            let orphaned = match dot_git.symlink_metadata() {
                Err(_) => true,
                Ok(m) if m.is_file() => main_repo_of_worktree(&dot_git).is_none(),
                Ok(_) => false, // an independent clone, not a worktree
            };
            if !orphaned {
                continue;
            }
            let size = ctx.sizer.size_of(&child.path);
            let reason = format!("Leftover worktree folder: not registered in {repo_name}'s git worktrees");
            let mut f = make_finding(rule, &path_str(&child.path), Some(&child.path), &size, reason);
            f.action = ActionKind::Trash;
            f.group = group.clone();
            findings.push(f);
        }
    }

    if !prunable.is_empty() {
        let reason = format!("{} stale worktree entries whose folders no longer exist", prunable.len());
        let mut f = make_finding(rule, &format!("{repo_s}#prune"), None, &SizeInfo::default(), reason);
        f.label = format!("{repo_name}: prune worktree entries");
        f.action = ActionKind::Command;
        f.command = Some(vec!["git".into(), "-C".into(), repo_s.clone(), "worktree".into(), "prune".into()]);
        f.group = group.clone();
        findings.push(f);
    }
    Ok(findings)
}

impl Detector for GitWorktreesDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let depth = param_u64(rule, "depth").unwrap_or(4) as usize;
        let mut repos = BTreeSet::new();
        for_each_root(rule, ctx, |root| find_repos(root, depth, &mut repos))?;

        let errors: Mutex<BTreeMap<PathBuf, String>> = Mutex::new(BTreeMap::new());
        let findings: Vec<Finding> = repos
            .par_iter()
            .flat_map_iter(|repo| match repo_findings(rule, ctx, repo) {
                Ok(found) => found,
                Err(e) => {
                    errors.lock().unwrap_or_else(|p| p.into_inner()).insert(repo.clone(), e.to_string());
                    Vec::new()
                }
            })
            .collect();
        let errors = errors.into_inner().unwrap_or_else(|p| p.into_inner());
        if findings.is_empty()
            && let Some((repo, message)) = errors.into_iter().next()
        {
            return Err(CoreError::Command(format!("{}: {message}", repo.display())));
        }
        Ok(findings)
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
    fn worktrees_blockers_orphans_and_prune() {
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
        assert_eq!(
            by_label("ahead").blockers,
            vec!["Unpushed commits (1 not on any remote branch or merged into main)"]
        );
        assert!(by_label("pushed").blockers.is_empty(), "{:?}", by_label("pushed").blockers);
        assert_eq!(by_label("locked").blockers, vec!["Worktree is locked: in use"]);

        let orphan = by_label("orphan");
        assert_eq!(orphan.action, ActionKind::Trash);
        assert!(orphan.command.is_none() && orphan.bytes >= 70_000);

        let prune = by_label("repo: prune worktree entries");
        assert_eq!(prune.action, ActionKind::Command);
        assert!(prune.path.is_none());
        assert_eq!(prune.command.as_ref().unwrap(), &vec!["git", "-C", &repo_s, "worktree", "prune"]);
        assert_eq!(prune.id, format!("dev.worktrees:{repo_s}#prune"));
    }

    #[test]
    fn worktree_outside_roots_leads_back_to_main_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let repo = home.join("elsewhere/repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_cmd(&repo, &["init", "-q", "-b", "main"]);
        git_cmd(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
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
