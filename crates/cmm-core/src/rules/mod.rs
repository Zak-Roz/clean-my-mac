//! Rule catalog loading and root expansion (owned by S1; catalog content in `rules/default.toml` by S2).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::CoreError;
use crate::model::RuleDef;

/// The built-in catalog, embedded at compile time.
pub const DEFAULT_RULES_TOML: &str = include_str!("../../../../rules/default.toml");

/// TOML file shape: a list of `[[rule]]` tables.
#[derive(Debug, Deserialize)]
struct RuleFile {
    #[serde(default)]
    rule: Vec<RuleDef>,
}

/// Parses a rule file in the `rules/default.toml` format.
pub fn parse_rules(toml_text: &str) -> Result<Vec<RuleDef>, CoreError> {
    let file: RuleFile = toml::from_str(toml_text).map_err(|e| CoreError::Rule(e.to_string()))?;
    Ok(file.rule)
}

/// Parses the embedded `rules/default.toml`.
pub fn builtin_rules() -> Result<Vec<RuleDef>, CoreError> {
    parse_rules(DEFAULT_RULES_TOML)
}

/// Built-in rules plus `user_dir/*.toml` (normally `config.data_dir.join("rules.d")`), user rules
/// overriding built-ins by id.
///
/// Skeleton: built-in rules only; `user_dir` is ignored for now.
pub fn load_rules(user_dir: Option<&Path>) -> Result<Vec<RuleDef>, CoreError> {
    let _ = user_dir;
    builtin_rules()
}

/// Expands `~` and glob patterns in `rule.roots`; returns existing paths only (sorted, deduplicated).
pub fn expand_roots(rule: &RuleDef, home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in &rule.roots {
        let pattern = expand_tilde(root, home);
        let Ok(paths) = glob::glob(&pattern) else { continue };
        out.extend(paths.filter_map(Result::ok).filter(|p| p.symlink_metadata().is_ok()));
    }
    out.sort();
    out.dedup();
    out
}

/// `~` / `~/x` -> `<home>` / `<home>/x` (home escaped for glob); other strings unchanged.
fn expand_tilde(root: &str, home: &Path) -> String {
    let home = glob::Pattern::escape(&home.to_string_lossy());
    if root == "~" {
        home
    } else if let Some(rest) = root.strip_prefix("~/") {
        format!("{}/{rest}", home.trim_end_matches('/'))
    } else {
        root.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ActionKind, DetectorKind, Risk};

    #[test]
    fn builtin_rules_parse_with_defaults() {
        let rules = builtin_rules().unwrap();
        assert!(!rules.is_empty());
        let vm = rules.iter().find(|r| r.id == "ai.claude-desktop.vm-bundles").unwrap();
        assert_eq!(vm.risk, Risk::Review);
        assert_eq!(vm.detector, DetectorKind::Paths);
        assert_eq!(vm.action, ActionKind::Trash);
        assert_eq!(vm.requires_quit, vec!["Claude".to_string()]);
        assert!(vm.regenerates);
        assert!(vm.enabled);
        assert!(vm.params.is_object());
        let mut ids: Vec<_> = rules.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), rules.len(), "rule ids are unique");
    }

    #[test]
    fn params_table_becomes_json() {
        let rules = parse_rules(
            r#"
            [[rule]]
            id = "t"
            title = "T"
            description = "d"
            category = "downloads"
            risk = "manual"
            detector = "older-than"
            roots = ["~/Downloads"]
            action = "trash"
            enabled = false
            [rule.params]
            days = 45
            extensions = ["dmg", "zip"]
            "#,
        )
        .unwrap();
        assert_eq!(rules[0].params["days"], 45);
        assert_eq!(rules[0].params["extensions"][1], "zip");
        assert!(!rules[0].enabled);
        assert!(rules[0].requires_quit.is_empty());
    }

    #[test]
    fn expand_roots_handles_tilde_and_globs() {
        let home = tempfile::tempdir().unwrap();
        for p in ["profiles/a/vm_bundles", "profiles/b/vm_bundles", "profiles/c/other"] {
            std::fs::create_dir_all(home.path().join(p)).unwrap();
        }
        let mut rule = builtin_rules().unwrap().remove(0);
        rule.roots = vec!["~/profiles/*/vm_bundles".into(), "~/missing".into()];
        let roots = expand_roots(&rule, home.path());
        assert_eq!(roots, vec![home.path().join("profiles/a/vm_bundles"), home.path().join("profiles/b/vm_bundles")]);
    }
}
