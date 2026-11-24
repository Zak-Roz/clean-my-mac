//! `simulator-runtimes` detector — unusable or duplicate simulator runtimes via `xcrun simctl` (owned by S2).
//!
//! `xcrun simctl runtime list -j`: a deletable runtime whose state is not ready/usable, or that
//! duplicates another runtime's platform + version + build, becomes a `command` finding
//! `["xcrun", "simctl", "runtime", "delete", <id>]` sized from `sizeBytes`. Several entries can
//! point at the **same** disk image (`path`): deleting one that shares its image with a runtime we
//! keep frees nothing, so it gets 0 bytes and a blocker; images shared only among deletable entries
//! are counted once. `xcrun simctl list devices unavailable -j` adds one
//! `["xcrun", "simctl", "delete", "unavailable"]` finding sized from the devices' folders.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::detectors::external;
use crate::detectors::{Detector, make_finding};
use crate::error::CoreError;
use crate::model::{ActionKind, Finding, RuleDef};
use crate::scan::ScanContext;
use crate::size::SizeInfo;

pub struct SimulatorRuntimesDetector;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    identifier: String,
    #[serde(default)]
    build: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    size_bytes: u64,
    #[serde(default)]
    runtime_identifier: String,
    #[serde(default)]
    platform_identifier: String,
    #[serde(default = "yes")]
    deletable: bool,
    #[serde(default)]
    path: String,
    last_used_at: Option<DateTime<Utc>>,
    unusable_error_message: Option<String>,
}

fn yes() -> bool {
    true
}

impl Runtime {
    fn is_usable(&self) -> bool {
        matches!(self.state.to_ascii_lowercase().as_str(), "ready" | "usable")
    }
    /// Mounting, Downloading, Deleting, ...: in flight, leave it alone.
    fn is_transient(&self) -> bool {
        self.state.to_ascii_lowercase().ends_with("ing")
    }
    /// "iOS 27.0 (24A100)"
    fn label(&self) -> String {
        let platform = self
            .runtime_identifier
            .rsplit('.')
            .next()
            .and_then(|last| last.split('-').next())
            .filter(|p| !p.is_empty())
            .unwrap_or("Simulator");
        format!("{platform} {} ({})", self.version, self.build)
    }
    fn dup_key(&self) -> (String, String, String) {
        let platform =
            if self.platform_identifier.is_empty() { &self.runtime_identifier } else { &self.platform_identifier };
        (platform.clone(), self.version.clone(), self.build.clone())
    }
}

/// Findings for the runtimes in `simctl runtime list -j` output.
fn runtime_findings(rule: &RuleDef, json: &str) -> Result<Vec<Finding>, CoreError> {
    let parsed: BTreeMap<String, Runtime> = serde_json::from_str(json)
        .map_err(|e| CoreError::Command(format!("unexpected `simctl runtime list -j` output: {e}")))?;
    let runtimes: Vec<&Runtime> = parsed.values().filter(|r| !r.is_transient()).collect();

    // Best runtime per platform+version+build: usable first, then most recently used, then id.
    let mut best: BTreeMap<(String, String, String), &Runtime> = BTreeMap::new();
    for r in &runtimes {
        let slot = best.entry(r.dup_key()).or_insert(r);
        let better = (r.is_usable(), r.last_used_at, std::cmp::Reverse(&r.identifier))
            > (slot.is_usable(), slot.last_used_at, std::cmp::Reverse(&slot.identifier));
        if better {
            *slot = r;
        }
    }

    let mut doomed: Vec<(&Runtime, String)> = Vec::new();
    for r in &runtimes {
        if !r.deletable {
            continue;
        }
        let keeper = best[&r.dup_key()];
        if !r.is_usable() {
            let why = r.unusable_error_message.as_deref().filter(|m| !m.is_empty()).unwrap_or(&r.state);
            doomed.push((r, format!("Unusable simulator runtime: {why}")));
        } else if keeper.identifier != r.identifier {
            doomed.push((r, format!("Duplicate of runtime {} (same version and build)", keeper.identifier)));
        }
    }
    let doomed_ids: HashSet<&str> = doomed.iter().map(|(r, _)| r.identifier.as_str()).collect();

    let mut counted_paths: HashSet<&str> = HashSet::new();
    let mut findings = Vec::new();
    for (r, reason) in doomed {
        let shared_with_kept = runtimes
            .iter()
            .find(|k| !r.path.is_empty() && k.path == r.path && !doomed_ids.contains(k.identifier.as_str()));
        let first_of_image = r.path.is_empty() || counted_paths.insert(r.path.as_str());
        let bytes = if shared_with_kept.is_none() && first_of_image { r.size_bytes } else { 0 };
        let size = SizeInfo { bytes, logical_bytes: bytes, files: 0, newest_mtime: None };
        let mut f = make_finding(rule, &r.identifier, None, &size, reason);
        f.label = r.label();
        f.last_used = r.last_used_at;
        f.action = ActionKind::Command;
        f.command =
            Some(vec!["xcrun".into(), "simctl".into(), "runtime".into(), "delete".into(), r.identifier.clone()]);
        f.group = Some(r.label());
        if let Some(kept) = shared_with_kept {
            f.add_blocker(format!(
                "Shares its disk image with runtime {} ({}): deleting frees no space and could remove that runtime too",
                kept.identifier, kept.state
            ));
        }
        findings.push(f);
    }
    Ok(findings)
}

