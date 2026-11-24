//! Core data model shared by the CLI, the desktop app (via ts-rs bindings) and history.
//!
//! This file is the contract (docs/ARCHITECTURE.md §2). Do not change public types without the
//! coordinator's approval. Structs serialize with camelCase fields, enums as kebab-case strings.
//! TypeScript bindings: `cargo test -p cmm-core export_bindings` -> `apps/desktop/src/bindings/`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How dangerous it is to remove a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum Risk {
    /// Regenerates by itself, preselected (caches).
    Safe,
    /// Probably unneeded, the user ticks it (stale node_modules, clean worktrees, VM bundles, old installers).
    Review,
    /// Shown, never preselected, never bulk-selected (DB dumps, media, documents).
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum Category {
    Downloads,
    DevArtifacts,
    Worktrees,
    PackageCaches,
    AiTools,
    Xcode,
    Containers,
    AppCaches,
    System,
    LargeFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DetectorKind {
    Paths,
    OlderThan,
    StaleArtifacts,
    VersionedSiblings,
    GitWorktrees,
    Installers,
    RedundantArchives,
    SimulatorRuntimes,
    Docker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ActionKind {
    Trash,
    Delete,
    GitWorktreeRemove,
    Command,
    InfoOnly,
}

/// One entry of the rule catalog (`rules/default.toml`, `data_dir/rules.d/*.toml`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RuleDef {
    /// e.g. "ai.claude-desktop.vm-bundles"
    pub id: String,
    pub title: String,
    pub description: String,
    pub category: Category,
    pub risk: Risk,
    pub detector: DetectorKind,
    /// Globs, "~" expanded, existing paths only: e.g. "~/.npm/_cacache"; `*` wildcards allowed in any
    /// segment ("~/Library/Application Support/ai-profiles/profiles/<*>/gui-data/vm_bundles" without the
    /// angle brackets; a literal star-slash would terminate the generated JSDoc comment).
    #[serde(default)]
    pub roots: Vec<String>,
    /// Detector-specific parameters (TOML table -> JSON), see ARCHITECTURE.md §4. Defaults to `{}`.
    #[serde(default = "empty_object")]
    pub params: serde_json::Value,
    pub action: ActionKind,
    /// Process names that must NOT be running (e.g. "Claude", "Docker").
    /// The TOML catalog spells it `requires_quit`, JSON uses `requiresQuit`.
    #[serde(default, alias = "requires_quit")]
    pub requires_quit: Vec<String>,
    /// The data comes back on its own (caches).
    #[serde(default)]
    pub regenerates: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Finding {
    /// "{rule_id}:{path-or-key}" — stable across scans (history depends on it).
    pub id: String,
    pub rule_id: String,
    pub category: Category,
    pub risk: Risk,
    /// Short human name.
    pub label: String,
    /// Absolute; None for command-only findings (docker prune).
    pub path: Option<String>,
    /// ALLOCATED bytes (st_blocks*512), hardlinks counted once.
    pub bytes: u64,
    /// st_size sum (sparse files: Docker.raw is 68.7 GB logical / 11.5 GB real).
    pub logical_bytes: u64,
    pub files: u64,
    /// Newest mtime inside (or git last commit for projects).
    pub last_used: Option<DateTime<Utc>>,
    /// Deterministic explanation: "Project untouched for 143 days".
    pub reason: String,
    pub action: ActionKind,
    /// argv for ActionKind::Command, program must be allowlisted.
    pub command: Option<Vec<String>>,
    /// Only Risk::Safe with no blockers.
    pub selected_by_default: bool,
    /// "Claude is running", "Uncommitted changes in worktree".
    pub blockers: Vec<String>,
    /// Set key for versioned siblings / worktrees of one repo.
    pub group: Option<String>,
}

impl Finding {
    /// Adds a blocker and keeps the `selected_by_default` invariant (never preselect a blocked finding).
    pub fn add_blocker(&mut self, blocker: impl Into<String>) {
        self.blockers.push(blocker.into());
        self.selected_by_default = false;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VolumeInfo {
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// NSURLVolumeAvailableCapacityForImportantUsageKey (incl. purgeable).
    pub available_important_bytes: Option<u64>,
    /// sysctl vm.swapusage
    pub swap_used_bytes: Option<u64>,
    /// None => no Full Disk Access.
    pub trash_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScanError {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScanReport {
    /// RFC3339 timestamp of start.
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub volume: VolumeInfo,
    pub full_disk_access: bool,
    /// Sorted by bytes desc.
    pub findings: Vec<Finding>,
    pub errors: Vec<ScanError>,
}

/// `None` means "no filter". Deserializing `{}` yields the default (everything).
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct ScanOptions {
    pub rule_ids: Option<Vec<String>>,
    pub categories: Option<Vec<Category>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ScanPhase {
    Volume,
    Rules,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScanProgress {
    pub phase: ScanPhase,
    pub rule_id: Option<String>,
    pub done_rules: u32,
    pub total_rules: u32,
    pub findings: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CleanMode {
    Trash,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanRequest {
    pub finding_ids: Vec<String>,
    pub mode: CleanMode,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanItemResult {
    pub finding_id: String,
    pub ok: bool,
    pub bytes: u64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanResult {
    pub dry_run: bool,
    pub mode: CleanMode,
    pub items: Vec<CleanItemResult>,
    pub freed_bytes: u64,
    pub moved_to_trash_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GrowthItem {
    pub key: String,
    pub label: String,
    pub bytes_then: u64,
    pub bytes_now: u64,
    pub delta: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FreeSpacePoint {
    pub at: DateTime<Utc>,
    pub free_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> ScanReport {
        let started_at = DateTime::parse_from_rfc3339("2026-11-24T10:00:00Z").unwrap().with_timezone(&Utc);
        ScanReport {
            id: started_at.to_rfc3339(),
            started_at,
            duration_ms: 1234,
            volume: VolumeInfo {
                total_bytes: 494_400_000_000,
                free_bytes: 47_900_000_000,
                available_important_bytes: None,
                swap_used_bytes: Some(14_800_000_000),
                trash_bytes: None,
            },
            full_disk_access: false,
            findings: vec![Finding {
                id: "cache.npm:/Users/test/.npm/_cacache".into(),
                rule_id: "cache.npm".into(),
                category: Category::PackageCaches,
                risk: Risk::Safe,
                label: "npm cache".into(),
                path: Some("/Users/test/.npm/_cacache".into()),
                bytes: 10,
                logical_bytes: 12,
                files: 1,
                last_used: Some(started_at),
                reason: "Package cache, regenerates".into(),
                action: ActionKind::GitWorktreeRemove,
                command: None,
                selected_by_default: true,
                blockers: vec![],
                group: None,
            }],
            errors: vec![ScanError { path: "/x".into(), message: "denied".into() }],
        }
    }

    #[test]
    fn scan_report_serializes_camel_case_and_kebab_case() {
        let json = serde_json::to_value(sample_report()).unwrap();
        assert!(json.get("startedAt").is_some());
        assert!(json.get("fullDiskAccess").is_some());
        assert_eq!(json["volume"]["swapUsedBytes"], 14_800_000_000u64);
        let finding = &json["findings"][0];
        assert_eq!(finding["ruleId"], "cache.npm");
        assert_eq!(finding["category"], "package-caches");
        assert_eq!(finding["action"], "git-worktree-remove");
        assert_eq!(finding["selectedByDefault"], true);

        let back: ScanReport = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(back).unwrap(), json);
    }

    #[test]
    fn scan_options_accepts_empty_object() {
        let opts: ScanOptions = serde_json::from_str("{}").unwrap();
        assert!(opts.rule_ids.is_none() && opts.categories.is_none());
        let opts: ScanOptions = serde_json::from_str(r#"{"categories":["ai-tools"]}"#).unwrap();
        assert_eq!(opts.categories, Some(vec![Category::AiTools]));
    }

    #[test]
    fn add_blocker_clears_preselection() {
        let mut finding = sample_report().findings.remove(0);
        finding.add_blocker("Claude is running");
        assert!(!finding.selected_by_default);
        assert_eq!(finding.blockers, vec!["Claude is running".to_string()]);
    }

    /// The UI mock fixture must match the model exactly: every field known, nothing missing.
    #[test]
    fn sample_report_fixture_matches_model() {
        let raw = include_str!("../../../fixtures/sample-report.json");
        let original: serde_json::Value = serde_json::from_str(raw).unwrap();
        let report: ScanReport = serde_json::from_value(original.clone()).expect("fixture deserializes");
        assert!(report.findings.windows(2).all(|w| w[0].bytes >= w[1].bytes), "findings sorted by bytes desc");
        assert_eq!(serde_json::to_value(report).unwrap(), original, "fixture round-trips without loss");
    }
}
