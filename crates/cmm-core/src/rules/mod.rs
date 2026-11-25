//! Rule catalog loading and root expansion. The built-in catalog is `rules/default.toml`.
//!
//! Rule files (the built-in catalog and every `rules.d/*.toml`) are a list of `[[rule]]` tables.
//! Parsing is strict: unknown keys (top-level or inside a rule, except under `params`) and
//! duplicate ids within one file are errors, so typos never silently change behaviour.
//!
//! Overrides (`rules.d`, files applied in name order):
//! - a complete rule whose `id` matches a built-in replaces it entirely; a new `id` adds a rule;
//! - an entry with only `id` and `enabled` toggles an existing rule (`enabled = false` disables).

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::error::CoreError;
use crate::model::RuleDef;

/// The built-in catalog, embedded at compile time.
pub const DEFAULT_RULES_TOML: &str = include_str!("../../../../rules/default.toml");

/// Keys accepted inside `[[rule]]` (snake_case as written in TOML, plus serde's camelCase names).
/// Kept in sync with [`RuleDef`] by the `known_keys_match_model` test.
const RULE_KEYS: &[&str] = &[
    "id",
    "title",
    "description",
    "category",
    "risk",
    "detector",
    "roots",
    "params",
    "action",
    "requires_quit",
    "requiresQuit",
    "regenerates",
    "enabled",
    "browse_only",
    "browseOnly",
];

/// One `[[rule]]` entry of a file: a full rule or an enable/disable toggle of an existing one.
#[derive(Debug)]
enum Entry {
    Full(RuleDef),
    Toggle { id: String, enabled: bool },
}

/// Parses a rule file in the `rules/default.toml` format (full rules only).
pub fn parse_rules(toml_text: &str) -> Result<Vec<RuleDef>, CoreError> {
    parse_entries(toml_text, "rules")?
        .into_iter()
        .map(|entry| match entry {
            Entry::Full(rule) => Ok(rule),
            Entry::Toggle { id, .. } => {
                Err(CoreError::Rule(format!("rule `{id}`: missing fields (only `id` and `enabled` given)")))
            }
        })
        .collect()
}

/// Parses the embedded `rules/default.toml`.
pub fn builtin_rules() -> Result<Vec<RuleDef>, CoreError> {
    parse_rules(DEFAULT_RULES_TOML).map_err(|e| CoreError::Rule(format!("built-in catalog: {e}")))
}

/// Built-in rules plus `rules_d/*.toml` (callers pass `config.data_dir.join("rules.d")`), applied in
/// file-name order. A missing directory means "no overrides"; any invalid file is an error.
pub fn load_rules(rules_d: Option<&Path>) -> Result<Vec<RuleDef>, CoreError> {
    let mut rules = builtin_rules()?;
    let Some(dir) = rules_d else { return Ok(rules) };
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml") && p.is_file())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(rules),
        Err(e) => return Err(CoreError::Rule(format!("{}: {e}", dir.display()))),
    };
    files.sort();
    for file in files {
        let origin = file.display().to_string();
        let text = std::fs::read_to_string(&file).map_err(|e| CoreError::Rule(format!("{origin}: {e}")))?;
        for entry in parse_entries(&text, &origin)? {
            apply(&mut rules, entry).map_err(|e| CoreError::Rule(format!("{origin}: {e}")))?;
        }
    }
    Ok(rules)
}

fn apply(rules: &mut Vec<RuleDef>, entry: Entry) -> Result<(), String> {
    match entry {
        Entry::Full(rule) => match rules.iter_mut().find(|r| r.id == rule.id) {
            Some(existing) => *existing = rule,
            None => rules.push(rule),
        },
        Entry::Toggle { id, enabled } => match rules.iter_mut().find(|r| r.id == id) {
            Some(existing) => existing.enabled = enabled,
            None => return Err(format!("rule `{id}`: no such rule to enable/disable (a new rule needs all fields)")),
        },
    }
    Ok(())
}

