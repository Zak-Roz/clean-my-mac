//! `docker` detector — reclaimable space from `docker system df` (owned by S2).
//!
//! Docker running: one `command` finding `["docker", "system", "prune", "-f"]` (never `-a`, never
//! volumes) sized from what that prune removes: stopped containers, build cache and dangling
//! images. Docker not running: one `info-only` finding sized from the `Docker.raw` disk image
//! (allocated vs logical — it is sparse) with the blocker "Docker is not running". Roots, if given,
//! override the `Docker.raw` location.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::detectors::external;
use crate::detectors::{Detector, make_finding};
use crate::error::CoreError;
use crate::model::{ActionKind, Finding, RuleDef};
use crate::rules::expand_roots;
use crate::scan::ScanContext;
use crate::size::SizeInfo;

pub struct DockerDetector;

const DOCKER_RAW: &str = "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw";

/// One line of `docker system df --format '{{json .}}'`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DfRow {
    #[serde(rename = "Type")]
    kind: String,
    #[serde(default)]
    reclaimable: String,
}

/// Parses Docker's human sizes (`1.2GB`, `512MB`, `13.4kB`, `0B`, `2GiB`) into bytes.
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.split_whitespace().next()?;
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = text.split_at(split);
    let value: f64 = num.parse().ok()?;
    let multiplier: f64 = match unit.to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((value * multiplier).round() as u64)
}

fn human(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 { format!("{gb:.1} GB") } else { format!("{:.0} MB", bytes as f64 / 1e6) }
}

/// The prune finding from `docker system df` rows and `docker images --filter dangling=true` sizes.
fn prune_finding(rule: &RuleDef, df_json_lines: &str, dangling_sizes: &str) -> Result<Option<Finding>, CoreError> {
    let mut containers = 0;
    let mut build_cache = 0;
    let mut images_unused = 0;
    for line in df_json_lines.lines().filter(|l| !l.trim().is_empty()) {
        let row: DfRow = serde_json::from_str(line)
            .map_err(|e| CoreError::Command(format!("unexpected `docker system df` output: {e}")))?;
        let bytes = parse_size(&row.reclaimable).unwrap_or(0);
        match row.kind.as_str() {
            "Containers" => containers = bytes,
            "Build Cache" => build_cache = bytes,
            "Images" => images_unused = bytes,
            _ => {} // Local Volumes: never pruned
        }
    }
    let dangling: u64 = dangling_sizes.lines().filter_map(parse_size).sum();
    let total = containers + build_cache + dangling;
    if total == 0 {
        return Ok(None);
    }
    let mut reason = format!(
        "Reclaimable: stopped containers {}, build cache {}, dangling images {}",
        human(containers),
        human(build_cache),
        human(dangling)
    );
    if images_unused > dangling {
        reason.push_str(&format!("; {} more in unused tagged images is not touched", human(images_unused - dangling)));
    }
    let size = SizeInfo { bytes: total, logical_bytes: total, files: 0, newest_mtime: None };
    let mut f = make_finding(rule, "system-prune", None, &size, reason);
    f.label = "Docker: stopped containers, build cache, dangling images".into();
    f.action = ActionKind::Command;
    f.command = Some(vec!["docker".into(), "system".into(), "prune".into(), "-f".into()]);
    Ok(Some(f))
}

/// Info-only finding for the Docker Desktop disk image when the daemon is not reachable.
fn not_running_finding(rule: &RuleDef, ctx: &ScanContext, raw: &Path) -> Option<Finding> {
    raw.symlink_metadata().ok()?;
    let size = ctx.sizer.size_of(raw);
    let reason = format!(
        "Docker Desktop disk image: {} on disk ({} logical). Start Docker to see what can be pruned",
        human(size.bytes),
        human(size.logical_bytes)
    );
    let mut f = make_finding(rule, &raw.to_string_lossy(), Some(raw), &size, reason);
    f.label = "Docker disk image".into();
    f.action = ActionKind::InfoOnly;
    f.add_blocker("Docker is not running");
    Some(f)
}

fn is_not_running(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("cannot connect to the docker daemon")
        || s.contains("is the docker daemon running")
        || s.contains("error during connect")
        || s.contains("docker desktop is not running")
}

fn raw_path(rule: &RuleDef, ctx: &ScanContext) -> PathBuf {
    if rule.roots.is_empty() {
        ctx.home.join(DOCKER_RAW)
    } else {
        expand_roots(rule, &ctx.home).into_iter().next().unwrap_or_else(|| ctx.home.join(DOCKER_RAW))
    }
}

