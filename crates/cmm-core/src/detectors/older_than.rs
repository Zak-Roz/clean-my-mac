//! `older-than` detector — one finding per direct child of each root not used for `days`
//! (owned by S1).
//!
//! "Last used" is the newest of: the mtime of anything inside the child, and Spotlight's
//! `kMDItemLastUsedDate` / `kMDItemDateAdded` of the child itself. Spotlight dates come from one
//! `mdls` call per root (batched only past ~1000 children), bounded by [`TOOL_TIMEOUT`]; when
//! Spotlight has nothing (or `mdls` fails, which is reported as a warning) the mtime alone decides.
//! `kMDItemDateAdded` keeps a just-extracted archive with old mtimes from looking abandoned.
//!
//! Params: `days` (default `downloads_older_than_days`), `extensions` (e.g. `["dmg", ".tar.gz"]`,
//! case-insensitive suffix match), `min_bytes` (default 0). Unknown params are an error.
//! Symlinks, `.DS_Store` and `.localized` are ignored.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rayon::prelude::*;

use crate::config::Config;
use crate::detectors::{Detector, age_days, make_finding};
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::process::{TOOL_TIMEOUT, run_with_timeout};
use crate::rules::expand_roots;
use crate::scan::ScanContext;

pub struct OlderThanDetector;

/// Spotlight lookup: one entry per input path, `None` when Spotlight knows nothing.
type Spotlight = dyn Fn(&[PathBuf], &ScanContext) -> Vec<Option<DateTime<Utc>>> + Sync;

impl Detector for OlderThanDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        detect_with(rule, ctx, &spotlight_dates)
    }
}

#[derive(Debug, PartialEq)]
struct Params {
    days: u32,
    /// Lower-case suffixes without the leading dot.
    extensions: Option<Vec<String>>,
    min_bytes: u64,
}

fn params(rule: &RuleDef, config: &Config) -> Result<Params, CoreError> {
    let bad =
        |key: &str, expected: &str| CoreError::Rule(format!("rule `{}`: params.{key} must be {expected}", rule.id));
    let map = rule.params.as_object().ok_or_else(|| bad("", "a table"))?;
    if let Some(key) = map.keys().find(|k| !["days", "extensions", "min_bytes"].contains(&k.as_str())) {
        return Err(CoreError::Rule(format!("rule `{}`: unknown param `{key}` for older-than", rule.id)));
    }
    let days = match map.get("days") {
        None => config.downloads_older_than_days,
        Some(v) => v.as_u64().and_then(|d| u32::try_from(d).ok()).ok_or_else(|| bad("days", "a whole number"))?,
    };
    let extensions = match map.get("extensions") {
        None => None,
        Some(v) => Some(
            v.as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .map(|e| e.as_str().map(|s| s.trim_start_matches('.').to_ascii_lowercase()))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| bad("extensions", "a list of strings"))?,
        ),
    };
    let min_bytes = match map.get("min_bytes") {
        None => 0,
        Some(v) => v.as_u64().ok_or_else(|| bad("min_bytes", "a whole number"))?,
    };
    Ok(Params { days, extensions, min_bytes })
}