fn parse_entries(toml_text: &str, origin: &str) -> Result<Vec<Entry>, CoreError> {
    let err = |msg: String| CoreError::Rule(format!("{origin}: {msg}"));
    let mut table: toml::Table = toml::from_str(toml_text).map_err(|e| err(e.to_string()))?;
    let rule_values = match table.remove("rule") {
        None => Vec::new(),
        Some(toml::Value::Array(items)) => items,
        Some(_) => return Err(err(String::from("`rule` must be an array of tables ([[rule]])"))),
    };
    if let Some(key) = table.keys().next() {
        return Err(err(format!("unknown top-level key `{key}` (expected only [[rule]] tables)")));
    }

    let mut seen = HashSet::new();
    let mut entries = Vec::with_capacity(rule_values.len());
    for (index, value) in rule_values.into_iter().enumerate() {
        let toml::Value::Table(rule_table) = value else {
            return Err(err(format!("rule #{}: not a table", index + 1)));
        };
        let id = match rule_table.get("id") {
            Some(toml::Value::String(id)) => id.clone(),
            _ => return Err(err(format!("rule #{}: missing string `id`", index + 1))),
        };
        if let Some(key) = rule_table.keys().find(|k| !RULE_KEYS.contains(&k.as_str())) {
            return Err(err(format!("rule `{id}`: unknown key `{key}`")));
        }
        if !seen.insert(id.clone()) {
            return Err(err(format!("duplicate rule id `{id}`")));
        }
        let only_toggle = rule_table.keys().all(|k| k == "id" || k == "enabled");
        let entry = match rule_table.get("enabled") {
            Some(toml::Value::Boolean(enabled)) if only_toggle => Entry::Toggle { id, enabled: *enabled },
            _ => Entry::Full(
                toml::Value::Table(rule_table).try_into::<RuleDef>().map_err(|e| err(format!("rule `{id}`: {e}")))?,
            ),
        };
        entries.push(entry);
    }
    Ok(entries)
}

