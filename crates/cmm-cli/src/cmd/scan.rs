//! `cmm scan [--json | --quiet] [--category <c>]... [--rule <id>]... [--no-record] [--notify]`
//!
//! `--quiet --notify` is what the daily LaunchAgent runs (`cmm agent install`): one summary line
//! for its log, and a macOS notification when free space is low or an item grew fast. It only
//! scans and records; it never cleans.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
use chrono::Local;
use cmm_core::config::home_dir;
use cmm_core::history::History;
use cmm_core::model::{Category, Risk, ScanOptions, ScanReport};
use cmm_core::{Config, monitor};

use super::{display_path, fmt_bytes, fmt_gb, kebab, load_rules, nested_flags, parse_category};

#[derive(Debug, clap::Args)]
pub struct ScanArgs {
    /// Print the full ScanReport as JSON.
    #[arg(long)]
    pub json: bool,
    /// Only rules of this category (repeatable), e.g. `package-caches`.
    #[arg(long = "category", value_name = "c", value_parser = parse_category)]
    pub categories: Vec<Category>,
    /// Only this rule id (repeatable).
    #[arg(long = "rule", value_name = "id")]
    pub rules: Vec<String>,
    /// Do not record this scan in the history database (filtered scans are never recorded).
    #[arg(long)]
    pub no_record: bool,
    /// No progress and no table: print one summary line (for the background scan's log).
    #[arg(long, conflicts_with = "json")]
    pub quiet: bool,
    /// After recording, post a macOS notification if free space is below the configured level or
    /// an item grew fast (`config.toml`: low_space_threshold_bytes, growth_alert_bytes,
    /// growth_alert_days, notifications). Needs a full, recorded scan.
    #[arg(long, conflicts_with_all = ["no_record", "categories", "rules"])]
    pub notify: bool,
}

