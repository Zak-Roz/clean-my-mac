//! One module per subcommand (ARCHITECTURE.md §10); shared formatting helpers live here.

use std::path::Path;

use anyhow::Result;
use cmm_core::Config;
use cmm_core::model::{Category, Finding, RuleDef};
use serde::Deserialize;
use serde::de::IntoDeserializer;

pub mod agent;
pub mod clean;
pub mod du;
pub mod history;
pub mod rules;
pub mod scan;
pub mod storage;

/// Built-in catalog plus `data_dir/rules.d` overrides.
pub fn load_rules(config: &Config) -> Result<Vec<RuleDef>> {
    Ok(cmm_core::rules::load_rules(Some(&config.data_dir.join("rules.d")))?)
}

/// Parses a kebab-case category name using the model's serde representation.
pub fn parse_category(s: &str) -> Result<Category, String> {
    Category::deserialize(s.into_deserializer()).map_err(|e: serde::de::value::Error| e.to_string())
}

/// The serde (kebab-case) name of a unit enum value.
pub fn kebab<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::from("?"),
    }
}

/// Decimal units, like Finder ("494.4 GB").
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// Always gigabytes with one decimal ("0.1 GB", "347.2 GB"), for aligned finding tables.
pub fn fmt_gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// `/Users/me/x` -> `~/x` for display.
pub fn display_path(path: &str, home: &Path) -> String {
    match Path::new(path).strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => String::from("~"),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.to_string(),
    }
}

/// `true` for findings whose path lies strictly inside another finding's path (or duplicates an
/// earlier one): totals skip them so nothing is counted twice.
pub fn nested_flags(findings: &[Finding]) -> Vec<bool> {
    use std::collections::HashMap;
    let mut first_with_path: HashMap<&Path, usize> = HashMap::new();
    for (i, f) in findings.iter().enumerate() {
        if let Some(p) = f.path.as_deref() {
            first_with_path.entry(Path::new(p)).or_insert(i);
        }
    }
    findings
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let Some(path) = f.path.as_deref().map(Path::new) else { return false };
            first_with_path.get(path).is_some_and(|first| *first != i)
                || path.ancestors().skip(1).any(|a| first_with_path.contains_key(a))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_decimal_units() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(494_384_795_648), "494.4 GB");
        assert_eq!(fmt_bytes(47_861_239_808), "47.9 GB");
        assert_eq!(fmt_gb(52_428_800), "0.1 GB");
        assert_eq!(fmt_gb(347_200_000_000), "347.2 GB");
    }

    #[test]
    fn display_path_uses_tilde() {
        let home = Path::new("/Users/me");
        assert_eq!(display_path("/Users/me/Library/Caches", home), "~/Library/Caches");
        assert_eq!(display_path("/Users/me", home), "~");
        assert_eq!(display_path("/Users/meme/x", home), "/Users/meme/x");
        assert_eq!(display_path("/opt/x", home), "/opt/x");
    }
}
