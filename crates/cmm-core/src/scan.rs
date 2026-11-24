//! Scan orchestration (owned by S1).

use std::path::PathBuf;
use std::time::Instant;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::config::{Config, home_dir};
use crate::detectors::detector_for;
use crate::model::{Finding, RuleDef, ScanError, ScanOptions, ScanPhase, ScanProgress, ScanReport};
use crate::process::ProcessSnapshot;
use crate::size::Sizer;
use crate::volume::{has_full_disk_access, volume_info};

/// Everything a detector may consult. Detectors must take `home` from here, never from the env,
/// so tests can point it at a tempdir.
pub struct ScanContext<'a> {
    pub home: PathBuf,
    pub now: DateTime<Utc>,
    pub sizer: &'a Sizer,
    pub processes: &'a ProcessSnapshot,
    pub config: &'a Config,
}

/// Runs the enabled rules selected by `opts` against the real home directory.
///
/// Skeleton: rules run sequentially (target: in parallel with rayon). A failing rule becomes a
/// `ScanError` (path = rule id) and never aborts the scan.
pub fn scan(
    rules: &[RuleDef],
    opts: &ScanOptions,
    config: &Config,
    on_progress: &(dyn Fn(ScanProgress) + Sync),
) -> ScanReport {
    let started = Instant::now();
    let started_at = Utc::now();
    let home = home_dir();

    let selected: Vec<&RuleDef> = rules
        .iter()
        .filter(|r| r.enabled)
        .filter(|r| opts.rule_ids.as_ref().is_none_or(|ids| ids.contains(&r.id)))
        .filter(|r| opts.categories.as_ref().is_none_or(|cats| cats.contains(&r.category)))
        .collect();
    let total_rules = selected.len() as u32;
    let progress = |phase, rule_id: Option<&str>, done_rules, findings: usize| {
        on_progress(ScanProgress {
            phase,
            rule_id: rule_id.map(str::to_string),
            done_rules,
            total_rules,
            findings: findings as u32,
        })
    };

    progress(ScanPhase::Volume, None, 0, 0);
    let volume = volume_info(&home);
    let full_disk_access = has_full_disk_access(&home);

    let sizer = Sizer::new();
    let processes = ProcessSnapshot::capture();
    let ctx = ScanContext { home: home.clone(), now: started_at, sizer: &sizer, processes: &processes, config };

    let mut findings: Vec<Finding> = Vec::new();
    let mut errors: Vec<ScanError> = Vec::new();
    for (i, rule) in selected.iter().enumerate() {
        progress(ScanPhase::Rules, Some(&rule.id), i as u32, findings.len());
        match detector_for(rule.detector).detect(rule, &ctx) {
            Ok(mut found) => {
                for finding in &mut found {
                    for name in &rule.requires_quit {
                        if processes.is_running(name) {
                            finding.add_blocker(format!("{name} is running"));
                        }
                    }
                }
                findings.extend(found);
            }
            Err(e) => errors.push(ScanError { path: rule.id.clone(), message: e.to_string() }),
        }
    }

    findings.retain(|f| f.bytes >= config.min_finding_bytes);
    findings.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.id.cmp(&b.id)));
    progress(ScanPhase::Done, None, total_rules, findings.len());

    ScanReport {
        id: started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        started_at,
        duration_ms: started.elapsed().as_millis() as u64,
        volume,
        full_disk_access,
        findings,
        errors,
    }
}