impl Detector for DockerDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let raw = raw_path(rule, ctx);
        // `docker` is resolved in the shared tool directories (incl. Docker.app's bundled CLI).
        let df = match external::run("docker", &["system", "df", "--format", "{{json .}}"]) {
            Ok(out) => out,
            // No CLI at all: still show the disk image if Docker Desktop left one behind.
            Err(missing) => return not_running_finding(rule, ctx, &raw).map(|f| vec![f]).ok_or(missing),
        };
        if !df.success() {
            if is_not_running(&df.stderr) {
                return Ok(not_running_finding(rule, ctx, &raw).into_iter().collect());
            }
            let first = df.stderr.lines().next().unwrap_or("").trim().to_string();
            return Err(CoreError::Command(format!("`docker system df` failed: {first}")));
        }
        let dangling = external::run("docker", &["images", "--filter", "dangling=true", "--format", "{{.Size}}"])
            .map(|o| o.stdout)
            .unwrap_or_default();
        Ok(prune_finding(rule, &df.stdout, &dangling)?.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::support::testutil::{TestEnv, rule};
    use crate::model::DetectorKind;

    fn docker_rule() -> RuleDef {
        rule("containers.docker", DetectorKind::Docker, ActionKind::Command, &[], serde_json::json!({}))
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1.2GB (50%)"), Some(1_200_000_000));
        assert_eq!(parse_size("13.4kB"), Some(13_400));
        assert_eq!(parse_size("0B"), Some(0));
        assert_eq!(parse_size("2GiB"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("n/a"), None);
    }

    #[test]
    fn prune_counts_what_prune_removes() {
        let df = r#"{"Active":"2","Reclaimable":"3.5GB (70%)","Size":"5GB","TotalCount":"9","Type":"Images"}
{"Active":"1","Reclaimable":"120MB (40%)","Size":"300MB","TotalCount":"3","Type":"Containers"}
{"Active":"1","Reclaimable":"9GB (90%)","Size":"10GB","TotalCount":"4","Type":"Local Volumes"}
{"Active":"0","Reclaimable":"1.1GB","Size":"1.1GB","TotalCount":"40","Type":"Build Cache"}
"#;
        let f = prune_finding(&docker_rule(), df, "500MB\n1GB\n").unwrap().unwrap();
        assert_eq!(f.bytes, 120_000_000 + 1_100_000_000 + 1_500_000_000, "volumes and tagged images excluded");
        assert_eq!(f.command.as_ref().unwrap(), &vec!["docker", "system", "prune", "-f"]);
        assert_eq!(f.action, ActionKind::Command);
        assert!(f.path.is_none());
        assert!(f.reason.contains("2.0 GB more in unused tagged images"), "{}", f.reason);
        assert_eq!(f.id, "containers.docker:system-prune");

        let empty = r#"{"Reclaimable":"0B","Type":"Containers"}"#;
        assert!(prune_finding(&docker_rule(), empty, "").unwrap().is_none());
        assert!(prune_finding(&docker_rule(), "garbage", "").is_err());
    }

    #[test]
    fn not_running_reports_sparse_disk_image() {
        let home = tempfile::tempdir().unwrap();
        let raw = home.path().join(DOCKER_RAW);
        std::fs::create_dir_all(raw.parent().unwrap()).unwrap();
        let f = std::fs::File::create(&raw).unwrap();
        f.set_len(8 * 1024 * 1024 * 1024).unwrap(); // sparse: 8 GiB logical, ~0 allocated
        drop(f);
        let env = TestEnv::new(home.path());
        let finding = not_running_finding(&docker_rule(), &env.ctx(), &raw).unwrap();
        assert_eq!(finding.action, ActionKind::InfoOnly);
        assert_eq!(finding.blockers, vec!["Docker is not running"]);
        assert_eq!(finding.logical_bytes, 8 * 1024 * 1024 * 1024);
        assert!(finding.bytes < finding.logical_bytes);
        assert!(!finding.selected_by_default);
        assert!(is_not_running(
            "Cannot connect to the Docker daemon at unix:///x/docker.sock. Is the docker daemon running?"
        ));
        assert!(not_running_finding(&docker_rule(), &env.ctx(), &home.path().join("nope")).is_none());
    }
}
