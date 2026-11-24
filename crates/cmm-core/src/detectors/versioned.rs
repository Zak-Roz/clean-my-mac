//! `versioned-siblings` detector — siblings differing only by date/version tokens; all but the newest `keep` (owned by S2).
//!
//! Params: `keep` (default `config.keep_versions`), `depth` (default 3: the root's entries and two
//! levels below). Siblings are grouped by [`naming::name_info`] key (and file vs directory); a group
//! needs ≥ 2 members, at least one token, and letters in the non-token stem — except pure version
//! names (`2.1.330`, as in Claude Code's `versions/`), which group on their own. Newest = parsed date when
//! every member has one, else version when every member has one, else mtime. Members of a group are
//! never walked into.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::detectors::naming::{NameInfo, name_info};
use crate::detectors::support::{Entry, for_each_root, mtime_of, param_u64, walk_dirs};
use crate::detectors::{Detector, make_finding};
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct VersionedSiblingsDetector;

impl Detector for VersionedSiblingsDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let keep = param_u64(rule, "keep").unwrap_or(u64::from(ctx.config.keep_versions)).max(1) as usize;
        let depth = param_u64(rule, "depth").unwrap_or(3).max(1) as usize;
        let mut findings = Vec::new();
        for_each_root(rule, ctx, |root| {
            walk_dirs(root, depth, &mut |dir, entries| {
                let (found, members) = groups_in(rule, ctx, dir, entries, keep);
                findings.extend(found);
                members
            })
        })?;
        Ok(findings)
    }
}

struct Member<'a> {
    entry: &'a Entry,
    info: NameInfo,
    mtime: Option<DateTime<Utc>>,
}

/// Version groups among one directory's entries: findings for the surplus members, plus the names
/// of every grouped directory (the walker must not descend into versions).
fn groups_in(
    rule: &RuleDef,
    ctx: &ScanContext,
    dir: &Path,
    entries: &[Entry],
    keep: usize,
) -> (Vec<Finding>, Vec<String>) {
    let mut groups: BTreeMap<(bool, String), Vec<Member>> = BTreeMap::new();
    for entry in entries.iter().filter(|e| !e.is_hidden() && (e.is_file() || e.is_dir())) {
        let info = name_info(&entry.name);
        groups.entry((entry.is_dir(), info.key.clone())).or_default().push(Member {
            entry,
            info,
            mtime: mtime_of(&entry.meta),
        });
    }

    let mut findings = Vec::new();
    let mut grouped_dirs = Vec::new();
    for ((is_dir, _), mut members) in groups {
        // Letters in the stem keep e.g. screenshots ("2025-04-12 14.05.31.jpg") from grouping;
        // bare versions ("2.1.330") are the exception.
        let named = |m: &Member| m.info.has_stem_letters || (m.info.version.is_some() && m.info.date.is_none());
        let versioned = members.iter().any(|m| m.info.has_token) && members.iter().all(named);
        if members.len() < 2 || !versioned {
            continue;
        }
        if is_dir {
            grouped_dirs.extend(members.iter().map(|m| m.entry.name.clone()));
        }
        sort_newest_first(&mut members);
        if members.len() <= keep {
            continue;
        }
        let kept: Vec<&str> = members[..keep].iter().map(|m| m.entry.name.as_str()).collect();
        let reason = format!("{} versions; keeping {}", members.len(), kept.join(", "));
        let group = format!("{}/{}", dir.display(), members[0].info.pattern);
        for m in &members[keep..] {
            let size = ctx.sizer.size_of(&m.entry.path);
            let mut f = make_finding(rule, &m.entry.path.to_string_lossy(), Some(&m.entry.path), &size, reason.clone());
            f.group = Some(group.clone());
            findings.push(f);
        }
    }
    (findings, grouped_dirs)
}

