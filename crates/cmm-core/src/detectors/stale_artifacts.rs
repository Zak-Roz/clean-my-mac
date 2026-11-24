//! `stale-artifacts` detector — artifact dirs (node_modules, target, ...) of projects untouched for `stale_days` (owned by S2).
//!
//! One parallel pass over each root:
//! - never enters artifact dirs (`names`), `.git`, other devices, symlinks or hidden dirs — except
//!   `<dir>/.claude/worktrees`, whose worktrees are walked as independent projects;
//! - a *project* is a directory with a marker (`package.json`, `Cargo.toml`, `*.xcodeproj`, …); an
//!   artifact belongs to its nearest project ancestor (artifacts outside any project are ignored);
//! - a project's source mtime is the newest regular file below it, excluding artifact dirs, `.git`,
//!   hidden dirs and `.DS_Store` (nested projects count towards their parents).
//!
//! Then, per project whose source mtime is already older than `days` (param, default
//! `config.stale_days`), the repository's last commit time (`git log -1 --format=%ct`) is consulted;
//! last activity = max of both. Artifacts tracked by git (a committed `dist/`) are not artifacts and
//! are skipped.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rayon::prelude::*;

use crate::detectors::external;
use crate::detectors::support::{Entry, days_since, for_each_root, list_dir, mtime_of, param_strings, param_u64};
use crate::detectors::{Detector, make_finding};
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct StaleArtifactsDetector;

/// Artifact directory names used when the rule has no `names` param (ARCHITECTURE.md §4).
pub const DEFAULT_NAMES: &[&str] =
    &["node_modules", "target", ".next", "dist", "build", "DerivedData", ".turbo", ".venv", "Pods", ".gradle"];

const MARKER_FILES: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "Podfile",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "requirements.txt",
    "setup.py",
    "Pipfile",
    "Gemfile",
    "composer.json",
    "pom.xml",
    "Package.swift",
    "deno.json",
    "pubspec.yaml",
    "mix.exs",
];
const MARKER_DIR_SUFFIXES: &[&str] = &[".xcodeproj", ".xcworkspace"];
const MAX_DEPTH: usize = 24;

fn is_marker(entry: &Entry) -> bool {
    if entry.is_file() {
        MARKER_FILES.contains(&entry.name.as_str())
    } else {
        entry.is_dir() && MARKER_DIR_SUFFIXES.iter().any(|s| entry.name.ends_with(s))
    }
}

struct Walker<'a> {
    names: &'a HashSet<String>,
    dev: u64,
}

#[derive(Default)]
struct Walked {
    /// Newest source file mtime in this subtree (separately walked worktrees excluded).
    newest: Option<DateTime<Utc>>,
    /// (artifact dir, its project)
    artifacts: Vec<(PathBuf, PathBuf)>,
    /// (project dir, newest source mtime)
    projects: Vec<(PathBuf, Option<DateTime<Utc>>)>,
}

impl Walked {
    fn absorb(&mut self, other: Walked, count_mtime: bool) {
        if count_mtime {
            self.newest = self.newest.max(other.newest);
        }
        self.artifacts.extend(other.artifacts);
        self.projects.extend(other.projects);
    }
}

impl Walker<'_> {
    fn walk(&self, dir: &Path, depth: usize, project: Option<&Path>) -> Walked {
        let Ok(entries) = list_dir(dir) else { return Walked::default() };
        let is_project = entries.iter().any(is_marker);
        let project = if is_project { Some(dir) } else { project };

        let mut out = Walked::default();
        let mut subdirs: Vec<(PathBuf, Option<&Path>, bool)> = Vec::new(); // (dir, project, counts towards mtime)
        for entry in &entries {
            if entry.is_file() {
                if entry.name != ".DS_Store" {
                    out.newest = out.newest.max(mtime_of(&entry.meta));
                }
                continue;
            }
            if !entry.is_dir() || entry.meta.dev() != self.dev {
                continue;
            }
            if self.names.contains(&entry.name) {
                if let Some(project) = project {
                    out.artifacts.push((entry.path.clone(), project.to_path_buf()));
                }
                continue;
            }
            if depth >= MAX_DEPTH {
                continue;
            }
            if entry.name == ".claude" {
                let worktrees = entry.path.join("worktrees");
                if worktrees.is_dir() {
                    // Each worktree is its own project; its activity does not make the parent active.
                    subdirs.push((worktrees, None, false));
                }
                continue;
            }
            if entry.is_hidden() {
                continue;
            }
            subdirs.push((entry.path.clone(), project, true));
        }

        let children: Vec<(Walked, bool)> = subdirs
            .into_par_iter()
            .map(|(path, project, counts)| (self.walk(&path, depth + 1, project), counts))
            .collect();
        for (child, counts) in children {
            out.absorb(child, counts);
        }
        if is_project {
            out.projects.push((dir.to_path_buf(), out.newest));
        }
        out
    }
}

