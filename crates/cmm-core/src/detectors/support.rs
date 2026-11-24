//! Small helpers shared by the S2 detectors: rule params, directory listing and pruned walks,
//! timestamps (owned by S2).

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::error::CoreError;
use crate::model::RuleDef;
use crate::rules::expand_roots;
use crate::scan::ScanContext;

/// Runs `per_root` on every existing root of `rule`. A failing root never hides the others: while
/// at least one root succeeded, each failure is reported through [`ScanContext::warn`] (contract
/// v2); if every root failed, the first error is returned (the rule fails) and the rest are warned.
pub fn for_each_root(
    rule: &RuleDef,
    ctx: &ScanContext,
    mut per_root: impl FnMut(&Path) -> Result<(), CoreError>,
) -> Result<(), CoreError> {
    let mut failures: Vec<(PathBuf, CoreError)> = Vec::new();
    let mut any_ok = false;
    for root in expand_roots(rule, &ctx.home) {
        match per_root(&root) {
            Ok(()) => any_ok = true,
            Err(e) => failures.push((root, e)),
        }
    }
    let mut failures = failures.into_iter();
    let fatal = if any_ok { None } else { failures.next() };
    for (root, e) in failures {
        ctx.warn(root.to_string_lossy(), format!("{}: {e}", rule.id));
    }
    match fatal {
        Some((root, e)) => Err(CoreError::Rule(format!("{}: {e}", root.display()))),
        None => Ok(()),
    }
}

/// `rule.params[key]` as an integer.
pub fn param_u64(rule: &RuleDef, key: &str) -> Option<u64> {
    rule.params.get(key).and_then(serde_json::Value::as_u64)
}

/// `rule.params[key]` as a list of strings (non-string items ignored).
pub fn param_strings(rule: &RuleDef, key: &str) -> Option<Vec<String>> {
    let items = rule.params.get(key)?.as_array()?;
    Some(items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
}

/// Whole days from `then` to `now` (0 if `then` is in the future).
pub fn days_since(now: DateTime<Utc>, then: DateTime<Utc>) -> i64 {
    (now - then).num_days().max(0)
}

pub fn mtime_of(meta: &Metadata) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(meta.mtime(), meta.mtime_nsec() as u32)
}

/// Newest of modification and creation (birth) time. A file downloaded today keeps the server's
/// old mtime on some browsers, but its birth time is today.
pub fn touched_of(meta: &Metadata) -> Option<DateTime<Utc>> {
    let born = meta.created().ok().map(DateTime::<Utc>::from);
    mtime_of(meta).max(born)
}

/// One directory entry, `lstat`ed (symlinks are not followed).
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub meta: Metadata,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.meta.file_type().is_dir()
    }
    pub fn is_file(&self) -> bool {
        self.meta.file_type().is_file()
    }
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// Lists `dir` sorted by name. Entries that vanish or cannot be `lstat`ed are skipped.
pub fn list_dir(dir: &Path) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for item in std::fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let path = item.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        out.push(Entry { name: item.file_name().to_string_lossy().into_owned(), path, meta });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Directory names a generic walk never enters (artifacts, VCS, package internals).
pub const NEVER_DESCEND: &[&str] = &["node_modules", ".git", "Pods", "DerivedData", "target", ".venv", "venv"];

/// macOS packages and bundles: shown as single files in Finder, never walked into.
pub fn is_bundle_name(name: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        ".app",
        ".framework",
        ".bundle",
        ".plugin",
        ".kext",
        ".pkg",
        ".mpkg",
        ".photoslibrary",
        ".musiclibrary",
        ".xcodeproj",
        ".xcworkspace",
        ".xcarchive",
        ".simruntime",
        ".sparsebundle",
        ".rtfd",
        ".key",
        ".pages",
        ".numbers",
    ];
    let lower = name.to_ascii_lowercase();
    SUFFIXES.iter().any(|s| lower.ends_with(s))
}

/// Visits `root` and its subdirectories down to `max_depth` (root's entries are depth 1).
///
/// `visit(dir, entries)` returns the names of child directories it does **not** want walked
/// (e.g. members of a version group). Hidden directories, bundles, [`NEVER_DESCEND`] names,
/// symlinks and other devices are never walked. An unreadable root is an error; unreadable
/// subdirectories are skipped.
pub fn walk_dirs(
    root: &Path,
    max_depth: usize,
    visit: &mut dyn FnMut(&Path, &[Entry]) -> Vec<String>,
) -> Result<(), CoreError> {
    let root_meta = std::fs::metadata(root)?;
    let entries = list_dir(root)?;
    walk_inner(root, entries, root_meta.dev(), 1, max_depth, visit);
    Ok(())
}

fn walk_inner(
    dir: &Path,
    entries: Vec<Entry>,
    dev: u64,
    depth: usize,
    max_depth: usize,
    visit: &mut dyn FnMut(&Path, &[Entry]) -> Vec<String>,
) {
    let skip = visit(dir, &entries);
    if depth >= max_depth {
        return;
    }
    for entry in entries {
        if !entry.is_dir()
            || entry.is_hidden()
            || entry.meta.dev() != dev
            || is_bundle_name(&entry.name)
            || NEVER_DESCEND.contains(&entry.name.as_str())
            || skip.contains(&entry.name)
        {
            continue;
        }
        if let Ok(children) = list_dir(&entry.path) {
            walk_inner(&entry.path, children, dev, depth + 1, max_depth, visit);
        }
    }
}

