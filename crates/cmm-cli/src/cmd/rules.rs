//! `cmm rules [--json]` — the rule catalog (built-in + rules.d overrides).

use std::process::ExitCode;

use anyhow::Result;
use cmm_core::Config;
use cmm_core::model::RuleDef;

use super::{kebab, load_rules};

#[derive(Debug, clap::Args)]
pub struct RulesArgs {
    /// Print the rules as JSON.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: RulesArgs) -> Result<ExitCode> {
    let rules = load_rules(&Config::load())?;
    if args.json {
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