pub fn run(args: ScanArgs) -> Result<ExitCode> {
    let config = Config::load();
    let rules = load_rules(&config)?;
    if let Some(unknown) = args.rules.iter().find(|id| !rules.iter().any(|r| &r.id == *id)) {
        bail!("unknown rule id `{unknown}` (see `cmm rules`)");
    }
    let opts = ScanOptions {
        rule_ids: (!args.rules.is_empty()).then_some(args.rules),
        categories: (!args.categories.is_empty()).then_some(args.categories),
    };

    let show_progress = !args.json && !args.quiet && std::io::stderr().is_terminal();
    let report = cmm_core::scan(&rules, &opts, &config, &|p| {
        if show_progress {
            eprint!("\r\x1b[2KScanning… {}/{} rules, {} findings", p.done_rules, p.total_rules, p.findings);
        }
    });
    if show_progress {
        eprint!("\r\x1b[2K");
    }
    // History compares whole scans: only unfiltered ones are recorded.
    let history = if !args.no_record && cmm_core::scan::is_full_scan(&opts) { record(&config, &report) } else { None };
    let notified = args.notify.then(|| notify(&config, &report, history.as_ref()));
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if args.quiet {
        println!("{}", summary_line(&report, history.is_some(), notified.as_deref()));
    } else {
        print_report(&report, &home_dir());
        if let Some(text) = notified {
            println!("\nAlerts: {text}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Appends the scan to `data_dir/history.sqlite` and returns the open database; history being
/// unavailable never fails a scan.
fn record(config: &Config, report: &ScanReport) -> Option<History> {
    let result = History::open(&config.data_dir.join("history.sqlite")).and_then(|h| h.record_scan(report).map(|()| h));
    match result {
        Ok(history) => Some(history),
        Err(e) => {
            eprintln!("warning: scan not recorded in history: {e}");
            None
        }
    }
}

/// `--notify`: checks the alerts for the scan just recorded and posts one notification. Returns
/// what happened, for the summary line. Failures are warnings, never a failed scan.
fn notify(config: &Config, report: &ScanReport, history: Option<&History>) -> String {
    let Some(history) = history else {
        eprintln!("warning: alerts not checked: the scan was not recorded in history");
        return String::from("alerts not checked");
    };
    let alerts = match monitor::alerts_after_scan(history, report, config, &home_dir()) {
        Ok(alerts) => alerts,
        Err(e) => {
            eprintln!("warning: alerts not checked: {e}");
            return String::from("alerts not checked");
        }
    };
    let Some(notification) = monitor::compose(&alerts, report.volume.free_bytes) else {
        return String::from("no alerts");
    };
    let what = format!("{} — {}", notification.subtitle, notification.body);
    if !config.notifications {
        return format!("not notified (notifications = false): {what}");
    }
    match monitor::post_notification(&notification) {
        Ok(()) => format!("notified: {what}"),
        Err(e) => {
            eprintln!("warning: notification not shown: {e}");
            format!("notification failed: {what}")
        }
    }
}

/// `--quiet` output: one line per scan, so the LaunchAgent log reads as a timeline.
fn summary_line(report: &ScanReport, recorded: bool, notified: Option<&str>) -> String {
    let nested = nested_flags(&report.findings);
    let found: u64 = report.findings.iter().zip(&nested).filter(|(_, n)| !**n).map(|(f, _)| f.bytes).sum();
    let mut line = format!(
        "{} scan {}: {} free of {}, {} findings ({}), {:.1} s",
        report.started_at.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
        if recorded { "recorded" } else { "not recorded" },
        fmt_bytes(report.volume.free_bytes),
        fmt_bytes(report.volume.total_bytes),
        report.findings.len(),
        fmt_bytes(found),
        report.duration_ms as f64 / 1000.0,
    );
    if !report.errors.is_empty() {
        line.push_str(&format!(", {} warnings", report.errors.len()));
    }
    if !report.full_disk_access {
        line.push_str(", no Full Disk Access");
    }
    if let Some(text) = notified {
        line.push_str("; ");
        line.push_str(text);
    }
    line
}

fn print_report(report: &ScanReport, home: &Path) {
    let v = &report.volume;
    let mut volume = format!("{} free of {}", fmt_bytes(v.free_bytes), fmt_bytes(v.total_bytes));
    if let Some(important) = v.available_important_bytes.filter(|b| *b > v.free_bytes) {
        volume.push_str(&format!(" ({} incl. purgeable)", fmt_bytes(important)));
    }
    let swap = v.swap_used_bytes.map_or_else(|| String::from("?"), fmt_bytes);
    let trash = v.trash_bytes.map_or_else(|| String::from("n/a"), fmt_bytes);
    println!("Volume: {volume}   Swap: {swap}   Trash: {trash}");
    println!(
        "Full Disk Access: {}   Scan: {:.1} s",
        if report.full_disk_access { "yes" } else { "no (some folders are not scanned)" },
        report.duration_ms as f64 / 1000.0
    );

    let nested = nested_flags(&report.findings);
    if report.findings.is_empty() {
        println!("\nNo findings.");
    } else {
        // Categories by their (de-duplicated) size, biggest first; findings keep bytes-desc order.
        let mut by_category: BTreeMap<String, (u64, Vec<usize>)> = BTreeMap::new();
        for (i, f) in report.findings.iter().enumerate() {
            let entry = by_category.entry(category_title(f.category)).or_default();
            entry.1.push(i);
            if !nested[i] {
                entry.0 += f.bytes;
            }
        }
        let mut groups: Vec<_> = by_category.into_iter().collect();
        groups.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));

        for (title, (total, indices)) in groups {
            println!("\n{title} — {}", fmt_gb(total));
            for i in indices {
                let f = &report.findings[i];
                let mark = if !f.blockers.is_empty() {
                    "!"
                } else if f.selected_by_default {
                    "*"
                } else {
                    " "
                };
                let nest = if nested[i] { "↳ " } else { "" };
                let path = f.path.as_deref().map(|p| display_path(p, home)).unwrap_or_default();
                println!("  {:>9}  {:<6}  {mark}  {nest}{}  {path}", fmt_gb(f.bytes), kebab(&f.risk), f.label);
                for blocker in &f.blockers {
                    println!("  {:>9}  {:<6}  !  {blocker}", "", "");
                }
            }
        }
        print_totals(report, &nested);
    }

    if !report.errors.is_empty() {
        const SHOW: usize = 8;
        eprintln!("\n{} warnings (use --json for all):", report.errors.len());
        for e in report.errors.iter().take(SHOW) {
            eprintln!("  {}: {}", display_path(&e.path, home), e.message);
        }
        if report.errors.len() > SHOW {
            eprintln!("  … and {} more", report.errors.len() - SHOW);
        }
    }
}

/// Per-risk totals counting nested paths once (the enclosing finding already includes them).
fn print_totals(report: &ScanReport, nested: &[bool]) {
    let mut totals = [(Risk::Safe, 0u64, 0usize), (Risk::Review, 0, 0), (Risk::Manual, 0, 0)];
    let mut preselected = 0;
    for (f, _) in report.findings.iter().zip(nested).filter(|(_, n)| !**n) {
        if let Some(t) = totals.iter_mut().find(|t| t.0 == f.risk) {
            t.1 += f.bytes;
            t.2 += 1;
        }
        if f.selected_by_default {
            preselected += f.bytes;
        }
    }
    println!("\nTotals (nested paths counted once)   * preselected   ! blocked   ↳ inside another finding");
    for (risk, bytes, count) in totals {
        println!("  {:<6}  {:>9}  in {count} findings", kebab(&risk), fmt_gb(bytes));
    }
    let all: u64 = totals.iter().map(|t| t.1).sum();
    println!("  {:<6}  {:>9}  ({} preselected)", "all", fmt_gb(all), fmt_gb(preselected));
}

fn category_title(category: Category) -> String {
    match category {
        Category::Downloads => "Downloads",
        Category::DevArtifacts => "Developer artifacts",
        Category::Worktrees => "Worktrees",
        Category::PackageCaches => "Package caches",
        Category::AiTools => "AI tools",
        Category::Xcode => "Xcode",
        Category::Containers => "Containers",
        Category::AppCaches => "App caches",
        Category::System => "System",
        Category::LargeFiles => "Large files",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_summary_is_one_line_with_what_happened() {
        let mut report: ScanReport =
            serde_json::from_str(include_str!("../../../../fixtures/sample-report.json")).unwrap();
        report.volume.free_bytes = 41_600_000_000;
        report.full_disk_access = true;
        report.errors.clear();
        let line = summary_line(&report, true, Some("no alerts"));
        assert!(!line.contains('\n'), "{line}");
        assert!(line.contains(" scan recorded: 41.6 GB free of "), "{line}");
        assert!(line.contains(&format!("{} findings (", report.findings.len())), "{line}");
        assert!(line.ends_with("; no alerts"), "{line}");

        report.full_disk_access = false;
        let line = summary_line(&report, false, None);
        assert!(line.contains("scan not recorded") && line.ends_with(", no Full Disk Access"), "{line}");
    }
}
