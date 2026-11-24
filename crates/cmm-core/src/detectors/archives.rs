//! `redundant-archives` detector — archives whose extracted sibling exists (owned by S2).
//!
//! `X.zip/.tar.gz/.tgz/.tar.bz2/.tar.xz/.tar/.rar/.7z` is redundant when a non-empty sibling `X` or
//! `X.app` exists. Names are compared after version-token normalization
//! ([`naming::comparison_key`]), so `Archive_0.1.1_mac.zip` matches `Archive_mac/`.
//! Params: `depth` (default 3).

use std::collections::BTreeMap;

use crate::detectors::naming::comparison_key;
use crate::detectors::support::{Entry, for_each_root, param_u64, walk_dirs};
use crate::detectors::{Detector, make_finding};
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct RedundantArchivesDetector;

/// Longest first, so `.tar.gz` wins over `.gz`-less matching of `.tar`.
const ARCHIVE_SUFFIXES: &[&str] =
    &[".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".tbz2", ".txz", ".tar", ".zip", ".rar", ".7z"];

/// `Some(stem)` when `name` is an archive (`foo.tar.gz` -> `foo`).
fn archive_stem(name: &str) -> Option<&str> {
    let lower = name.to_ascii_lowercase();
    ARCHIVE_SUFFIXES
        .iter()
        .find(|s| lower.ends_with(*s) && lower.len() > s.len())
        .map(|s| &name[..name.len() - s.len()])
}

/// A sibling only proves the archive redundant if it has content.
fn has_content(entry: &Entry) -> bool {
    if entry.is_dir() {
        std::fs::read_dir(&entry.path).is_ok_and(|mut it| it.next().is_some())
    } else {
        entry.is_file() && entry.meta.len() > 0
    }
}

impl Detector for RedundantArchivesDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let depth = param_u64(rule, "depth").unwrap_or(3).max(1) as usize;
        let mut findings = Vec::new();
        for_each_root(rule, ctx, |root| {
            walk_dirs(root, depth, &mut |_, entries| {
                let visible = entries.iter().filter(|e| !e.is_hidden());
                let mut extracted: BTreeMap<String, &Entry> = BTreeMap::new();
                for entry in visible.clone().filter(|e| archive_stem(&e.name).is_none()) {
                    let name = entry.name.strip_suffix(".app").unwrap_or(&entry.name);
                    extracted.entry(comparison_key(name)).or_insert(entry);
                }
                for entry in visible.filter(|e| e.is_file()) {
                    let Some(stem) = archive_stem(&entry.name) else { continue };
                    let Some(sibling) = extracted.get(&comparison_key(stem)) else { continue };
                    if !has_content(sibling) {
                        continue;
                    }
                    let reason = if sibling.name.strip_suffix(".app").unwrap_or(&sibling.name) == stem {
                        format!("Already extracted: “{}” is next to it", sibling.name)
                    } else {
                        format!("Extracted copy “{}” is next to it (version may differ)", sibling.name)
                    };
                    let size = ctx.sizer.size_of(&entry.path);
                    findings.push(make_finding(rule, &entry.path.to_string_lossy(), Some(&entry.path), &size, reason));
                }
                Vec::new()
            })
        })?;
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::support::testutil::{TestEnv, file, rule};
    use crate::model::{ActionKind, DetectorKind};

    #[test]
    fn stems() {
        assert_eq!(archive_stem("a.tar.gz"), Some("a"));
        assert_eq!(archive_stem("Archive_0.1.1_mac.zip"), Some("Archive_0.1.1_mac"));
        assert_eq!(archive_stem("X.TGZ"), Some("X"));
        assert_eq!(archive_stem(".zip"), None);
        assert_eq!(archive_stem("notes.txt"), None);
    }

    #[test]
    fn archives_with_extracted_siblings() {
        let home = tempfile::tempdir().unwrap();
        let dl = home.path().join("Downloads");
        file(&dl.join("Archive_0.1.1_mac.zip"), 3000, 5);
        file(&dl.join("Archive_mac/Archive.app/Contents/Info.plist"), 10, 5);
        file(&dl.join("Album-art-SVG.zip"), 3000, 5);
        file(&dl.join("Album-art-SVG/a.svg"), 10, 5);
        file(&dl.join("Tool.tar.gz"), 3000, 5);
        file(&dl.join("Tool.app/Contents/Info.plist"), 10, 5);
        // No sibling / empty sibling / archive-only siblings: not redundant.
        file(&dl.join("Lonely.zip"), 3000, 5);
        file(&dl.join("Empty.zip"), 3000, 5);
        std::fs::create_dir_all(dl.join("Empty")).unwrap();
        file(&dl.join("Pair.zip"), 3000, 5);
        file(&dl.join("Pair (1).zip"), 3000, 5);
        // Nested one level down.
        file(&dl.join("proj/data.7z"), 3000, 5);
        file(&dl.join("proj/data/x.csv"), 10, 5);

        let env = TestEnv::new(home.path());
        let r = rule(
            "downloads.archives",
            DetectorKind::RedundantArchives,
            ActionKind::Trash,
            &["~/Downloads"],
            serde_json::json!({}),
        );
        let mut found = RedundantArchivesDetector.detect(&r, &env.ctx()).unwrap();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        let labels: Vec<_> = found.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, vec!["Album-art-SVG.zip", "Archive_0.1.1_mac.zip", "Tool.tar.gz", "data.7z"]);
        assert_eq!(found[0].reason, "Already extracted: “Album-art-SVG” is next to it");
        assert_eq!(found[1].reason, "Extracted copy “Archive_mac” is next to it (version may differ)");
        assert_eq!(found[2].reason, "Already extracted: “Tool.app” is next to it");
        assert!(found[0].bytes >= 3000);
    }
}
