//! `cmm` — clean-my-mac command-line interface. See docs/ARCHITECTURE.md §5.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgGroup, Parser, Subcommand};
use cmm_core::model::{Category, RuleDef, ScanOptions, ScanReport};
use cmm_core::{Config, rules};
use serde::Deserialize;
use serde::de::IntoDeserializer;

mod cmd;

#[derive(Debug, Parser)]
#[command(name = "cmm", version, about = "Rule-based macOS disk cleaner")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan the disk and list findings (table by default).
    Scan {
        #[arg(long)]
        json: bool,
        /// Only rules of this category (repeatable), e.g. `package-caches`.
        #[arg(long = "category", value_name = "c", value_parser = parse_category)]
        categories: Vec<Category>,
        /// Only this rule id (repeatable).
        #[arg(long = "rule", value_name = "id")]
        rules: Vec<String>,
    },
    /// List the rule catalog.
    Rules {
        #[arg(long)]
        json: bool,
    },
    /// Clean findings (dry-run unless --yes). Re-scans the requested rules first.
    #[command(group(ArgGroup::new("target").required(true).args(["ids", "rules", "safe"])))]
    Clean {
        /// Finding id (repeatable).
        #[arg(long = "id", value_name = "finding-id")]
        ids: Vec<String>,
        /// All findings of this rule (repeatable).
        #[arg(long = "rule", value_name = "id")]
        rules: Vec<String>,
        /// All findings selected by default (risk safe, no blockers).
        #[arg(long)]
        safe: bool,
        /// Delete permanently instead of moving to Trash.
        #[arg(long)]
        delete: bool,
        /// Actually do it (otherwise dry-run).
        #[arg(long)]
        yes: bool,
    },
    /// Growth and free-space trend from the history database.
    History {
        #[arg(long, default_value_t = 30)]
        days: u32,
        #[arg(long)]
        json: bool,
    },
    /// Quick allocated-size breakdown of a directory.
    Du {
        path: PathBuf,
        #[arg(long, default_value_t = 1)]
        depth: u32,
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
}

/// Parses a kebab-case category name using the model's serde representation.
fn parse_category(s: &str) -> Result<Category, String> {
    Category::deserialize(s.into_deserializer()).map_err(|e: serde::de::value::Error| e.to_string())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Rules { json } => cmd_rules(json),
        Command::Scan { json, categories, rules } => cmd_scan(json, categories, rules),
        Command::Clean { ids, rules, safe, delete, yes } => {
            cmd::clean::run(cmd::clean::Args { ids, rules, safe, delete, yes })
        }
        Command::History { days, json } => cmd::history::run(days, json),
        Command::Du { .. } => not_implemented("du"),
    }
}

fn not_implemented(name: &str) -> Result<ExitCode> {
    eprintln!("cmm {name}: not implemented yet");
    Ok(ExitCode::from(2))
}

fn cmd_rules(json: bool) -> Result<ExitCode> {
    let config = Config::load();
    let rules = rules::load_rules(Some(&config.data_dir.join("rules.d")))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rules)?);
    } else {
        print_rules(&rules);
    }
    Ok(ExitCode::SUCCESS)
}

fn print_rules(rules: &[RuleDef]) {
    let id_width = rules.iter().map(|r| r.id.len()).max().unwrap_or(2).max(2);
    println!("{:<id_width$}  {:<15}  {:<7}  {:<19}  TITLE", "ID", "CATEGORY", "RISK", "DETECTOR");
    for rule in rules {
        let disabled = if rule.enabled { "" } else { " (disabled)" };
        println!(
            "{:<id_width$}  {:<15}  {:<7}  {:<19}  {}{disabled}",
            rule.id,
            kebab(&rule.category),
            kebab(&rule.risk),
            kebab(&rule.detector),
            rule.title,
        );
    }
}

fn cmd_scan(json: bool, categories: Vec<Category>, rule_ids: Vec<String>) -> Result<ExitCode> {
    let config = Config::load();
    let rules = rules::load_rules(Some(&config.data_dir.join("rules.d")))?;
    let opts = ScanOptions {
        rule_ids: (!rule_ids.is_empty()).then_some(rule_ids),
        categories: (!categories.is_empty()).then_some(categories),
    };
    let report = cmm_core::scan(&rules, &opts, &config, &|_progress| {});
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    Ok(ExitCode::SUCCESS)
}

fn print_report(report: &ScanReport) {
    let v = &report.volume;
    println!(
        "Volume: {} free of {}   Full Disk Access: {}   ({} ms)",
        fmt_bytes(v.free_bytes),
        fmt_bytes(v.total_bytes),
        if report.full_disk_access { "yes" } else { "no" },
        report.duration_ms,
    );
    println!();
    if report.findings.is_empty() {
        println!("No findings.");
    } else {
        println!("{:>10}  {:<7}  {:<15}  {:<6}  WHAT", "SIZE", "RISK", "CATEGORY", "SELECT");
        for f in &report.findings {
            let what = f.path.as_deref().unwrap_or(&f.label);
            let select = if f.selected_by_default { "yes" } else { "" };
            println!(
                "{:>10}  {:<7}  {:<15}  {:<6}  {what}",
                fmt_bytes(f.bytes),
                kebab(&f.risk),
                kebab(&f.category),
                select
            );
            for blocker in &f.blockers {
                println!("{:>10}  ! {blocker}", "");
            }
        }
        let total: u64 = report.findings.iter().map(|f| f.bytes).sum();
        println!("\n{} findings, {} total", report.findings.len(), fmt_bytes(total));
    }
    for e in &report.errors {
        eprintln!("warning: {}: {}", e.path, e.message);
    }
}

/// The serde (kebab-case) name of a unit enum value.
fn kebab<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::from("?"),
    }
}

/// Decimal units, like Finder ("494.4 GB").
fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_scan_filters() {
        let cli =
            Cli::try_parse_from(["cmm", "scan", "--json", "--category", "ai-tools", "--rule", "cache.npm"]).unwrap();
        match cli.command {
            Command::Scan { json, categories, rules } => {
                assert!(json);
                assert_eq!(categories, vec![Category::AiTools]);
                assert_eq!(rules, vec!["cache.npm".to_string()]);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(Cli::try_parse_from(["cmm", "scan", "--category", "nope"]).is_err());
    }

    #[test]
    fn clean_requires_exactly_one_target() {
        assert!(Cli::try_parse_from(["cmm", "clean"]).is_err());
        assert!(Cli::try_parse_from(["cmm", "clean", "--safe", "--id", "x"]).is_err());
        assert!(Cli::try_parse_from(["cmm", "clean", "--id", "a", "--id", "b", "--delete", "--yes"]).is_ok());
        assert!(Cli::try_parse_from(["cmm", "clean", "--rule", "cache.npm"]).is_ok());
    }

    #[test]
    fn formats_decimal_units() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(494_384_795_648), "494.4 GB");
        assert_eq!(fmt_bytes(47_861_239_808), "47.9 GB");
    }
}