#[derive(Debug, Deserialize)]
struct DeviceList {
    #[serde(default)]
    devices: BTreeMap<String, Vec<Device>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    #[serde(default)]
    udid: String,
    data_path: Option<String>,
}

/// One finding for `simctl delete unavailable`, sized from the unavailable devices' folders.
fn unavailable_devices_finding(rule: &RuleDef, ctx: &ScanContext, json: &str) -> Option<Finding> {
    let list: DeviceList = serde_json::from_str(json).ok()?;
    let devices: Vec<&Device> = list.devices.values().flatten().collect();
    if devices.is_empty() {
        return None;
    }
    let default_root = ctx.home.join("Library/Developer/CoreSimulator/Devices");
    let mut size = SizeInfo::default();
    for d in &devices {
        // dataPath is ".../Devices/<udid>/data"; the whole device folder goes.
        let dir: PathBuf = match &d.data_path {
            Some(p) => Path::new(p).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(p)),
            None => default_root.join(&d.udid),
        };
        if !dir.starts_with(&ctx.home) {
            continue;
        }
        let s = ctx.sizer.size_of(&dir);
        size.bytes += s.bytes;
        size.logical_bytes += s.logical_bytes;
        size.files += s.files;
        size.newest_mtime = size.newest_mtime.max(s.newest_mtime);
    }
    let reason = format!("{} simulator devices whose runtime is no longer installed", devices.len());
    let mut f = make_finding(rule, "delete-unavailable", None, &size, reason);
    f.label = "Unavailable simulator devices".into();
    f.action = ActionKind::Command;
    f.command = Some(vec!["xcrun".into(), "simctl".into(), "delete".into(), "unavailable".into()]);
    Some(f)
}

