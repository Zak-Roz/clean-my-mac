//! `installers` detector — `.dmg .pkg .iso .xip .mpkg` older than `days`.
//!
//! Params: `days` (default `config.installers_older_than_days`), `extensions` (default
//! `dmg pkg mpkg iso xip`), `depth` (default 3). Age is the newer of mtime and birth time, so a
//! freshly downloaded installer that kept the server's old mtime is not flagged.

use crate::detectors::support::{days_since, for_each_root, param_strings, param_u64, touched_of, walk_dirs};
use crate::detectors::{Detector, make_finding};
use crate::error::CoreError;
use crate::model::{Finding, RuleDef};
use crate::scan::ScanContext;

pub struct InstallersDetector;

const DEFAULT_EXTENSIONS: &[&str] = &["dmg", "pkg", "mpkg", "iso", "xip"];

impl Detector for InstallersDetector {
    fn detect(&self, rule: &RuleDef, ctx: &ScanContext) -> Result<Vec<Finding>, CoreError> {
        let days = param_u64(rule, "days").unwrap_or(u64::from(ctx.config.installers_older_than_days)) as i64;
        let depth = param_u64(rule, "depth").unwrap_or(3).max(1) as usize;
        let extensions: Vec<String> = param_strings(rule, "extensions")
            .unwrap_or_else(|| DEFAULT_EXTENSIONS.iter().map(|s| s.to_string()).collect())
            .into_iter()
            .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
            .collect();

        let mut findings = Vec::new();
        for_each_root(rule, ctx, |root| {
            walk_dirs(root, depth, &mut |_, entries| {
                for entry in entries.iter().filter(|e| !e.is_hidden() && (e.is_file() || e.is_dir())) {
                    let Some((_, ext)) = entry.name.rsplit_once('.') else { continue };
                    if !extensions.contains(&ext.to_ascii_lowercase()) {
                        continue;
                    }
                    let Some(touched) = touched_of(&entry.meta) else { continue };
                    let age = days_since(ctx.now, touched);
                    if age < days {
                        continue;
                    }
                    let size = ctx.sizer.size_of(&entry.path);
                    let reason = format!("Installer untouched for {age} days");
                    let mut f = make_finding(rule, &entry.path.to_string_lossy(), Some(&entry.path), &size, reason);
                    f.last_used = Some(touched);
                    findings.push(f);
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
    use crate::detectors::support::testutil::{TestEnv, age, file, rule};
    use crate::model::{ActionKind, DetectorKind};

    #[test]
    fn old_installers_only() {
        let home = tempfile::tempdir().unwrap();
        let dl = home.path().join("Downloads");
        file(&dl.join("Figma.dmg"), 5000, 40);
        file(&dl.join("Fresh.dmg"), 5000, 2);
        file(&dl.join("tools/Xcode_27.xip"), 5000, 10);
        file(&dl.join("notes.txt"), 5000, 400);
        file(&dl.join("UPPER.ISO"), 5000, 30);
        // A .pkg bundle (directory) counts as one installer and is not walked into.
        file(&dl.join("Thing.pkg/Contents/Archive.pax"), 5000, 20);
        age(&dl.join("Thing.pkg/Contents"), 20);
        age(&dl.join("Thing.pkg"), 20);

        let env = TestEnv::new(home.path());
        let all = rule(
            "downloads.installers",
            DetectorKind::Installers,
            ActionKind::Trash,
            &["~/Downloads"],
            serde_json::json!({}),
        );
        let mut found = InstallersDetector.detect(&all, &env.ctx()).unwrap();
        found.sort_by(|a, b| a.label.cmp(&b.label));
        let labels: Vec<_> = found.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, vec!["Figma.dmg", "Thing.pkg", "UPPER.ISO", "Xcode_27.xip"]);
        assert!(found[0].reason.starts_with("Installer untouched for 4"), "{}", found[0].reason);
        assert!(found[1].files == 1 && found[1].bytes >= 5000);

        let params = serde_json::json!({"days": 35, "extensions": [".DMG"], "depth": 1});
        let narrow = rule("x", DetectorKind::Installers, ActionKind::Trash, &["~/Downloads"], params);
        let found = InstallersDetector.detect(&narrow, &env.ctx()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "Figma.dmg");
    }
}
