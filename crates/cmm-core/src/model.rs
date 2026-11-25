//! Core data model shared by the CLI, the desktop app (via ts-rs bindings) and history.
//!
//! Described in docs/ARCHITECTURE.md §2. Structs serialize with camelCase fields, enums as
//! kebab-case strings. TypeScript bindings: `cargo test -p cmm-core export_bindings` ->
//! `apps/desktop/src/bindings/`.

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
    /// `path` was a directory when scanned (`symlink_metadata`: a symlink is never a directory).
    /// `false` for command-only findings and for reports saved before this field existed.
    #[serde(default)]
    pub is_dir: bool,
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

/// What happened to one requested item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CleanOutcome {
    /// Acted on (or, in a dry run, would be acted on); also a child covered by a done parent.
    Done,
    /// Not acted on by design: info-only, blocked, unknown id, nothing to do.
    Skipped,
    /// Tried and failed (re-validation, guard, tool error).
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanItemResult {
    pub finding_id: String,
    pub outcome: CleanOutcome,
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

/// Whether a [`GrowthItem`] sums a whole rule or tracks one finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum GrowthKind {
    /// `key` is a rule id; the sizes are the sum of that rule's findings.
    Rule,
    /// `key` is a finding id.
    Item,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GrowthItem {
    pub kind: GrowthKind,
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

/// What a [`StorageNode`] stands for (ARCHITECTURE.md §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum StorageKind {
    /// Several nodes shown together ("macOS", "Other in home", "System data", "Other users").
    Group,
    /// An APFS volume of the startup container, sized with `statfs` (System, VM, Preboot, …).
    Volume,
    Folder,
    File,
    /// The direct entries of `path` too small (or too many) to list one by one.
    Rest,
    /// Used space no walked file accounts for (snapshots, unreadable folders, APFS metadata).
    Unmeasured,
}

/// Why part of a [`StorageNode`] could not be read; its size is then a lower bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum Unreadable {
    /// `EPERM`: protected by macOS privacy controls (Full Disk Access) or System Integrity Protection.
    Protected,
    /// `EACCES` or another error: owned by the system or another user.
    Denied,
}

/// One part of the used space: a folder, file, volume or group (Storage view).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StorageNode {
    /// Unique within a breakdown: the absolute path, or "@…" for nodes without one
    /// ("@macos", "@home-other", "@unmeasured", "PATH/@rest").
    pub id: String,
    pub label: String,
    /// Absolute path (folders, files, volumes' mount points). For a `rest` node: the folder whose
    /// smaller entries it sums. None for groups and `unmeasured`.
    pub path: Option<String>,
    pub kind: StorageKind,
    /// ALLOCATED bytes, hardlinks counted once per breakdown.
    pub bytes: u64,
    pub files: u64,
    /// Direct entries of a folder (for a `rest` node: how many entries it sums).
    pub entries: u64,
    /// Newest mtime of the entry and everything below it.
    pub modified: Option<DateTime<Utc>>,
    /// Something in here could not be read: `bytes` is a lower bound.
    pub unreadable: Option<Unreadable>,
    /// Bytes of this node the last scan's findings also count (de-duplicated, at most `bytes`).
    pub found_bytes: u64,
    /// Short explanation for nodes that are not plain folders.
    pub note: Option<String>,
    /// Kept children, biggest first, `rest` last. A folder with `entries > 0` and no children was
    /// not kept that deep: `storage::children` lists it on demand.
    pub children: Vec<StorageNode>,
}

/// The used space of the startup disk split into parts that add up to `used_bytes`
/// (`total_bytes - free_bytes`, the Overview's used space).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StorageBreakdown {
    pub measured_at: DateTime<Utc>,
    pub duration_ms: u64,
    /// Id of the scan report whose findings set `found_bytes`; None without a scan.
    pub scan_id: Option<String>,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    /// Bytes of the walked folders and files (the Data volume, i.e. everything but `volumes`).
    pub walked_bytes: u64,
    /// Used space of the other volumes in the container (system, VM, Preboot, …) and the
    /// container's own overhead, sized with `statfs`.
    pub volumes_bytes: u64,
    /// The `unmeasured` node: the Data volume's used space minus `walked_bytes` (never negative).
    pub unmeasured_bytes: u64,
    /// `walked_bytes` beyond the Data volume's used space (APFS clones are counted per copy);
    /// 0 when the parts add up exactly.
    pub overcount_bytes: u64,
    pub full_disk_access: bool,
    /// Names of the local Time Machine snapshots (`tmutil listlocalsnapshots /`); their space is
    /// part of `unmeasured_bytes`.
    pub local_snapshots: Vec<String>,
    /// All findings of `scan_id`, de-duplicated like the Overview's "Found by last scan".
    pub found_bytes: u64,
    /// The part of `found_bytes` placed in the nodes (the rest has no path, e.g. Docker data).
    pub found_located_bytes: u64,
    /// Top level, biggest first.
    pub nodes: Vec<StorageNode>,
    /// Unreadable paths met while walking (capped).
    pub errors: Vec<ScanError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum StoragePhase {
    Volumes,
    Walking,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StorageProgress {
    pub phase: StoragePhase,
    /// Bytes walked so far.
    pub walked_bytes: u64,
    /// What the walk should reach: the Data volume's used space (0 if unknown).
    pub expected_bytes: u64,
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
                is_dir: true,
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
        assert_eq!(finding["isDir"], true);

        let back: ScanReport = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(back).unwrap(), json);
    }

    /// `last-report.json` files written before `isDir` existed still load (as "not a directory";
    /// the fresh-match kind check then skips their folder findings until the user rescans).
    #[test]
    fn finding_without_is_dir_defaults_to_false() {
        let mut json = serde_json::to_value(sample_report()).unwrap();
        json["findings"][0].as_object_mut().unwrap().remove("isDir");
        let report: ScanReport = serde_json::from_value(json).unwrap();
        assert!(!report.findings[0].is_dir);
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