/// Fixtures for detector tests: a context over a tempdir "home", plus rule and mtime helpers.
#[cfg(test)]
pub mod testutil {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use chrono::{DateTime, Utc};

    use crate::config::Config;
    use crate::model::{ActionKind, Category, DetectorKind, Risk, RuleDef};
    use crate::process::ProcessSnapshot;
    use crate::scan::ScanContext;
    use crate::size::Sizer;

    pub struct TestEnv {
        pub home: PathBuf,
        pub now: DateTime<Utc>,
        pub sizer: Sizer,
        pub processes: ProcessSnapshot,
        pub config: Config,
    }

    impl TestEnv {
        pub fn new(home: &Path) -> Self {
            Self {
                home: home.to_path_buf(),
                now: Utc::now(),
                sizer: Sizer::new(),
                processes: ProcessSnapshot::from_names(Vec::<String>::new()),
                config: Config { data_dir: home.join("data"), ..Config::default() },
            }
        }

        pub fn ctx(&self) -> ScanContext<'_> {
            ScanContext {
                home: self.home.clone(),
                now: self.now,
                sizer: &self.sizer,
                processes: &self.processes,
                config: &self.config,
            }
        }
    }

    pub fn rule(
        id: &str,
        detector: DetectorKind,
        action: ActionKind,
        roots: &[&str],
        params: serde_json::Value,
    ) -> RuleDef {
        RuleDef {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            category: Category::Downloads,
            risk: Risk::Review,
            detector,
            roots: roots.iter().map(|r| r.to_string()).collect(),
            params,
            action,
            requires_quit: vec![],
            regenerates: false,
            enabled: true,
        }
    }

    /// Writes `bytes` bytes to `path` (creating parents) and backdates it by `days_ago` days.
    pub fn file(path: &Path, bytes: usize, days_ago: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![1u8; bytes]).unwrap();
        age(path, days_ago);
    }

    /// Sets mtime (and, on APFS, birth time) of a file or directory to `days_ago` days back.
    pub fn age(path: &Path, days_ago: u64) {
        let when = SystemTime::now() - Duration::from_secs(days_ago * 86_400);
        std::fs::File::open(path).unwrap().set_modified(when).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_respects_depth_and_pruning() {
        let dir = tempfile::tempdir().unwrap();
        for p in ["a/b/c/d", "node_modules/x", ".hidden/y", "Foo.app/Contents", "skipme/z"] {
            std::fs::create_dir_all(dir.path().join(p)).unwrap();
        }
        let mut seen = Vec::new();
        walk_dirs(dir.path(), 3, &mut |d, _| {
            seen.push(d.strip_prefix(dir.path()).unwrap().to_string_lossy().into_owned());
            vec!["skipme".to_string()]
        })
        .unwrap();
        seen.sort();
        assert_eq!(seen, vec!["", "a", "a/b"]);
        assert!(walk_dirs(&dir.path().join("missing"), 3, &mut |_, _| vec![]).is_err());
    }

    #[test]
    fn partial_root_failures_are_warned_total_failure_is_an_error() {
        use crate::model::{ActionKind, DetectorKind};
        let dir = tempfile::tempdir().unwrap();
        for name in ["good", "bad1", "bad2"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        let env = testutil::TestEnv::new(dir.path());
        let rule = testutil::rule(
            "t.roots",
            DetectorKind::Installers,
            ActionKind::Trash,
            &["~/good", "~/bad1", "~/bad2"],
            serde_json::json!({}),
        );
        let fail_bad = |root: &Path| {
            if root.ends_with("good") { Ok(()) } else { Err(CoreError::Rule("unreadable".into())) }
        };

        for_each_root(&rule, &env.ctx(), fail_bad).unwrap();
        let warned = env.sizer.take_errors();
        assert_eq!(warned.len(), 2, "{warned:?}");
        assert!(warned.iter().all(|e| e.message.starts_with("t.roots: ") && e.message.ends_with("unreadable")));
        assert!(warned.iter().any(|e| e.path.ends_with("bad1")) && warned.iter().any(|e| e.path.ends_with("bad2")));

        let rule_all_bad = RuleDef { roots: vec!["~/bad1".into(), "~/bad2".into()], ..rule };
        let err = for_each_root(&rule_all_bad, &env.ctx(), fail_bad).unwrap_err().to_string();
        assert!(err.contains("bad1") && err.contains("unreadable"), "{err}");
        let warned = env.sizer.take_errors();
        assert_eq!(warned.len(), 1, "the other failed root is still reported: {warned:?}");
        assert!(warned[0].path.ends_with("bad2"));
    }

    #[test]
    fn params_helpers() {
        let mut rule = crate::rules::builtin_rules().unwrap().remove(0);
        rule.params = serde_json::json!({"days": 5, "names": ["a", 1, "b"]});
        assert_eq!(param_u64(&rule, "days"), Some(5));
        assert_eq!(param_u64(&rule, "keep"), None);
        assert_eq!(param_strings(&rule, "names"), Some(vec!["a".to_string(), "b".to_string()]));
    }
}
