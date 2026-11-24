//! `cmm clean (--id … | --rule … | --safe) [--delete] [--yes]`.
//!
//! Scans only the rules involved, plans and prints the plan, then runs the executor: as a dry run
//! unless `--yes`. The executor is [`actions::execute_fresh`]: it scans those rules once more and
//! acts only on findings that are still exactly what the plan showed (safeguard 1), so nothing is
//! ever done on stale data. With `--yes` the history database must open, because every executed
//! action goes to its audit log (safeguard 5; see ARCHITECTURE.md §10).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use cmm_core::actions::{self, CleanPlan};
use cmm_core::history::{History, action_label};
use cmm_core::model::{
    CleanItemResult, CleanMode, CleanOutcome, CleanRequest, CleanResult, Finding, Risk, ScanOptions, ScanReport,
};
use cmm_core::{Config, config::home_dir};

use super::{display_path, fmt_bytes, kebab, load_rules};

/// Parsed `cmm clean` flags (exactly one of `ids` / `rules` / `safe` is set, enforced by clap).
pub struct Args {
    pub ids: Vec<String>,
    pub rules: Vec<String>,
    pub safe: bool,
    pub delete: bool,
    pub yes: bool,
}

pub fn run(args: Args) -> Result<ExitCode> {
    let config = Config::load();
    let catalog = load_rules(&config)?;
    let home = home_dir();

    let scope: Vec<String> = if !args.ids.is_empty() {
        // Finding ids are "{rule_id}:{key}"; rule ids never contain ':'.
        let set: BTreeSet<&str> =
            args.ids.iter().map(|id| id.split_once(':').map_or(id.as_str(), |(r, _)| r)).collect();
        set.into_iter().map(str::to_string).collect()
    } else if !args.rules.is_empty() {
        if let Some(r) = args.rules.iter().find(|r| !catalog.iter().any(|c| &&c.id == r)) {
            bail!("unknown rule `{r}` (see `cmm rules`)");
        }
        args.rules.clone()
    } else if args.safe {
        catalog.iter().filter(|r| r.enabled && r.risk == Risk::Safe).map(|r| r.id.clone()).collect()
    } else {
        bail!("choose what to clean: --id, --rule or --safe");
    };

    eprintln!("Scanning {} rule(s)…", scope.len());
    let opts = ScanOptions { rule_ids: Some(scope), categories: None };
    let report = cmm_core::scan(&catalog, &opts, &config, &|_| {});
    for e in &report.errors {
        eprintln!("warning: {}: {}", e.path, e.message);
    }

    let (ids, manual_left_out) = select_ids(&report, &args);
    if manual_left_out > 0 {
        println!("Note: {manual_left_out} manual-risk finding(s) are never bulk-selected; pick them with --id.");
    }
    if ids.is_empty() {
        println!("Nothing to clean.");
        return Ok(ExitCode::SUCCESS);
    }

    let mode = if args.delete { CleanMode::Delete } else { CleanMode::Trash };
    let dry_run = !args.yes;
    let plan = actions::plan(&report, &CleanRequest { finding_ids: ids, mode, dry_run })?;
    print_plan(&plan, mode, dry_run, &home);
    if plan.items.is_empty() {
        println!("Nothing to clean.");
        return Ok(ExitCode::SUCCESS);
    }

    let history = if dry_run {
        None // a dry run has no side effects, not even creating the database
    } else {
        let path = config.data_dir.join("history.sqlite");
        Some(History::open(&path).with_context(|| {
            format!("cannot open the history database {} (audit log); refusing to clean", path.display())
        })?)
    };

    let req = CleanRequest { finding_ids: plan.items.iter().map(|f| f.id.clone()).collect(), mode, dry_run };
    let by_id: HashMap<&str, &Finding> = report.findings.iter().map(|f| (f.id.as_str(), f)).collect();
    println!();
    eprintln!("Re-checking {} item(s) against a fresh scan…", req.finding_ids.len());
    let result = actions::execute_fresh(&catalog, &report, &req, &config, &home, history.as_ref(), &|item| {
        print_item(item, &by_id, &home)
    });
    print_summary(&result);
    let failed = result.items.iter().any(|i| i.outcome == CleanOutcome::Failed);
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// The finding ids the flags select, and how many manual-risk findings `--rule` left out.
fn select_ids(report: &ScanReport, args: &Args) -> (Vec<String>, usize) {
    if !args.ids.is_empty() {
        return (args.ids.clone(), 0);
    }
    if !args.rules.is_empty() {
        let of_rules: Vec<&Finding> = report.findings.iter().filter(|f| args.rules.contains(&f.rule_id)).collect();
        let manual = of_rules.iter().filter(|f| f.risk == Risk::Manual).count();
        let ids = of_rules.iter().filter(|f| f.risk != Risk::Manual).map(|f| f.id.clone()).collect();
        return (ids, manual);
    }
    (report.findings.iter().filter(|f| f.selected_by_default).map(|f| f.id.clone()).collect(), 0)
}

fn print_plan(plan: &CleanPlan, mode: CleanMode, dry_run: bool, home: &Path) {
    let how = match mode {
        CleanMode::Trash => "move to Trash",
        CleanMode::Delete => "DELETE PERMANENTLY",
    };
    let when = if dry_run { "dry run — nothing will change; add --yes to apply" } else { "for real" };
    println!("Plan: {how} ({when})");
    if !plan.items.is_empty() {
        println!();
        println!("{:>10}  {:<7}  {:<19}  WHAT", "SIZE", "RISK", "ACTION");
        for f in &plan.items {
            println!(
                "{:>10}  {:<7}  {:<19}  {}",
                fmt_bytes(f.bytes),
                kebab(&f.risk),
                action_label(f.action, mode),
                what(f, home)
            );
        }
        println!("{:>10}  in {} item(s)", fmt_bytes(plan.total_bytes), plan.items.len());
    }
    if !plan.warnings.is_empty() {
        println!();
        for w in &plan.warnings {
            println!("! {w}");
        }
    }
}

fn print_item(item: &CleanItemResult, by_id: &HashMap<&str, &Finding>, home: &Path) {
    let what = by_id.get(item.finding_id.as_str()).map_or_else(|| item.finding_id.clone(), |f| what(f, home));
    let status = match item.outcome {
        CleanOutcome::Done => "ok",
        CleanOutcome::Skipped => "skip",
        CleanOutcome::Failed => "FAIL",
    };
    let size = if item.bytes > 0 { fmt_bytes(item.bytes) } else { String::new() };
    let message = item.message.as_deref().unwrap_or("");
    println!("{status:<4}  {size:>10}  {what}  — {message}");
}

fn print_summary(result: &CleanResult) {
    let count = |o: CleanOutcome| result.items.iter().filter(|i| i.outcome == o).count();
    let (failed, skipped) = (count(CleanOutcome::Failed), count(CleanOutcome::Skipped));
    println!();
    if result.dry_run {
        println!(
            "Dry run: would move {} to Trash and free {}; {failed} item(s) would fail, {skipped} skipped.",
            fmt_bytes(result.moved_to_trash_bytes),
            fmt_bytes(result.freed_bytes),
        );
        println!("Nothing was changed. Re-run with --yes to apply.");
    } else {
        println!(
            "Moved to Trash: {}   Freed: {}   Failed: {failed}   Skipped: {skipped}",
            fmt_bytes(result.moved_to_trash_bytes),
            fmt_bytes(result.freed_bytes),
        );
        if result.moved_to_trash_bytes > 0 {
            println!("Items in the Trash still use disk space until you empty the Trash.");
        }
    }
}

/// `~/…` path, or the label for path-less findings.
fn what(f: &Finding, home: &Path) -> String {
    f.path.as_deref().map_or_else(|| f.label.clone(), |p| display_path(p, home))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, risk: Risk, selected: bool) -> Finding {
        serde_json::from_value(serde_json::json!({
            "id": id, "ruleId": id.split_once(':').unwrap().0, "category": "downloads", "risk": risk,
            "label": id, "path": null, "bytes": 1, "logicalBytes": 1, "files": 1, "lastUsed": null,
            "reason": "", "action": "trash", "command": null, "selectedByDefault": selected,
            "blockers": [], "group": null
        }))
        .unwrap()
    }

    #[test]
    fn rule_selection_never_bulk_selects_manual_findings() {
        let mut report: ScanReport = serde_json::from_value(serde_json::json!({
            "id": "x", "startedAt": "2026-11-24T10:00:00Z", "durationMs": 0,
            "volume": {"totalBytes": 0, "freeBytes": 0, "availableImportantBytes": null, "swapUsedBytes": null, "trashBytes": null},
            "fullDiskAccess": true, "findings": [], "errors": []
        }))
        .unwrap();
        report.findings = vec![
            finding("dl:/a", Risk::Review, false),
            finding("dl:/dump.sql", Risk::Manual, false),
            finding("cache:/c", Risk::Safe, true),
        ];
        let args = |ids: &[&str], rules: &[&str], safe: bool| Args {
            ids: ids.iter().map(|s| s.to_string()).collect(),
            rules: rules.iter().map(|s| s.to_string()).collect(),
            safe,
            delete: false,
            yes: false,
        };
        assert_eq!(select_ids(&report, &args(&[], &["dl"], false)), (vec!["dl:/a".to_string()], 1));
        assert_eq!(select_ids(&report, &args(&[], &[], true)), (vec!["cache:/c".to_string()], 0));
        assert_eq!(select_ids(&report, &args(&["dl:/dump.sql"], &[], false)), (vec!["dl:/dump.sql".to_string()], 0));
    }
}
