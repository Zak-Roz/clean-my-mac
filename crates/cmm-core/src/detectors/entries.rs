//! `entries` detector — one finding per top-level entry of each root (files, folders, bundles and
//! symlinks alike), for browsing a folder such as `~/Downloads` in the desktop app.
//!
//! A folder is sized as one unit with the shared sizer; symlinks are never followed (a link is
//! listed and sized as the link itself). Finder litter (`.DS_Store`, `.localized`, `Icon\r`) is
//! skipped. An unfinished download (`.download`, `.crdownload`, `.part`, …) gets a blocker, so it
//! cannot be cleaned while a browser may still be writing it.
//!
//! Each finding carries [`crate::model::FileMeta`] (Spotlight dates, type and origin, see
//! `filemeta.rs`), and `last_used` is the newest of: the mtime of anything inside, the last-opened
//! date and the date added — the same notion of "used" as the `older-than` detector.
//!
//! Params: `min_bytes` only (read by the scan, which uses it instead of the global
//! `min_finding_bytes`; `downloads.browse` sets 0 so every entry is listed). Unknown params are an
//! error.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::detectors::support::for_each_root;
use crate::detectors::{Detector, age_days, describe_age, make_finding};
use crate::error::CoreError;
use crate::filemeta;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct EntriesDetector;

/// Names never listed (lower case): Finder's own bookkeeping files.
const SKIPPED: &[&str] = &[".ds_store", ".localized", "icon\r"];

/// Suffixes of downloads a browser is still writing (Safari, Chrome/Edge/Brave, Firefox, Opera, …).
pub const PARTIAL_DOWNLOAD_SUFFIXES: &[&str] = &[".download", ".crdownload", ".part", ".partial", ".opdownload"];

impl Detector for EntriesDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        check_params(rule)?;
        let mut findings = Vec::new();
        for_each_root(rule, ctx, |root| {
            if !root.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                return Ok(());
            }
            let entries = list_entries(root)?;
            findings.extend(entries.par_iter().map(|path| entry_finding(rule, ctx, path)).collect::<Vec<_>>());
            Ok(())
        })?;
        Ok(findings)
    }
}

fn check_params(rule: &RuleDef) -> Result<(), CoreError> {
    let map = rule
        .params
        .as_object()
        .ok_or_else(|| CoreError::Rule(format!("rule `{}`: params must be a table", rule.id)))?;
    if let Some(key) = map.keys().find(|k| k.as_str() != "min_bytes") {
        return Err(CoreError::Rule(format!("rule `{}`: unknown param `{key}` for entries", rule.id)));
    }
    if map.get("min_bytes").is_some_and(|v| v.as_u64().is_none()) {
        return Err(CoreError::Rule(format!("rule `{}`: params.min_bytes must be a whole number", rule.id)));
    }
    Ok(())
}

/// Direct children of `dir` (sorted), minus Finder litter. Symlinks are kept (as links).
fn list_entries(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if SKIPPED.contains(&name.as_str()) {
            continue;
        }
        out.push(entry.path());
    }
    out.sort();
    Ok(out)
}

/// The file name ends with a partial-download suffix (case-insensitive).
pub fn is_partial_download(name: &str) -> bool {
    let lower = name.to_lowercase();
    PARTIAL_DOWNLOAD_SUFFIXES.iter().any(|suffix| lower.len() > suffix.len() && lower.ends_with(suffix))
}

fn entry_finding(rule: &RuleDef, ctx: &ScanContext, path: &Path) -> Finding {
    let size = ctx.sizer.size_of(path);
    let meta = filemeta::read(path);
    let reason = describe(ctx, &meta);
    let mut finding = make_finding(rule, &path.to_string_lossy(), Some(path), &size, reason);
    finding.last_used = size.newest_mtime.max(meta.last_opened).max(meta.date_added);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if is_partial_download(&name) {
        finding.add_blocker("Download not finished: let it complete or cancel it in the browser first");
    }
    finding.meta = Some(meta);
    finding
}