/// Expands `~` and glob patterns in `rule.roots`; returns existing paths only (sorted,
/// deduplicated). Spaces need no quoting, `*`/`?`/`[..]` work in any segment, and special
/// characters inside the home path itself are escaped. Symlinks are returned as-is (not resolved).
pub fn expand_roots(rule: &RuleDef, home: &Path) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    for root in &rule.roots {
        let pattern = expand_tilde(root, home);
        let Ok(paths) = glob::glob(&pattern) else { continue };
        out.extend(paths.filter_map(Result::ok).filter(|p| p.symlink_metadata().is_ok()));
    }
    out.into_iter().collect()
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
    use std::fs;

    use super::*;
    use crate::model::{ActionKind, Category, DetectorKind, Risk};

    const FULL_RULE: &str = r#"
        [[rule]]
        id = "cache.npm"
        title = "My npm"
        description = "overridden"
        category = "package-caches"
        risk = "review"
        detector = "paths"
        roots = ["~/custom-npm"]
        action = "trash"
    "#;

    fn rule_with_roots(roots: &[&str]) -> RuleDef {
        let mut rule = builtin_rules().unwrap().remove(0);
        rule.roots = roots.iter().map(|r| r.to_string()).collect();
        rule
    }

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
            anything_goes = { nested = true }
            "#,
        )
        .unwrap();
        assert_eq!(rules[0].params["days"], 45);
        assert_eq!(rules[0].params["extensions"][1], "zip");
        assert_eq!(rules[0].params["anything_goes"]["nested"], true);
        assert!(!rules[0].enabled);
        assert!(rules[0].requires_quit.is_empty());
    }

    #[test]
    fn unknown_keys_are_errors() {
        let typo = FULL_RULE.replace("action = \"trash\"", "action = \"trash\"\nrequire_quit = [\"X\"]");
        let err = parse_rules(&typo).unwrap_err().to_string();
        assert!(err.contains("unknown key `require_quit`"), "{err}");

        let err = parse_rules(&format!("version = 2\n{FULL_RULE}")).unwrap_err().to_string();
        assert!(err.contains("unknown top-level key `version`"), "{err}");

        let err = parse_rules(&FULL_RULE.replace("\"review\"", "\"risky\"")).unwrap_err().to_string();
        assert!(err.contains("cache.npm"), "{err}");
    }

    #[test]
    fn duplicate_ids_in_one_file_are_errors() {
        let err = parse_rules(&format!("{FULL_RULE}\n{FULL_RULE}")).unwrap_err().to_string();
        assert!(err.contains("duplicate rule id `cache.npm`"), "{err}");
    }

    /// RULE_KEYS must list every RuleDef field (TOML snake_case spelling).
    #[test]
    fn known_keys_match_model() {
        let rule = builtin_rules().unwrap().remove(0);
        let json = serde_json::to_value(&rule).unwrap();
        for key in json.as_object().unwrap().keys() {
            let snake: String = key
                .chars()
                .flat_map(|c| if c.is_ascii_uppercase() { vec!['_', c.to_ascii_lowercase()] } else { vec![c] })
                .collect();
            assert!(RULE_KEYS.contains(&snake.as_str()), "RULE_KEYS lacks `{snake}`");
        }
    }

    #[test]
    fn rules_d_overrides_replaces_disables_and_adds() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("10-npm.toml"), FULL_RULE).unwrap();
        fs::write(
            dir.path().join("20-disable.toml"),
            "[[rule]]\nid = \"ai.claude-desktop.vm-bundles\"\nenabled = false\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("30-new.toml"),
            FULL_RULE.replace("cache.npm", "my.extra").replace("package-caches", "large-files"),
        )
        .unwrap();
        fs::write(dir.path().join("notes.txt"), "not a rule file").unwrap();

        let builtin = builtin_rules().unwrap();
        let rules = load_rules(Some(dir.path())).unwrap();
        assert_eq!(rules.len(), builtin.len() + 1);

        let npm = rules.iter().find(|r| r.id == "cache.npm").unwrap();
        assert_eq!(npm.title, "My npm");
        assert_eq!(npm.risk, Risk::Review);
        assert_eq!(npm.roots, vec!["~/custom-npm".to_string()]);
        assert!(!npm.regenerates, "replacement is whole, not a merge");

        let vm = rules.iter().find(|r| r.id == "ai.claude-desktop.vm-bundles").unwrap();
        assert!(!vm.enabled);
        assert_eq!(vm.requires_quit, vec!["Claude".to_string()], "toggle keeps the rest");

        let extra = rules.iter().find(|r| r.id == "my.extra").unwrap();
        assert_eq!(extra.category, Category::LargeFiles);
    }

    #[test]
    fn rules_d_later_file_wins_and_can_reenable() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.toml"), "[[rule]]\nid = \"cache.npm\"\nenabled = false\n").unwrap();
        fs::write(dir.path().join("b.toml"), "[[rule]]\nid = \"cache.npm\"\nenabled = true\n").unwrap();
        let rules = load_rules(Some(dir.path())).unwrap();
        assert!(rules.iter().find(|r| r.id == "cache.npm").unwrap().enabled);
    }

    #[test]
    fn rules_d_errors_name_the_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bad.toml"), "[[rule]]\nid = \"nope.unknown\"\nenabled = false\n").unwrap();
        let err = load_rules(Some(dir.path())).unwrap_err().to_string();
        assert!(err.contains("bad.toml") && err.contains("nope.unknown"), "{err}");

        fs::write(dir.path().join("bad.toml"), "[[rule]]\nid = \"cache.npm\"\ntitle = \"only a title\"\n").unwrap();
        let err = load_rules(Some(dir.path())).unwrap_err().to_string();
        assert!(err.contains("missing field"), "{err}");
    }

    #[test]
    fn missing_rules_d_means_builtin_only() {
        let dir = tempfile::tempdir().unwrap();
        let rules = load_rules(Some(&dir.path().join("rules.d"))).unwrap();
        assert_eq!(rules.len(), builtin_rules().unwrap().len());
        assert_eq!(load_rules(None).unwrap().len(), rules.len());
    }

    #[test]
    fn expand_roots_handles_tilde_and_globs() {
        let home = tempfile::tempdir().unwrap();
        for p in ["profiles/a/vm_bundles", "profiles/b/vm_bundles", "profiles/c/other"] {
            fs::create_dir_all(home.path().join(p)).unwrap();
        }
        let rule = rule_with_roots(&["~/profiles/*/vm_bundles", "~/missing", "~/profiles/a/vm_bundles"]);
        let roots = expand_roots(&rule, home.path());
        assert_eq!(roots, vec![home.path().join("profiles/a/vm_bundles"), home.path().join("profiles/b/vm_bundles")]);
    }

    #[test]
    fn expand_roots_handles_spaces_and_mid_path_star() {
        let tmp = tempfile::tempdir().unwrap();
        // A home path with a space and glob metacharacters must be taken literally.
        let home = tmp.path().join("my [home] dir");
        let base = "Library/Application Support/ai-profiles/profiles";
        for p in [
            format!("{base}/a1b5-0001/gui-data/vm_bundles"),
            format!("{base}/second profile/gui-data/vm_bundles"),
            format!("{base}/no-bundles/gui-data"),
            "Library/Application Support/Claude/vm_bundles".to_string(),
        ] {
            fs::create_dir_all(home.join(p)).unwrap();
        }
        let rule = rule_with_roots(&[
            "~/Library/Application Support/Claude/vm_bundles",
            "~/Library/Application Support/ai-profiles/profiles/*/gui-data/vm_bundles",
        ]);
        let roots = expand_roots(&rule, &home);
        assert_eq!(
            roots,
            vec![
                home.join("Library/Application Support/Claude/vm_bundles"),
                home.join(format!("{base}/a1b5-0001/gui-data/vm_bundles")),
                home.join(format!("{base}/second profile/gui-data/vm_bundles")),
            ]
        );
    }

    #[test]
    fn expand_roots_returns_symlinks_without_resolving() {
        let home = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(target.path(), home.path().join("link")).unwrap();
        std::os::unix::fs::symlink(home.path().join("gone"), home.path().join("dangling")).unwrap();
        let rule = rule_with_roots(&["~/link", "~/dangling"]);
        assert_eq!(expand_roots(&rule, home.path()), vec![home.path().join("dangling"), home.path().join("link")]);
    }
}