impl Detector for SimulatorRuntimesDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let runtimes = external::run_ok("xcrun", &["simctl", "runtime", "list", "-j"])?;
        let mut findings = runtime_findings(rule, &runtimes.stdout)?;
        if let Ok(devices) = external::run_ok("xcrun", &["simctl", "list", "devices", "unavailable", "-j"]) {
            findings.extend(unavailable_devices_finding(rule, ctx, &devices.stdout));
        }
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::support::testutil::{TestEnv, file, rule};
    use crate::model::DetectorKind;

    fn runtime(id: &str, version: &str, build: &str, state: &str, path: &str, last_used: Option<&str>) -> String {
        let last = last_used.map(|t| format!(r#""lastUsedAt": "{t}","#)).unwrap_or_default();
        format!(
            r#""{id}": {{
                "build": "{build}", "deletable": true, "identifier": "{id}", {last}
                "kind": "Patchable Cryptex Disk Image", "path": "{path}",
                "platformIdentifier": "com.apple.platform.iphonesimulator",
                "runtimeIdentifier": "com.apple.CoreSimulator.SimRuntime.iOS-{v}",
                "sizeBytes": 6512893440, "state": "{state}",
                "unusableErrorMessage": "Cryptex Mount Preferred", "version": "{version}"
            }}"#,
            v = version.replace('.', "-")
        )
    }

    fn sim_rule() -> RuleDef {
        rule(
            "xcode.simulator-runtimes",
            DetectorKind::SimulatorRuntimes,
            ActionKind::Command,
            &[],
            serde_json::json!({}),
        )
    }

    #[test]
    fn typical_layout_unusable_entries_share_the_ready_image() {
        // Typical layout: two Unusable iOS 27.0 entries and one Ready entry on one 6.5 GB image.
        const UNUSABLE_1: &str = "00000000-0000-4000-8000-000000000001";
        const UNUSABLE_2: &str = "00000000-0000-4000-8000-000000000002";
        const READY: &str = "00000000-0000-4000-8000-000000000003";
        const NEWER: &str = "00000000-0000-4000-8000-000000000004";
        let json = format!(
            "{{{},{},{},{}}}",
            runtime(UNUSABLE_2, "27.0", "24A100", "Unusable", "/assets/a", Some("2026-11-20T09:00:00Z")),
            runtime(UNUSABLE_1, "27.0", "24A100", "Unusable", "/assets/a", Some("2026-11-20T09:00:00Z")),
            runtime(READY, "27.0", "24A100", "Ready", "/assets/a", Some("2026-11-20T09:00:00Z")),
            runtime(NEWER, "27.2", "24B200", "Ready", "/assets/b", Some("2026-11-21T09:00:00Z")),
        );
        let found = runtime_findings(&sim_rule(), &json).unwrap();
        assert_eq!(found.len(), 2);
        let shares = format!("Shares its disk image with runtime {READY} (Ready)");
        for f in &found {
            assert_eq!(f.bytes, 0, "frees nothing: the image stays for the ready runtime");
            assert_eq!(f.reason, "Unusable simulator runtime: Cryptex Mount Preferred");
            assert_eq!(f.label, "iOS 27.0 (24A100)");
            assert!(f.blockers[0].starts_with(&shares), "{:?}", f.blockers);
            assert!(f.path.is_none() && !f.selected_by_default);
        }
        assert_eq!(found[0].command.as_ref().unwrap(), &vec!["xcrun", "simctl", "runtime", "delete", UNUSABLE_1]);
        assert_eq!(found[0].id, format!("xcode.simulator-runtimes:{UNUSABLE_1}"));
    }

    #[test]
    fn duplicates_and_unusable_with_own_images() {
        let json = format!(
            "{{{},{},{},{}}}",
            // Two ready copies of 26.0 on different images: keep the most recently used.
            runtime("AAA", "26.0", "23A1", "Ready", "/a/1", Some("2026-01-01T00:00:00Z")),
            runtime("BBB", "26.0", "23A1", "Ready", "/a/2", Some("2026-06-01T00:00:00Z")),
            // Unusable on its own image: full size counts once even when two entries share it.
            runtime("CCC", "25.0", "22A1", "Unusable", "/a/3", None),
            runtime("DDD", "25.0", "22A1", "Unusable", "/a/3", None),
        );
        let mut found = runtime_findings(&sim_rule(), &json).unwrap();
        found.sort_by(|a, b| a.id.cmp(&b.id));
        let ids: Vec<_> = found.iter().map(|f| f.id.rsplit(':').next().unwrap()).collect();
        assert_eq!(ids, vec!["AAA", "CCC", "DDD"]);
        assert_eq!(found[0].reason, "Duplicate of runtime BBB (same version and build)");
        assert_eq!(found[0].bytes, 6512893440);
        assert_eq!(found[1].bytes + found[2].bytes, 6512893440, "shared image counted once");
        assert!(found.iter().all(|f| f.blockers.is_empty()));
    }

    #[test]
    fn non_deletable_and_transient_are_skipped() {
        let json = r#"{
          "X": {"identifier": "X", "version": "1", "build": "b", "state": "Unusable", "deletable": false, "sizeBytes": 5},
          "Y": {"identifier": "Y", "version": "2", "build": "b", "state": "Deleting", "sizeBytes": 5}
        }"#;
        assert!(runtime_findings(&sim_rule(), json).unwrap().is_empty());
        assert!(runtime_findings(&sim_rule(), "not json").is_err());
    }

    #[test]
    fn unavailable_devices_are_sized_from_their_folders() {
        let home = tempfile::tempdir().unwrap();
        let devices = home.path().join("Library/Developer/CoreSimulator/Devices");
        file(&devices.join("U1/data/blob"), 100_000, 1);
        file(&devices.join("U1/device.plist"), 100, 1);
        let json = format!(
            r#"{{"devices": {{"com.apple.CoreSimulator.SimRuntime.iOS-17-0": [
                {{"udid": "U1", "dataPath": "{}", "name": "iPhone 15"}},
                {{"udid": "U2", "name": "iPhone 14"}}
              ], "com.apple.CoreSimulator.SimRuntime.iOS-27-0": []}}}}"#,
            devices.join("U1/data").display()
        );
        let env = TestEnv::new(home.path());
        let f = unavailable_devices_finding(&sim_rule(), &env.ctx(), &json).unwrap();
        assert_eq!(f.reason, "2 simulator devices whose runtime is no longer installed");
        assert_eq!(f.command.as_ref().unwrap(), &vec!["xcrun", "simctl", "delete", "unavailable"]);
        assert!(f.bytes >= 100_000 && f.files == 2);
        assert!(unavailable_devices_finding(&sim_rule(), &env.ctx(), r#"{"devices": {"x": []}}"#).is_none());
    }
}