/// "Added 43 days ago · never opened", "Added today · opened 2 days ago", "Added 3 days ago".
fn describe(ctx: &ScanContext, meta: &crate::model::FileMeta) -> String {
    let added = match meta.date_added {
        Some(t) => format!("Added {}", describe_age(age_days(ctx.now, t))),
        None => String::from("Date added unknown"),
    };
    match (meta.last_opened, meta.indexed) {
        (Some(t), _) => format!("{added} · opened {}", describe_age(age_days(ctx.now, t))),
        (None, true) => format!("{added} · never opened"),
        (None, false) => added,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::detectors::support::testutil::{TestEnv, file, rule};
    use crate::model::{ActionKind, DetectorKind};

    fn browse_rule(params: serde_json::Value) -> RuleDef {
        let mut r = rule("downloads.browse", DetectorKind::Entries, ActionKind::Trash, &["~/Downloads"], params);
        r.browse_only = true;
        r
    }

    /// ~/Downloads with a file, a folder (two files), a bundle-like folder, a symlink to a big file
    /// outside, Finder litter and two unfinished downloads.
    fn downloads() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dl = home.path().join("Downloads");
        file(&dl.join("report.pdf"), 3000, 40);
        file(&dl.join("photos/a.jpg"), 5000, 10);
        file(&dl.join("photos/b.jpg"), 7000, 2);
        file(&dl.join("Tool.app/Contents/MacOS/tool"), 1000, 5);
        file(&dl.join(".DS_Store"), 10, 1);
        file(&dl.join(".localized"), 0, 1);
        file(&dl.join("Icon\r"), 0, 1);
        file(&dl.join("movie.mp4.crdownload"), 2000, 0);
        fs::create_dir_all(dl.join("Installer.dmg.download")).unwrap();
        file(&home.path().join("elsewhere/huge.bin"), 1_000_000, 1);
        std::os::unix::fs::symlink(home.path().join("elsewhere/huge.bin"), dl.join("shortcut.bin")).unwrap();
        home
    }

    fn by_name(findings: &[Finding]) -> std::collections::BTreeMap<String, &Finding> {
        findings.iter().map(|f| (f.label.clone(), f)).collect()
    }

    #[test]
    fn one_finding_per_top_level_entry() {
        let home = downloads();
        let env = TestEnv::new(home.path());
        let found = EntriesDetector.detect(&browse_rule(serde_json::json!({ "min_bytes": 0 })), &env.ctx()).unwrap();
        let names: Vec<String> = by_name(&found).into_keys().collect();
        assert_eq!(
            names,
            vec!["Installer.dmg.download", "Tool.app", "movie.mp4.crdownload", "photos", "report.pdf", "shortcut.bin"],
            "Finder litter skipped, nothing below the top level listed"
        );
        let dl = home.path().join("Downloads");
        let f = by_name(&found)["report.pdf"];
        assert_eq!(f.id, format!("downloads.browse:{}", dl.join("report.pdf").display()));
        assert_eq!((f.is_dir, f.files), (false, 1));
        assert!(f.bytes >= 3000);
        assert_eq!(f.action, ActionKind::Trash);
        assert!(!f.selected_by_default);
    }

    #[test]
    fn folders_are_sized_as_one_unit() {
        let home = downloads();
        let env = TestEnv::new(home.path());
        let found = EntriesDetector.detect(&browse_rule(serde_json::json!({})), &env.ctx()).unwrap();
        let photos = by_name(&found)["photos"];
        assert!(photos.is_dir);
        assert_eq!(photos.files, 2);
        assert!(photos.logical_bytes >= 12_000, "{}", photos.logical_bytes);
        let app = by_name(&found)["Tool.app"];
        assert!(app.is_dir && app.files == 1, "a bundle is one entry");
        // Newest mtime inside (2 days) wins over the folder's older contents.
        let used = photos.last_used.unwrap();
        assert!(age_days(env.now, used) <= 2, "{used}");
    }

    #[test]
    fn symlinks_are_listed_but_not_followed() {
        let home = downloads();
        let env = TestEnv::new(home.path());
        let found = EntriesDetector.detect(&browse_rule(serde_json::json!({})), &env.ctx()).unwrap();
        let link = by_name(&found)["shortcut.bin"];
        assert!(!link.is_dir);
        assert!(link.logical_bytes < 1_000, "sized as the link, not the 1 MB target: {}", link.logical_bytes);
        let meta = link.meta.as_ref().unwrap();
        assert!(!meta.indexed && meta.content_type.is_none(), "no Spotlight lookup through a link");
        assert!(link.blockers.is_empty());
    }

    #[test]
    fn unfinished_downloads_are_blocked() {
        let home = downloads();
        let env = TestEnv::new(home.path());
        let found = EntriesDetector.detect(&browse_rule(serde_json::json!({})), &env.ctx()).unwrap();
        let found = by_name(&found);
        for name in ["movie.mp4.crdownload", "Installer.dmg.download"] {
            let f = found[name];
            assert_eq!(f.blockers.len(), 1, "{name}");
            assert!(f.blockers[0].starts_with("Download not finished"), "{:?}", f.blockers);
            assert!(!f.selected_by_default);
        }
        assert!(found["report.pdf"].blockers.is_empty());
        assert!(is_partial_download("Big.ISO.PART"));
        assert!(is_partial_download("x.opdownload") && is_partial_download("y.partial"));
        assert!(!is_partial_download(".part"), "a bare suffix is a name, not a partial download");
        assert!(!is_partial_download("party.zip") && !is_partial_download("download"));
    }

    #[test]
    fn metadata_and_reason_are_set() {
        let home = downloads();
        let env = TestEnv::new(home.path());
        let found = EntriesDetector.detect(&browse_rule(serde_json::json!({})), &env.ctx()).unwrap();
        let pdf = by_name(&found)["report.pdf"];
        let meta = pdf.meta.as_ref().expect("entries findings carry metadata");
        assert!(meta.created.is_some() && meta.modified.is_some() && meta.date_added.is_some());
        // A temp dir is outside the Spotlight index: the open date is unknown, not "never".
        assert!(!meta.indexed && meta.last_opened.is_none());
        assert!(pdf.reason.starts_with("Added "), "{}", pdf.reason);
        assert!(!pdf.reason.contains("never opened"), "{}", pdf.reason);
        assert!(pdf.last_used >= meta.date_added);
    }

    #[test]
    fn missing_root_yields_nothing_and_bad_params_fail() {
        let home = tempfile::tempdir().unwrap();
        let env = TestEnv::new(home.path());
        assert!(EntriesDetector.detect(&browse_rule(serde_json::json!({})), &env.ctx()).unwrap().is_empty());
        for params in [serde_json::json!({ "days": 3 }), serde_json::json!({ "min_bytes": "0" })] {
            let err = EntriesDetector.detect(&browse_rule(params.clone()), &env.ctx()).unwrap_err();
            assert!(matches!(err, CoreError::Rule(_)), "{params}: {err}");
        }
    }
}
