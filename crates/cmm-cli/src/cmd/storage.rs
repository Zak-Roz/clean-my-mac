//! `cmm storage [--json] [--depth 1] [--top 12] [--no-findings]` — what the used space of the
//! startup disk is made of (ARCHITECTURE.md §11). Read-only: walks the Data volume (tens of
//! seconds) and sizes the system volumes with `statfs`.
//!
//! The "found" column comes from the desktop app's last full scan (`data_dir/last-report.json`),
//! when there is one; `cmm scan` does not keep its report.

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use chrono::{Local, Utc};
use cmm_core::Config;
use cmm_core::config::home_dir;
use cmm_core::model::{ScanReport, StorageBreakdown, StorageKind, StorageNode, Unreadable};
use cmm_core::storage;

use super::{display_path, fmt_bytes};

/// The desktop app's last full scan (ARCHITECTURE.md §8).
const LAST_REPORT_FILE: &str = "last-report.json";

#[derive(Debug, clap::Args)]
pub struct StorageArgs {
    /// Print the full breakdown (every kept node) as JSON.
    #[arg(long)]
    pub json: bool,
    /// How many levels below the top level to print.
    #[arg(long, default_value_t = 1)]
    pub depth: u32,
    /// Show at most this many entries per level (biggest first).
    #[arg(long, default_value_t = 12)]
    pub top: usize,
    /// Do not mark what the app's last scan found.
    #[arg(long)]
    pub no_findings: bool,
}

pub fn run(args: StorageArgs) -> Result<ExitCode> {
    let config = Config::load();
    let home = home_dir();
    let report = if args.no_findings { None } else { load_last_report(&config.data_dir.join(LAST_REPORT_FILE)) };
    let show_progress = !args.json && std::io::stderr().is_terminal();
    let breakdown = storage::breakdown(&home, report.as_ref(), &|p| {
        if show_progress && p.expected_bytes > 0 {
            eprint!("\r\x1b[2KMeasuring… {} of ~{}", fmt_bytes(p.walked_bytes), fmt_bytes(p.expected_bytes));
        }
    });
    if show_progress {
        eprint!("\r\x1b[2K");
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&breakdown)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_breakdown(&breakdown, report.as_ref(), &home, &args);
    Ok(ExitCode::SUCCESS)
}

fn load_last_report(path: &Path) -> Option<ScanReport> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn print_breakdown(b: &StorageBreakdown, report: Option<&ScanReport>, home: &Path, args: &StorageArgs) {
    println!(
        "Startup disk: {} used of {} ({} free)  [measured in {:.1} s]",
        fmt_bytes(b.used_bytes),
        fmt_bytes(b.total_bytes),
        fmt_bytes(b.free_bytes),
        b.duration_ms as f64 / 1000.0
    );
    match report {
        Some(r) => {
            let ago = Utc::now().signed_duration_since(r.started_at);
            println!(
                "Found by the app's last scan ({}, {} ago): {} — {} of it placed below",
                r.started_at.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
                fmt_age(ago.num_minutes()),
                fmt_bytes(b.found_bytes),
                fmt_bytes(b.found_located_bytes)
            );
        }
        None if !args.no_findings => println!("No scan from the app yet: the FOUND column is empty."),
        None => {}
    }
    println!();
    println!("{:>10}  {:>10}  PART", "SIZE", "FOUND");
    print_nodes(&b.nodes, home, args.top, 0, args.depth);

    let parts: u64 = b.nodes.iter().map(|n| n.bytes).sum();
    println!("{:>10}", "─".repeat(10));
    println!(
        "{:>10}  = walked {} + volumes {} + not measured {}{}",
        fmt_bytes(parts),
        fmt_bytes(b.walked_bytes),
        fmt_bytes(b.volumes_bytes),
        fmt_bytes(b.unmeasured_bytes),
        if b.overcount_bytes > 0 {
            format!(
                " (the walk exceeds the Data volume by {}: APFS clones count per copy)",
                fmt_bytes(b.overcount_bytes)
            )
        } else {
            String::new()
        }
    );
    let diff = parts.abs_diff(b.used_bytes);
    println!(
        "{:>10}  used (total − free); the parts differ by {} ({:.3} %)",
        fmt_bytes(b.used_bytes),
        fmt_bytes(diff),
        if b.used_bytes > 0 { diff as f64 * 100.0 / b.used_bytes as f64 } else { 0.0 }
    );
    if !b.local_snapshots.is_empty() {
        println!("\n{} local snapshots (their space is part of \"Not measured\").", b.local_snapshots.len());
    }
    if !b.errors.is_empty() {
        let fda = if b.full_disk_access { "" } else { " — grant Full Disk Access to read the protected ones" };
        eprintln!("\n{} unreadable paths (not counted){fda}:", b.errors.len());
        for e in b.errors.iter().take(5) {
            eprintln!("  {}: {}", display_path(&e.path, home), e.message);
        }
        if b.errors.len() > 5 {
            eprintln!("  … and {} more", b.errors.len() - 5);
        }
    }
}

fn print_nodes(nodes: &[StorageNode], home: &Path, top: usize, indent: usize, depth: u32) {
    for node in nodes.iter().take(top) {
        let found = if node.found_bytes > 0 { fmt_bytes(node.found_bytes) } else { String::from("–") };
        let label = match node.kind {
            StorageKind::Folder | StorageKind::File if indent == 0 => display_path(&node.label, home),
            StorageKind::Folder => format!("{}/", node.label),
            _ => node.label.clone(),
        };
        let flag = match node.unreadable {
            Some(Unreadable::Protected) => "  [partly unreadable: protected by macOS]",
            Some(Unreadable::Denied) => "  [partly unreadable: permission denied]",
            None => "",
        };
        println!("{:>10}  {found:>10}  {:indent$}{label}{flag}", fmt_bytes(node.bytes), "");
        if depth > 0 {
            print_nodes(&node.children, home, top, indent + 2, depth - 1);
        }
    }
    if nodes.len() > top {
        let rest = &nodes[top..];
        let bytes: u64 = rest.iter().map(|n| n.bytes).sum();
        println!("{:>10}  {:>10}  {:indent$}(+{} more)", fmt_bytes(bytes), "", "", rest.len());
    }
}

fn fmt_age(minutes: i64) -> String {
    match minutes {
        m if m < 60 => format!("{m} min"),
        m if m < 48 * 60 => format!("{} h", m / 60),
        m => format!("{} days", m / (24 * 60)),
    }
}