fn sort_newest_first(members: &mut [Member]) {
    let all_dated = members.iter().all(|m| m.info.date.is_some());
    let all_versioned = members.iter().all(|m| m.info.version.is_some());
    let date = |m: &Member| m.info.date.unwrap_or(NaiveDateTime::MIN);
    members.sort_by(|a, b| {
        let primary = if all_dated {
            date(b).cmp(&date(a))
        } else if all_versioned {
            b.info.version.cmp(&a.info.version)
        } else {
            Ordering::Equal
        };
        primary.then_with(|| b.mtime.cmp(&a.mtime)).then_with(|| b.entry.name.cmp(&a.entry.name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::support::testutil::{TestEnv, age, file, rule};
    use crate::model::{ActionKind, DetectorKind};

    fn detect(home: &Path, params: serde_json::Value) -> Vec<Finding> {
        let env = TestEnv::new(home);
        let rule =
            rule("downloads.versioned", DetectorKind::VersionedSiblings, ActionKind::Trash, &["~/Downloads"], params);
        let mut found = VersionedSiblingsDetector.detect(&rule, &env.ctx()).unwrap();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found
    }

    fn labels(found: &[Finding]) -> Vec<&str> {
        found.iter().map(|f| f.label.as_str()).collect()
    }

    #[test]
    fn mongo_and_mongo_eu_are_separate_groups() {
        let home = tempfile::tempdir().unwrap();
        let dumps = home.path().join("Downloads/backups");
        // mtimes deliberately disagree with the dates in the names: the parsed date wins.
        for (i, name) in
            ["mongo_28_01_2025", "mongo_05_03_2025", "mongo_17_05_2025", "mongo_11_08_2025"].iter().enumerate()
        {
            file(&dumps.join(name).join("db.bson"), 4096, 10);
            age(&dumps.join(name), 10 + i as u64);
        }
        for name in ["mongo_eu_28_01_2025", "mongo_eu_05_03_2025", "mongo_eu_11_08_2025"] {
            file(&dumps.join(name).join("db.bson"), 4096, 10);
        }
        file(&dumps.join("export-notes.json"), 100, 1);

        let found = detect(home.path(), serde_json::json!({}));
        assert_eq!(
            labels(&found),
            vec![
                "mongo_05_03_2025",
                "mongo_17_05_2025",
                "mongo_28_01_2025",
                "mongo_eu_05_03_2025",
                "mongo_eu_28_01_2025"
            ]
        );
        let mongo = &found[0];
        assert_eq!(mongo.reason, "4 versions; keeping mongo_11_08_2025");
        assert_eq!(mongo.group.as_deref(), Some(format!("{}/mongo_*", dumps.display()).as_str()));
        assert!(mongo.bytes >= 4096 && mongo.files == 1);
        let eu = &found[3];
        assert_eq!(eu.reason, "3 versions; keeping mongo_eu_11_08_2025");
        assert_eq!(eu.group.as_deref(), Some(format!("{}/mongo_eu_*", dumps.display()).as_str()));
        assert!(found.iter().all(|f| f.id.starts_with("downloads.versioned:/") && !f.selected_by_default));
    }

    #[test]
    fn iso_timestamp_dump_series_keeps_newest() {
        let home = tempfile::tempdir().unwrap();
        let dl = home.path().join("Downloads");
        for name in [
            "db-backup-dev-2025-02-03T15-00-00-008Z.dump",
            "db-backup-dev-2025-03-17T09-00-00-004Z.dump",
            "db-backup-dev-2025-06-20T11-00-00-030Z.dump",
            "db-backup-dev-2025-05-06T16-00-00-031Z.dump",
            "db-backup-prod-2025-02-12T07-00-06-303Z.dump",
            "2025-04-12 14.05.31.jpg",
            "2025-04-12 14.11.09.jpg",
        ] {
            file(&dl.join(name), 1000, 3);
        }
        let found = detect(home.path(), serde_json::json!({}));
        assert_eq!(
            labels(&found),
            vec![
                "db-backup-dev-2025-02-03T15-00-00-008Z.dump",
                "db-backup-dev-2025-03-17T09-00-00-004Z.dump",
                "db-backup-dev-2025-05-06T16-00-00-031Z.dump",
            ]
        );
        assert_eq!(found[0].reason, "4 versions; keeping db-backup-dev-2025-06-20T11-00-00-030Z.dump");
    }

    #[test]
    fn keep_param_copies_and_mtime_fallback() {
        let home = tempfile::tempdir().unwrap();
        let dl = home.path().join("Downloads");
        file(&dl.join("Report.pdf"), 10, 30);
        file(&dl.join("Report (1).pdf"), 10, 20);
        file(&dl.join("Report copy.pdf"), 10, 5);
        file(&dl.join("app-1.2.0.zip"), 10, 1);
        file(&dl.join("app-1.10.0.zip"), 10, 9);
        file(&dl.join("app-1.9.0.zip"), 10, 3);
        // Single member, depth limit: nested deeper than `depth` is ignored.
        file(&dl.join("a/b/c/x_2026-01-01.txt"), 10, 3);
        file(&dl.join("a/b/c/x_2026-02-01.txt"), 10, 3);

        let found = detect(home.path(), serde_json::json!({"keep": 2}));
        // Copies: no dates/versions on every member -> newest mtime wins.
        // Versions: 1.10.0 > 1.9.0 > 1.2.0 regardless of mtime.
        assert_eq!(labels(&found), vec!["Report.pdf", "app-1.2.0.zip"]);
        assert_eq!(found[0].reason, "3 versions; keeping Report copy.pdf, Report (1).pdf");
        assert_eq!(found[1].reason, "3 versions; keeping app-1.10.0.zip, app-1.9.0.zip");

        let found = detect(home.path(), serde_json::json!({"keep": 1, "depth": 4}));
        assert!(labels(&found).contains(&"x_2026-01-01.txt"));
    }

    #[test]
    fn bare_version_names_group() {
        let home = tempfile::tempdir().unwrap();
        let versions = home.path().join("Downloads");
        for (name, days) in [("2.1.9", 1), ("2.1.330", 30), ("2.1.331", 20), ("2.0.99", 2)] {
            file(&versions.join(name), 100, days);
        }
        file(&versions.join("2025-04-12 14.05.31.jpg"), 100, 1);
        file(&versions.join("2025-04-12 14.11.09.jpg"), 100, 1);
        let found = detect(home.path(), serde_json::json!({"keep": 2}));
        assert_eq!(labels(&found), vec!["2.0.99", "2.1.9"]);
        assert_eq!(found[0].reason, "4 versions; keeping 2.1.331, 2.1.330");
    }

    #[test]
    fn missing_root_is_not_an_error() {
        let home = tempfile::tempdir().unwrap();
        assert!(detect(home.path(), serde_json::json!({})).is_empty());
    }
}