/// Nearest ancestor-or-self containing `.git` (directory or worktree file).
fn repo_root(path: &Path) -> Option<&Path> {
    path.ancestors().find(|p| p.join(".git").symlink_metadata().is_ok())
}

/// Last commit time of the repository checked out at `repo` (`None` if not a repo or no commits).
/// A git that cannot run (missing, timed out) is warned: the project is then judged by mtime only.
fn last_commit(repo: &Path, ctx: &ScanContext) -> Option<DateTime<Utc>> {
    let dir = repo.to_string_lossy();
    let out = match external::run("git", &["-C", &dir, "log", "-1", "--format=%ct"]) {
        Ok(out) => out,
        Err(e) => {
            ctx.warn(dir.as_ref(), format!("last commit unknown, judged by file dates only: {e}"));
            return None;
        }
    };
    if !out.success() {
        return None;
    }
    DateTime::<Utc>::from_timestamp(out.stdout.trim().parse().ok()?, 0)
}

/// Whether git tracks anything under `dir` (a committed `dist/` is source, not an artifact).
/// Unknown (git failed) counts as tracked, to stay on the safe side.
fn tracked_by_git(dir: &Path) -> bool {
    let path = dir.to_string_lossy();
    match external::run("git", &["-C", &path, "ls-files", "--cached", "--", "."]) {
        Ok(out) if out.success() => !out.stdout.trim().is_empty(),
        _ => true,
    }
}

/// "<repo or project name>/<path inside it>", e.g. `example-app/apps/web/node_modules`. The
/// repository root is used when it lies inside the scanned root (a dotfiles repo at `~` is not).
fn label_for(artifact: &Path, project: &Path, roots: &[PathBuf]) -> String {
    let base = repo_root(project)
        .filter(|repo| roots.iter().any(|root| repo.starts_with(root) && *repo != root.as_path()))
        .unwrap_or(project);
    let name = base.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let rel = artifact.strip_prefix(base).unwrap_or(artifact);
    format!("{name}/{}", rel.display())
}

