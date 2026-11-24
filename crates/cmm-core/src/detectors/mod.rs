//! Detector trait, registry and shared helpers (owned by S1; individual detectors per §9).

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::error::CoreError;
use crate::model::{DetectorKind, Finding, Risk, RuleDef};
use crate::scan::ScanContext;
use crate::size::SizeInfo;

pub mod archives;
pub mod docker;
pub mod git_worktrees;
pub mod installers;
pub mod older_than;
pub mod paths;
pub mod simulators;
pub mod stale_artifacts;
pub mod versioned;

/// Turns one rule into findings. Must be read-only: detectors never modify the filesystem.
pub trait Detector: Send + Sync {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError>;
}

/// The detector implementing `kind`.
pub fn detector_for(kind: DetectorKind) -> &'static dyn Detector {
    match kind {
        DetectorKind::Paths => &paths::PathsDetector,
        DetectorKind::OlderThan => &older_than::OlderThanDetector,
        DetectorKind::StaleArtifacts => &stale_artifacts::StaleArtifactsDetector,
        DetectorKind::VersionedSiblings => &versioned::VersionedSiblingsDetector,
        DetectorKind::GitWorktrees => &git_worktrees::GitWorktreesDetector,
        DetectorKind::Installers => &installers::InstallersDetector,
        DetectorKind::RedundantArchives => &archives::RedundantArchivesDetector,
        DetectorKind::SimulatorRuntimes => &simulators::SimulatorRuntimesDetector,
        DetectorKind::Docker => &docker::DockerDetector,
    }
}

/// Shared constructor: id `"{rule_id}:{key}"`, risk/category/action copied from the rule,
/// label = file name of `path` (or `key`), no blockers, `selected_by_default` only for `Risk::Safe`.
/// Detectors then set `command`, `group`, `label` and add blockers via [`Finding::add_blocker`].
pub fn make_finding(rule: &RuleDef, key: &str, path: Option<&Path>, size: &SizeInfo, reason: String) -> Finding {
    let label = path
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| key.to_string());
    let blockers: Vec<String> = Vec::new();
    Finding {
        id: format!("{}:{}", rule.id, key),
        rule_id: rule.id.clone(),
        category: rule.category,
        risk: rule.risk,
        label,
        path: path.map(|p| p.to_string_lossy().into_owned()),
        bytes: size.bytes,
        logical_bytes: size.logical_bytes,
        files: size.files,
        last_used: size.newest_mtime,
        reason,
        action: rule.action,
        command: None,
        selected_by_default: rule.risk == Risk::Safe && blockers.is_empty(),
        blockers,
        group: None,
    }
}

/// Whole days between `then` and `now` (0 if `then` is in the future).
pub fn age_days(now: DateTime<Utc>, then: DateTime<Utc>) -> i64 {
    (now - then).num_days().max(0)
}

/// "today", "1 day ago", "143 days ago" — for deterministic `Finding::reason` texts.
pub fn describe_age(days: i64) -> String {
    match days {
        ..=0 => String::from("today"),
        1 => String::from("1 day ago"),
        n => format!("{n} days ago"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::builtin_rules;

    #[test]
    fn ages_are_whole_days() {
        let now = DateTime::parse_from_rfc3339("2026-11-24T10:00:00Z").unwrap().to_utc();
        assert_eq!(age_days(now, now - chrono::Duration::hours(47)), 1);
        assert_eq!(age_days(now, now + chrono::Duration::days(3)), 0);
        assert_eq!(describe_age(0), "today");
        assert_eq!(describe_age(1), "1 day ago");
        assert_eq!(describe_age(143), "143 days ago");
    }

    #[test]
    fn make_finding_id_and_defaults() {
        let rules = builtin_rules().unwrap();
        let npm = rules.iter().find(|r| r.id == "cache.npm").unwrap();
        let size = SizeInfo { bytes: 4096, logical_bytes: 100, files: 1, newest_mtime: None };
        let path = Path::new("/Users/test/.npm/_cacache");
        let f = make_finding(npm, &path.to_string_lossy(), Some(path), &size, "cache".into());
        assert_eq!(f.id, "cache.npm:/Users/test/.npm/_cacache");
        assert_eq!(f.rule_id, "cache.npm");
        assert_eq!(f.label, "_cacache");
        assert_eq!(f.path.as_deref(), Some("/Users/test/.npm/_cacache"));
        assert_eq!((f.bytes, f.logical_bytes, f.files), (4096, 100, 1));
        assert_eq!(f.risk, npm.risk);
        assert_eq!(f.action, npm.action);
        assert!(f.selected_by_default, "safe rule without blockers is preselected");

        let vm = rules.iter().find(|r| r.id == "ai.claude-desktop.vm-bundles").unwrap();
        let f = make_finding(vm, "docker", None, &size, "x".into());
        assert_eq!(f.id, "ai.claude-desktop.vm-bundles:docker");
        assert_eq!(f.label, "docker");
        assert!(!f.selected_by_default, "review risk is never preselected");
    }

    #[test]
    fn every_kind_has_a_detector() {
        use DetectorKind::*;
        for kind in [
            Paths,
            OlderThan,
            StaleArtifacts,
            VersionedSiblings,
            GitWorktrees,
            Installers,
            RedundantArchives,
            SimulatorRuntimes,
            Docker,
        ] {
            let _ = detector_for(kind);
        }
    }
}
