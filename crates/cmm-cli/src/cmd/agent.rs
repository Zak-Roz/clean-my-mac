//! `cmm agent install [--at HH:MM] [--cmm <path>] | uninstall | status [--json]` — the daily
//! background scan (a user LaunchAgent running `cmm scan --quiet --notify`). It only scans and
//! records history; it never cleans. Installed only when the user runs `install`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use cmm_core::agent::{self, AgentSpec, AgentStatus, SystemLaunchd};
use cmm_core::config::home_dir;
use cmm_core::{Config, monitor};

use super::display_path;

#[derive(Debug, clap::Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub action: AgentAction,
}

#[derive(Debug, Subcommand)]
pub enum AgentAction {
    /// Run `cmm scan --quiet --notify` once a day (records history, notifies; never cleans).
    Install {
        /// Time of day, local 24-hour clock.
        #[arg(long, value_name = "HH:MM", default_value = "10:00", value_parser = agent::parse_time)]
        at: (u8, u8),
        /// The cmm binary the job runs (default: this one).
        #[arg(long, value_name = "path")]
        cmm: Option<PathBuf>,
    },
    /// Unload and remove the LaunchAgent (the log and history stay).
    Uninstall,
    /// Whether the LaunchAgent is installed and loaded, what it runs and when.
    Status {
        #[arg(long)]
        json: bool,
    },
}

pub fn run(args: AgentArgs) -> Result<ExitCode> {
    let config = Config::load();
    let home = home_dir();
    match args.action {
        AgentAction::Install { at: (hour, minute), cmm } => install(&config, &home, hour, minute, cmm),
        AgentAction::Uninstall => {
            if agent::uninstall(&home, &SystemLaunchd)? {
                println!("Removed the daily background scan ({}).", agent::LABEL);
            } else {
                println!("The daily background scan was not installed.");
            }
            Ok(ExitCode::SUCCESS)
        }
        AgentAction::Status { json } => {
            let mut status = agent::status(&home, &config, &SystemLaunchd);
            status.resolved_cmm = std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned());
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print_status(&status, &home);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn install(config: &Config, home: &Path, hour: u8, minute: u8, cmm: Option<PathBuf>) -> Result<ExitCode> {
    let cmm = match cmm {
        Some(path) => {
            let path = std::path::absolute(&path).with_context(|| format!("bad path {}", path.display()))?;
            if !agent::is_cmm(&path) {
                bail!("{} is not a cmm binary (`{} --version` must print `cmm …`)", path.display(), path.display());
            }
            path
        }
        None => std::env::current_exe().context("cannot locate the running cmm binary")?,
    };
    if agent::is_dev_build(&cmm) {
        eprintln!(
            "note: {} is a development build; it disappears on `cargo clean`. For a stable path run\n      \
             `cargo install --path crates/cmm-cli` and then `~/.cargo/bin/cmm agent install`.",
            cmm.display()
        );
    }
    let spec = AgentSpec { hour, minute, ..AgentSpec::new(cmm, config) };
    let status = agent::install(&spec, home, config, &SystemLaunchd)?;
    println!("Installed: `{} {}` runs daily at {hour:02}:{minute:02}.", spec.cmm.display(), agent::SCAN_ARGS.join(" "));
    println!("It scans and records history only; it never cleans anything.");
    println!(
        "Notifies when free space is below {} or an item grows more than {} in {} days.",
        monitor::format_bytes_short(config.low_space_threshold_bytes),
        monitor::format_bytes_short(config.growth_alert_bytes),
        config.growth_alert_days
    );
    println!("Log: {}", display_path(&status.log_path, home));
    println!();
    println!("Tips:");
    println!(
        "  • Give {} Full Disk Access (System Settings › Privacy & Security) so it sees what the app sees.",
        spec.cmm.display()
    );
    println!("  • Notifications come from Script Editor; allow them there if none show up.");
    println!("  • Remove with `cmm agent uninstall`.");
    Ok(ExitCode::SUCCESS)
}

fn print_status(s: &AgentStatus, home: &Path) {
    if !s.installed && !s.loaded {
        println!("Daily background scan: not installed.");
        println!("Enable it with `cmm agent install` (daily at 10:00; scans and records history, never cleans).");
        return;
    }
    let plist = display_path(&s.plist_path, home);
    println!("Daily background scan ({})", agent::LABEL);
    println!("  plist      {plist}{}", if s.installed { "" } else { "  (missing)" });
    println!("  launchd    {}", if s.loaded { "loaded" } else { "not loaded — run `cmm agent install` again" });
    if let Some(program) = &s.program {
        let warn = if s.program_ok { "" } else { "  (missing — reinstall)" };
        println!("  runs       {program} {}{warn}", agent::SCAN_ARGS.join(" "));
    }
    if let (Some(h), Some(m)) = (s.hour, s.minute) {
        println!("  schedule   daily at {h:02}:{m:02}");
    }
    match s.last_exit_code {
        Some(code) => println!("  last exit  {code}"),
        None => println!("  last exit  — (not run yet)"),
    }
    println!("  log        {}", display_path(&s.log_path, home));
    if let Some(line) = &s.last_log_line {
        println!("  last line  {line}");
    }
}
