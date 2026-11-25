//! `paths` detector — one finding per existing match of the rule's root globs.
//!
//! A match that is itself a symlink is skipped: trashing the link would free nothing.

use rayon::prelude::*;

use crate::detectors::{Detector, age_days, describe_age, make_finding};
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::rules::expand_roots;
use crate::scan::ScanContext;

pub struct PathsDetector;

impl Detector for PathsDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let roots: Vec<_> = expand_roots(rule, &ctx.home)
            .into_iter()
            .filter(|p| p.symlink_metadata().is_ok_and(|m| !m.file_type().is_symlink()))
            .collect();
        Ok(roots
            .par_iter()
            .map(|path| {
                let size = ctx.sizer.size_of(path);
                let mut reason = if rule.regenerates {
                    format!("{}; recreated automatically when needed", rule.title)
                } else {
                    rule.title.clone()
                };
                if let Some(mtime) = size.newest_mtime {
                    reason.push_str(&format!("; last modified {}", describe_age(age_days(ctx.now, mtime))));
                }
                let mut finding = make_finding(rule, &path.to_string_lossy(), Some(path), &size, reason);
                finding.label = rule.title.clone();
                finding
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::config::Config;
    use crate::model::{ActionKind, Category, DetectorKind, Risk};
    use crate::process::ProcessSnapshot;
    use crate::size::Sizer;

    fn rule(roots: &[&str]) -> RuleDef {
        RuleDef {
            id: "ai.vm".into(),
            title: "VM bundle".into(),
            description: String::new(),
            category: Category::AiTools,
            risk: Risk::Review,
            detector: DetectorKind::Paths,
            roots: roots.iter().map(|r| r.to_string()).collect(),
            params: serde_json::json!({}),
            action: ActionKind::Trash,
            requires_quit: vec![],
            regenerates: true,
            enabled: true,
            browse_only: false,
        }
    }

    #[test]
    fn one_finding_per_match_with_sizes() {
        let home = tempfile::tempdir().unwrap();
        for profile in ["p1", "p 2"] {
            let dir = home.path().join(format!("Library/Application Support/profiles/{profile}/vm_bundles"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("disk.img"), vec![1u8; 100_000]).unwrap();
        }
        let target = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("Library/Application Support/profiles/p3")).unwrap();
        std::os::unix::fs::symlink(
            target.path(),
            home.path().join("Library/Application Support/profiles/p3/vm_bundles"),
        )
        .unwrap();

        let (sizer, processes, config) = (Sizer::new(), ProcessSnapshot::default(), Config::default());
        let ctx = ScanContext {
            home: home.path().into(),
            now: chrono::Utc::now(),
            sizer: &sizer,
            processes: &processes,
            config: &config,
        };
        let findings =
            PathsDetector.detect(&rule(&["~/Library/Application Support/profiles/*/vm_bundles"]), &ctx).unwrap();
        assert_eq!(findings.len(), 2, "symlinked match skipped");
        let f = &findings[0];
        let path = home.path().join("Library/Application Support/profiles/p 2/vm_bundles");
        assert_eq!(f.path.as_deref(), Some(path.to_str().unwrap()));
        assert_eq!(f.id, format!("ai.vm:{}", path.display()));
        assert_eq!(f.label, "VM bundle");
        assert_eq!(f.files, 1);
        assert!(f.bytes >= 100_000);
        assert!(f.last_used.is_some());
        assert_eq!(f.reason, "VM bundle; recreated automatically when needed; last modified today");
        assert!(!f.selected_by_default, "review risk");
    }
}