impl Detector for StaleArtifactsDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let days = param_u64(rule, "days").unwrap_or(u64::from(ctx.config.stale_days)) as i64;
        let names: HashSet<String> = param_strings(rule, "names")
            .unwrap_or_else(|| DEFAULT_NAMES.iter().map(|s| s.to_string()).collect())
            .into_iter()
            .collect();

        let mut walked = Walked::default();
        let mut roots = Vec::new();
        for_each_root(rule, ctx, |root| {
            let dev = std::fs::metadata(root)?.dev();
            list_dir(root)?; // unreadable root -> error for this root
            walked.absorb(Walker { names: &names, dev }.walk(root, 0, None), true);
            roots.push(root.to_path_buf());
            Ok(())
        })?;

        let source_mtime: HashMap<PathBuf, Option<DateTime<Utc>>> = walked.projects.into_iter().collect();
        let is_stale = |t: Option<DateTime<Utc>>| t.is_none_or(|t| days_since(ctx.now, t) >= days);

        // Only projects that look stale by mtime need the (slower) git check.
        let mut stale_projects: Vec<&PathBuf> = walked
            .artifacts
            .iter()
            .map(|(_, project)| project)
            .filter(|p| is_stale(source_mtime.get(*p).copied().flatten()))
            .collect();
        stale_projects.sort();
        stale_projects.dedup();
        let commit_cache: Mutex<HashMap<PathBuf, Option<DateTime<Utc>>>> = Mutex::new(HashMap::new());
        let activity: HashMap<&PathBuf, Option<DateTime<Utc>>> = stale_projects
            .par_iter()
            .map(|project| {
                let commit = repo_root(project).and_then(|repo| {
                    if let Some(hit) = commit_cache.lock().unwrap_or_else(|e| e.into_inner()).get(repo) {
                        return *hit;
                    }
                    let t = last_commit(repo, ctx);
                    commit_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(repo.to_path_buf(), t);
                    t
                });
                (*project, source_mtime.get(*project).copied().flatten().max(commit))
            })
            .collect();

        let findings = walked
            .artifacts
            .par_iter()
            .filter_map(|(artifact, project)| {
                let last = *activity.get(project)?;
                let last = last?;
                if !is_stale(Some(last)) {
                    return None;
                }
                let in_repo = repo_root(artifact).is_some();
                let name = artifact.file_name()?.to_string_lossy();
                if in_repo && name != "node_modules" && tracked_by_git(artifact) {
                    return None;
                }
                let size = ctx.sizer.size_of(artifact);
                let reason = format!("Project untouched for {} days", days_since(ctx.now, last));
                let mut f = make_finding(rule, &artifact.to_string_lossy(), Some(artifact), &size, reason);
                f.label = label_for(artifact, project, &roots);
                f.last_used = Some(last);
                Some(f)
            })
            .collect();
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::support::testutil::{TestEnv, age, file, rule};
    use crate::model::{ActionKind, DetectorKind};

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A project with a 1 MB node_modules, all source files `days_ago` old.
    fn project(root: &Path, days_ago: u64) {
        file(&root.join("package.json"), 20, days_ago);
        file(&root.join("src/index.ts"), 20, days_ago);
        file(&root.join("node_modules/dep/index.js"), 1 << 20, 1);
    }

    fn detect(home: &Path) -> Vec<Finding> {
        let env = TestEnv::new(home);
        let r =
            rule("dev.stale", DetectorKind::StaleArtifacts, ActionKind::Trash, &["~/Developer"], serde_json::json!({}));
        let mut found = StaleArtifactsDetector.detect(&r, &env.ctx()).unwrap();
        found.sort_by(|a, b| a.label.cmp(&b.label));
        found
    }

    fn labels(found: &[Finding]) -> Vec<&str> {
        found.iter().map(|f| f.label.as_str()).collect()
    }

    #[test]
    fn stale_vs_active_projects() {
        let home = tempfile::tempdir().unwrap();
        let dev = home.path().join("Developer");
        project(&dev.join("old-app"), 100);
        project(&dev.join("fresh-app"), 3);
        // Rust project with a stale target and an artifact name outside any project.
        file(&dev.join("old-crate/Cargo.toml"), 20, 90);
        file(&dev.join("old-crate/target/debug/bin"), 4096, 1);
        file(&dev.join("not-a-project/build/out.bin"), 4096, 200);
        // .DS_Store (Finder) does not count as activity; nested artifacts are never entered.
        file(&dev.join("old-app/.DS_Store"), 10, 0);
        file(&dev.join("old-app/node_modules/inner/package.json"), 10, 0);
        // Hidden tool dirs are not walked.
        project(&dev.join(".cache-tool/hidden-proj"), 100);

        let found = detect(home.path());
        assert_eq!(labels(&found), vec!["old-app/node_modules", "old-crate/target"]);
        assert_eq!(found[0].reason, "Project untouched for 100 days");
        assert!(found[0].bytes >= 1 << 20);
        assert!(found[0].last_used.is_some());
        assert!(!found[0].selected_by_default, "rule risk is review");
    }

    #[test]
    fn monorepo_nested_package_follows_parent_activity() {
        let home = tempfile::tempdir().unwrap();
        let mono = home.path().join("Developer/mono");
        project(&mono, 200);
        project(&mono.join("apps/web"), 200);
        project(&mono.join("apps/old"), 200);
        let found = detect(home.path());
        assert_eq!(labels(&found), vec!["mono/node_modules", "old/node_modules", "web/node_modules"]);

        // A recent change in apps/web makes web and the monorepo root active, not apps/old.
        file(&mono.join("apps/web/src/new.ts"), 10, 1);
        let found = detect(home.path());
        assert_eq!(labels(&found), vec!["old/node_modules"]);
    }

    #[test]
    fn git_commit_time_counts_and_tracked_dirs_are_skipped() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("Developer/repo");
        project(&repo, 120);
        file(&repo.join("dist/bundle.js"), 4096, 120);
        file(&repo.join("build/committed.txt"), 10, 120);
        std::fs::write(repo.join(".gitignore"), "node_modules\ndist\n").unwrap();
        age(&repo.join(".gitignore"), 120);
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "."]);
        // Commit dated 120 days ago: still stale.
        let old = format!("{} +0000", (Utc::now() - chrono::Duration::days(120)).timestamp());
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"])
            .env("GIT_COMMITTER_DATE", &old)
            .env("GIT_AUTHOR_DATE", &old)
            .output()
            .unwrap();
        assert!(out.status.success());
        let found = detect(home.path());
        // build/ is tracked by git -> not an artifact; dist/ and node_modules are ignored -> stale.
        assert_eq!(labels(&found), vec!["repo/dist", "repo/node_modules"]);
        assert_eq!(
            label_for(
                &repo.join("apps/web/node_modules"),
                &repo.join("apps/web"),
                &[repo.parent().unwrap().to_path_buf()]
            ),
            "repo/apps/web/node_modules"
        );
        assert_eq!(label_for(&repo.join("node_modules"), &repo, std::slice::from_ref(&repo)), "repo/node_modules");

        // A fresh commit (files untouched) makes the project active.
        git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "now"]);
        assert!(detect(home.path()).is_empty());
    }

    #[test]
    fn claude_worktrees_are_independent_projects() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("Developer/repo");
        project(&repo, 3);
        project(&repo.join(".claude/worktrees/old-wt"), 90);
        project(&repo.join(".claude/worktrees/new-wt"), 2);
        // Other hidden dirs under .claude are not walked.
        project(&repo.join(".claude/other/x"), 90);
        let found = detect(home.path());
        assert_eq!(labels(&found), vec!["old-wt/node_modules"]);
    }
}
