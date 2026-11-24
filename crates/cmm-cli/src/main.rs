//! `cmm` — clean-my-mac command-line interface. See docs/ARCHITECTURE.md §5.

use std::process::ExitCode;

use anyhow::Result;
use clap::{ArgGroup, Parser, Subcommand};

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
    Scan(cmd::scan::ScanArgs),
    /// List the rule catalog.
    Rules(cmd::rules::RulesArgs),
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
    Du(cmd::du::DuArgs),
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
        Command::Scan(args) => cmd::scan::run(args),
        Command::Rules(args) => cmd::rules::run(args),
        Command::Du(args) => cmd::du::run(args),
        Command::Clean { .. } => not_implemented("clean"),
        Command::History { .. } => not_implemented("history"),
    }
}

fn not_implemented(name: &str) -> Result<ExitCode> {
    eprintln!("cmm {name}: not implemented yet");
    Ok(ExitCode::from(2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use cmm_core::model::Category;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_scan_filters() {
        let cli = Cli::try_parse_from([
            "cmm",
            "scan",
            "--json",
            "--category",
            "ai-tools",
            "--rule",
            "cache.npm",
            "--no-record",
        ])
        .unwrap();
        match cli.command {
            Command::Scan(args) => {
                assert!(args.json && args.no_record);
                assert_eq!(args.categories, vec![Category::AiTools]);
                assert_eq!(args.rules, vec!["cache.npm".to_string()]);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(Cli::try_parse_from(["cmm", "scan", "--category", "nope"]).is_err());
    }

    #[test]
    fn parses_du() {
        let cli = Cli::try_parse_from(["cmm", "du", "/tmp", "--depth", "2", "--top", "5"]).unwrap();
        match cli.command {
            Command::Du(args) => assert_eq!((args.path.to_str(), args.depth, args.top), (Some("/tmp"), 2, 5)),
            other => panic!("unexpected {other:?}"),
        }
        let Command::Du(args) = Cli::try_parse_from(["cmm", "du", "."]).unwrap().command else { panic!() };
        assert_eq!((args.depth, args.top), (1, 20));
    }

    #[test]
    fn clean_requires_exactly_one_target() {
        assert!(Cli::try_parse_from(["cmm", "clean"]).is_err());
        assert!(Cli::try_parse_from(["cmm", "clean", "--safe", "--id", "x"]).is_err());
        assert!(Cli::try_parse_from(["cmm", "clean", "--id", "a", "--id", "b", "--delete", "--yes"]).is_ok());
        assert!(Cli::try_parse_from(["cmm", "clean", "--rule", "cache.npm"]).is_ok());
    }
}