fn detect_with(rule: &RuleDef, ctx: &ScanContext, spotlight: &Spotlight) -> Result<Vec<Finding>, CoreError> {
    let params = params(rule, ctx.config)?;
    let cutoff = ctx.now - chrono::Duration::days(i64::from(params.days));
    let mut findings = Vec::new();
    for root in expand_roots(rule, &ctx.home) {
        if !root.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let children = match list_children(&root, params.extensions.as_deref()) {
            Ok(children) => children,
            Err(e) => {
                ctx.warn(root.to_string_lossy(), e.to_string());
                continue;
            }
        };
        let spotlight_used = spotlight(&children, ctx);
        // Spotlight alone can already prove recent use: skip walking those children.
        let candidates: Vec<(PathBuf, Option<DateTime<Utc>>)> = children
            .into_iter()
            .zip(spotlight_used.into_iter().chain(std::iter::repeat(None)))
            .filter(|(_, used)| used.is_none_or(|t| t < cutoff))
            .collect();
        let found: Vec<Finding> = candidates
            .par_iter()
            .filter_map(|(path, spotlight_used)| {
                let size = ctx.sizer.size_of(path);
                let last_used = size.newest_mtime.max(*spotlight_used);
                if last_used.is_some_and(|t| t >= cutoff) || size.bytes < params.min_bytes {
                    return None;
                }
                let reason = match last_used {
                    Some(t) => {
                        format!("Not opened or modified for {} days (since {})", age_days(ctx.now, t), t.date_naive())
                    }
                    None => String::from("No usage date available"),
                };
                let mut finding = make_finding(rule, &path.to_string_lossy(), Some(path), &size, reason);
                finding.last_used = last_used;
                Some(finding)
            })
            .collect();
        findings.extend(found);
    }
    Ok(findings)
}

/// Direct children of `dir` (sorted), minus symlinks and Finder litter, filtered by extension.
fn list_children(dir: &Path, extensions: Option<&[String]>) -> std::io::Result<Vec<PathBuf>> {
    let mut children = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type().is_ok_and(|t| t.is_symlink()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name == ".ds_store" || name == ".localized" {
            continue;
        }
        if let Some(exts) = extensions
            && !exts.iter().any(|ext| name.len() > ext.len() + 1 && name.ends_with(&format!(".{ext}")))
        {
            continue;
        }
        children.push(entry.path());
    }
    children.sort();
    Ok(children)
}

/// Spotlight dates for `paths` via `mdls -raw` (values NUL-separated, attributes in name order).
fn spotlight_dates(paths: &[PathBuf], ctx: &ScanContext) -> Vec<Option<DateTime<Utc>>> {
    const MAX_ARG_BYTES: usize = 256 * 1024;
    const MAX_PATHS: usize = 1000;
    let mut out = Vec::with_capacity(paths.len());
    let mut start = 0;
    while start < paths.len() {
        let mut end = start;
        let mut bytes = 0;
        while end < paths.len() && end - start < MAX_PATHS && bytes < MAX_ARG_BYTES {
            bytes += paths[end].as_os_str().len() + 1;
            end += 1;
        }
        let chunk = &paths[start..end];
        let mut args: Vec<&OsStr> =
            ["-raw", "-name", "kMDItemDateAdded", "-name", "kMDItemLastUsedDate"].map(OsStr::new).to_vec();
        args.extend(chunk.iter().map(|p| p.as_os_str()));
        let dates = match run_with_timeout("mdls", &args, TOOL_TIMEOUT) {
            Ok(output) => parse_mdls(&output.stdout, chunk.len()).unwrap_or_else(|| {
                let where_ = chunk[0].parent().unwrap_or(&chunk[0]).to_string_lossy();
                ctx.warn(where_, "mdls: no Spotlight dates (a file vanished?); using modification dates");
                vec![None; chunk.len()]
            }),
            Err(e) => {
                ctx.warn("mdls", format!("{e}; using modification dates"));
                vec![None; chunk.len()]
            }
        };
        out.extend(dates);
        start = end;
    }
    out
}

