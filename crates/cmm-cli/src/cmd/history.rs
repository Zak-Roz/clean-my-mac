//! `cmm history [--days 30] [--json]` — what grew between scans, and the free-space trend (owned by S3).

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use cmm_core::history::History;
use cmm_core::model::{FreeSpacePoint, GrowthItem};
use cmm_core::{Config, config::home_dir};

use super::clean::tilde;
use crate::fmt_bytes;

const TOP_RULES: usize = 10;
const TOP_ITEMS: usize = 15;
const TOP_SHRANK: usize = 5;
const SPARK_WIDTH: usize = 60;

pub fn run(days: u32, json: bool) -> Result<ExitCode> {
    let path = Config::load().data_dir.join("history.sqlite");
    // Reading history never creates the database.
    let (growth, window, free) = if path.exists() {
        let history = History::open(&path)?;
        (history.growth(days)?, history.growth_window(days)?, history.free_space(days)?)
    } else {
        (vec![], None, vec![])
    };

    if json {
        let out = serde_json::json!({
            "days": days,
            "baselineAt": window.map(|w| w.0),
            "latestAt": window.map(|w| w.1),
            "growth": growth,
            "freeSpace": free,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(ExitCode::SUCCESS);
    }
    if !path.exists() || free.is_empty() && growth.is_empty() {
        println!("No scans recorded in the last {days} days. Run `cmm scan` to record one.");
        return Ok(ExitCode::SUCCESS);
    }
    print_free_space(&free, days);
    println!();
    print_growth(&growth, window, &home_dir());
    Ok(ExitCode::SUCCESS)
}

fn local(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn signed(delta: i64) -> String {
    let sign = if delta < 0 { "−" } else { "+" };
    format!("{sign}{}", fmt_bytes(delta.unsigned_abs()))
}

fn print_free_space(points: &[FreeSpacePoint], days: u32) {
    println!("Free space, last {days} days ({} scan(s))", points.len());
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        println!("  no scans in this window");
        return;
    };
    let min = points.iter().map(|p| p.free_bytes).min().unwrap_or(0);
    let change =
        i64::try_from(last.free_bytes).unwrap_or(i64::MAX) - i64::try_from(first.free_bytes).unwrap_or(i64::MAX);
    println!("  {}", sparkline(&points.iter().map(|p| p.free_bytes).collect::<Vec<_>>(), SPARK_WIDTH));
    println!(
        "  {} ({}) → {} ({})   change {}   lowest {}",
        fmt_bytes(first.free_bytes),
        local(first.at),
        fmt_bytes(last.free_bytes),
        local(last.at),
        signed(change),
        fmt_bytes(min),
    );
}

fn print_growth(growth: &[GrowthItem], window: Option<(DateTime<Utc>, DateTime<Utc>)>, home: &Path) {
    let Some((then, now)) = window else {
        println!("Growth: needs at least two recorded scans.");
        return;
    };
    println!("Growth between scans of {} and {}", local(then), local(now));
    let (rules, items): (Vec<&GrowthItem>, Vec<&GrowthItem>) = growth.iter().partition(|g| g.key.starts_with("rule:"));

    let total: i64 = rules.iter().map(|g| g.delta).sum();
    println!("  all findings: {}", signed(total));

    let grew_rules: Vec<_> = rules.iter().filter(|g| g.delta > 0).take(TOP_RULES).collect();
    if !grew_rules.is_empty() {
        println!();
        println!("  By rule");
        for g in grew_rules {
            println!(
                "  {:>10}  {:<40}  {} → {}",
                signed(g.delta),
                g.label,
                fmt_bytes(g.bytes_then),
                fmt_bytes(g.bytes_now)
            );
        }
    }
    let grew: Vec<_> = items.iter().filter(|g| g.delta > 0).take(TOP_ITEMS).collect();
    if !grew.is_empty() {
        println!();
        println!("  Top growing items");
        for g in grew {
            println!("  {:>10}  {}", signed(g.delta), item_name(g, home));
        }
    }
    let shrank: Vec<_> = items.iter().rev().filter(|g| g.delta < 0).take(TOP_SHRANK).collect();
    if !shrank.is_empty() {
        println!();
        println!("  Shrank or removed");
        for g in shrank {
            println!("  {:>10}  {}", signed(g.delta), item_name(g, home));
        }
    }
}

/// Finding ids are "{rule_id}:{path-or-key}": show the path (with ~) and the rule.
fn item_name(g: &GrowthItem, home: &Path) -> String {
    match g.key.split_once(':') {
        Some((rule, rest)) if rest.starts_with('/') => format!("{}  ({rule})", tilde(rest, home)),
        Some((rule, rest)) => format!("{}  ({rule}: {rest})", g.label),
        None => g.label.clone(),
    }
}

/// `▁▂▃▄▅▆▇█`, scaled between the series' min and max, at most `width` characters (evenly sampled).
fn sparkline(values: &[u64], width: usize) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let sampled: Vec<u64> = if values.len() <= width {
        values.to_vec()
    } else {
        (0..width).map(|i| values[i * (values.len() - 1) / (width - 1).max(1)]).collect()
    };
    let (min, max) = (*sampled.iter().min().unwrap_or(&0), *sampled.iter().max().unwrap_or(&0));
    sampled
        .iter()
        .map(|&v| {
            if max == min {
                BARS[3]
            } else {
                let level = (v - min) as f64 / (max - min) as f64 * (BARS.len() - 1) as f64;
                BARS[level.round() as usize]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_scales_and_samples() {
        assert_eq!(sparkline(&[0, 7, 14], 60), "▁▅█");
        assert_eq!(sparkline(&[5, 5], 60), "▄▄");
        assert_eq!(sparkline(&[], 60), "");
        let long: Vec<u64> = (0..1000).collect();
        let s = sparkline(&long, 60);
        assert_eq!(s.chars().count(), 60);
        assert!(s.starts_with('▁') && s.ends_with('█'));
    }

    #[test]
    fn signed_and_names() {
        assert_eq!(signed(1_500_000_000), "+1.5 GB");
        assert_eq!(signed(-2_000), "−2.0 KB");
        let home = Path::new("/Users/me");
        let g = |key: &str| GrowthItem { key: key.into(), label: "lbl".into(), bytes_then: 0, bytes_now: 1, delta: 1 };
        assert_eq!(item_name(&g("dev.nm:/Users/me/p/node_modules"), home), "~/p/node_modules  (dev.nm)");
        assert_eq!(item_name(&g("docker:reclaimable"), home), "lbl  (docker: reclaimable)");
    }
}