/// Parses `mdls -raw -name kMDItemDateAdded -name kMDItemLastUsedDate f1 … fn` output: `2n`
/// NUL-separated values (`(null)` or `2025-09-02 13:06:13 +0000`); per file the newer of the two.
/// `None` when the value count does not match (mdls stops at the first missing file).
fn parse_mdls(stdout: &[u8], files: usize) -> Option<Vec<Option<DateTime<Utc>>>> {
    let text = String::from_utf8_lossy(stdout);
    let mut values: Vec<&str> = text.split('\0').map(str::trim).collect();
    if values.len() == 2 * files + 1 && values.last() == Some(&"") {
        values.pop();
    }
    if values.len() != 2 * files {
        return None;
    }
    let parse = |v: &str| DateTime::parse_from_str(v, "%Y-%m-%d %H:%M:%S %z").ok().map(|t| t.to_utc());
    Some(values.chunks(2).map(|pair| parse(pair[0]).max(parse(pair[1]))).collect())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::model::{ActionKind, Category, DetectorKind, Risk};
    use crate::process::ProcessSnapshot;
    use crate::size::Sizer;

    const MIB: usize = 1024 * 1024;

    fn rule(params: serde_json::Value) -> RuleDef {
        RuleDef {
            id: "downloads.old".into(),
            title: "Old downloads".into(),
            description: String::new(),
            category: Category::Downloads,
            risk: Risk::Manual,
            detector: DetectorKind::OlderThan,
            roots: vec!["~/Downloads".into()],
            params,
            action: ActionKind::Trash,
            requires_quit: vec![],
            regenerates: false,
            enabled: true,
        }
    }

    fn days_ago(days: u64) -> SystemTime {
        SystemTime::now() - Duration::from_secs(days * 86_400)
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        fs::File::open(path).unwrap().set_modified(when).unwrap();
    }

    fn file(path: &Path, len: usize, when: SystemTime) {
        fs::write(path, vec![1u8; len]).unwrap();
        set_mtime(path, when);
    }

    /// ~/Downloads with: old.DMG (90 d), new.zip (today), old-dir/ (all 90 d), mixed-dir/ (dir old,
    /// one file new), tiny.txt (90 d, 10 bytes), a symlink and .DS_Store.
    fn downloads() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dl = home.path().join("Downloads");
        fs::create_dir_all(dl.join("old-dir/sub")).unwrap();
        fs::create_dir_all(dl.join("mixed-dir")).unwrap();
        file(&dl.join("old.DMG"), 2 * MIB, days_ago(90));
        file(&dl.join("new.zip"), 2 * MIB, SystemTime::now());
        file(&dl.join("old-dir/sub/a.sql"), 2 * MIB, days_ago(90));
        for d in ["old-dir/sub", "old-dir"] {
            set_mtime(&dl.join(d), days_ago(90));
        }
        file(&dl.join("mixed-dir/fresh.txt"), MIB, days_ago(2));
        set_mtime(&dl.join("mixed-dir"), days_ago(90));
        file(&dl.join("tiny.txt"), 10, days_ago(90));
        file(&dl.join(".DS_Store"), 10, days_ago(90));
        std::os::unix::fs::symlink(dl.join("old.DMG"), dl.join("link.dmg")).unwrap();
        home
    }

    fn run(home: &Path, rule: &RuleDef, spotlight: &Spotlight) -> Result<Vec<String>, CoreError> {
        let (sizer, processes, config) = (Sizer::new(), ProcessSnapshot::default(), Config::default());
        let ctx =
            ScanContext { home: home.into(), now: Utc::now(), sizer: &sizer, processes: &processes, config: &config };
        let mut names: Vec<String> = detect_with(rule, &ctx, spotlight)?
            .into_iter()
            .map(|f| Path::new(f.path.as_deref().unwrap()).file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Ok(names)
    }

    fn no_spotlight(paths: &[PathBuf], _: &ScanContext) -> Vec<Option<DateTime<Utc>>> {
        vec![None; paths.len()]
    }

    #[test]
    fn finds_old_children_without_extension_filter() {
        let home = downloads();
        let names = run(home.path(), &rule(serde_json::json!({})), &no_spotlight).unwrap();
        assert_eq!(names, vec!["old-dir", "old.DMG", "tiny.txt"]);
        let names = run(home.path(), &rule(serde_json::json!({ "min_bytes": MIB })), &no_spotlight).unwrap();
        assert_eq!(names, vec!["old-dir", "old.DMG"], "min_bytes drops tiny.txt");
    }

    #[test]
    fn extension_filter_is_case_insensitive_suffix() {
        let home = downloads();
        let names = run(home.path(), &rule(serde_json::json!({ "extensions": ["dmg", ".txt"] })), &no_spotlight);
        assert_eq!(names.unwrap(), vec!["old.DMG", "tiny.txt"]);
        let names = run(home.path(), &rule(serde_json::json!({ "extensions": ["zip"] })), &no_spotlight);
        assert!(names.unwrap().is_empty(), "new.zip is recent");
    }

    #[test]
    fn days_param_moves_the_cutoff() {
        let home = downloads();
        let names = run(home.path(), &rule(serde_json::json!({ "days": 1 })), &no_spotlight).unwrap();
        assert_eq!(names, vec!["mixed-dir", "old-dir", "old.DMG", "tiny.txt"]);
        let names = run(home.path(), &rule(serde_json::json!({ "days": 365 })), &no_spotlight).unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn spotlight_last_used_wins_over_old_mtime() {
        let home = downloads();
        let recently_opened = |paths: &[PathBuf], _: &ScanContext| {
            paths
                .iter()
                .map(|p| p.ends_with("old.DMG").then(|| Utc::now() - chrono::Duration::days(3)))
                .collect::<Vec<_>>()
        };
        let names = run(home.path(), &rule(serde_json::json!({})), &recently_opened).unwrap();
        assert_eq!(names, vec!["old-dir", "tiny.txt"]);
    }

    #[test]
    fn last_used_and_reason_are_set() {
        let home = downloads();
        let (sizer, processes, config) = (Sizer::new(), ProcessSnapshot::default(), Config::default());
        let ctx = ScanContext {
            home: home.path().into(),
            now: Utc::now(),
            sizer: &sizer,
            processes: &processes,
            config: &config,
        };
        let rule = rule(serde_json::json!({ "extensions": ["dmg"] }));
        let found = detect_with(&rule, &ctx, &no_spotlight).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(age_days(ctx.now, found[0].last_used.unwrap()), 90);
        assert!(found[0].reason.starts_with("Not opened or modified for 90 days (since "), "{}", found[0].reason);
        assert!(found[0].bytes >= 2 * MIB as u64);
    }

    #[test]
    fn invalid_params_are_rule_errors() {
        let home = downloads();
        for params in [
            serde_json::json!({ "days": "30" }),
            serde_json::json!({ "days": -1 }),
            serde_json::json!({ "extensions": "dmg" }),
            serde_json::json!({ "min_bytes": 1.5 }),
            serde_json::json!({ "dayz": 3 }),
        ] {
            let err = run(home.path(), &rule(params.clone()), &no_spotlight).unwrap_err();
            assert!(matches!(err, CoreError::Rule(_)), "{params}: {err}");
        }
    }

    #[test]
    fn parses_mdls_raw_output() {
        let out = [
            "2025-09-02 13:06:13 +0000",
            "(null)",
            "(null)",
            "(null)",
            "2025-01-01 00:00:00 +0000",
            "2026-03-04 05:06:07 +0100",
        ]
        .join("\0");
        let out = out.as_bytes();
        let dates = parse_mdls(out, 3).unwrap();
        assert_eq!(dates[0].unwrap().to_rfc3339(), "2025-09-02T13:06:13+00:00");
        assert_eq!(dates[1], None);
        assert_eq!(dates[2].unwrap().to_rfc3339(), "2026-03-04T04:06:07+00:00", "newer of the two, in UTC");
        assert_eq!(parse_mdls(b"(null)\0(null)\0", 1).unwrap(), vec![None], "trailing NUL tolerated");
        assert_eq!(parse_mdls(b"", 2), None);
        assert_eq!(parse_mdls(b"(null)\0(null)", 2), None, "count mismatch");
    }

    #[test]
    fn real_mdls_on_tempdir_does_not_fail_the_rule() {
        let home = downloads();
        let names = run(home.path(), &rule(serde_json::json!({})), &spotlight_dates).unwrap();
        assert_eq!(names, vec!["old-dir", "old.DMG", "tiny.txt"]);
    }
}
