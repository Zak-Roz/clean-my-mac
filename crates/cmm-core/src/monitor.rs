//! Passive monitoring: which alerts a recorded scan raises (low free space, one item growing fast),
//! remembering what was already notified so a daily scan does not repeat itself, and the macOS
//! notification that carries the alerts.
//!
//! The decision logic is pure ([`evaluate`], [`filter_new`], [`compose`]); [`alerts_after_scan`]
//! wires it to the history database and `data_dir/monitor-state.json`. Nothing here cleans
//! anything: alerts only point the user at the app.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::config::Config;
use crate::error::CoreError;
use crate::history::{History, ScanWindow};
use crate::model::{GrowthItem, GrowthKind, ScanReport};

/// What was already notified, in `data_dir`.
pub const STATE_FILE: &str = "monitor-state.json";

/// A low-space alert is repeated after this many days even if free space did not drop further.
pub const LOW_SPACE_REMINDER_DAYS: i64 = 7;

/// Upper bound for posting one notification.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// One reason to notify, before de-duplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alert {
    /// Free space is below `low_space_threshold_bytes`.
    LowSpace { free_bytes: u64, threshold_bytes: u64 },
    /// One finding grew by more than `growth_alert_bytes` within `growth_alert_days`.
    Growth {
        /// Finding id (`{rule_id}:{path-or-key}`).
        key: String,
        label: String,
        /// `~/…` path of the finding, if it has one.
        path: Option<String>,
        bytes_now: u64,
        /// Growth between the two compared scans.
        delta: u64,
        /// Days between the two compared scans (at least 1).
        span_days: u32,
    },
}

/// The thresholds the app shows in Settings (read from `config.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MonitorSettings {
    pub low_space_threshold_bytes: u64,
    pub growth_alert_bytes: u64,
    pub growth_alert_days: u32,
    pub notifications: bool,
    /// `data_dir/config.toml`, where the values above are edited.
    pub config_path: String,
}

impl From<&Config> for MonitorSettings {
    fn from(config: &Config) -> Self {
        Self {
            low_space_threshold_bytes: config.low_space_threshold_bytes,
            growth_alert_bytes: config.growth_alert_bytes,
            growth_alert_days: config.growth_alert_days,
            notifications: config.notifications,
            config_path: config.data_dir.join("config.toml").to_string_lossy().into_owned(),
        }
    }
}

/// `true` when `free_bytes` is below the configured warning level (menu bar warning state).
pub fn is_low_space(free_bytes: u64, config: &Config) -> bool {
    free_bytes < config.low_space_threshold_bytes
}

/// The alerts a scan raises: low free space, and every finding (not rule) whose growth between
/// the compared scans exceeds `growth_alert_bytes`. Growth comes from [`History::growth`] with
/// `growth_alert_days`; when the baseline scan is older than that window (missed days), the delta
/// is scaled down to the window before comparing, so slow growth over a month is not reported as
/// a weekly jump. Growth alerts are ordered by delta, largest first.
pub fn evaluate(
    free_bytes: u64,
    growth: &[GrowthItem],
    window: Option<ScanWindow>,
    config: &Config,
    home: &Path,
) -> Vec<Alert> {
    let mut alerts = Vec::new();
    if is_low_space(free_bytes, config) {
        alerts.push(Alert::LowSpace { free_bytes, threshold_bytes: config.low_space_threshold_bytes });
    }
    let Some((then, now)) = window else { return alerts };
    let span_days = (now - then).num_seconds().max(1) as f64 / 86_400.0;
    let window_days = f64::from(config.growth_alert_days.max(1));
    let scale = if span_days > window_days { window_days / span_days } else { 1.0 };
    let mut grown: Vec<&GrowthItem> = growth
        .iter()
        .filter(|g| g.kind == GrowthKind::Item && g.delta > 0)
        .filter(|g| g.delta as f64 * scale > config.growth_alert_bytes as f64)
        .collect();
    grown.sort_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.key.cmp(&b.key)));
    alerts.extend(grown.into_iter().map(|g| Alert::Growth {
        key: g.key.clone(),
        label: g.label.clone(),
        path: finding_path(&g.key).map(|p| tilde(p, home)),
        bytes_now: g.bytes_now,
        delta: g.delta.unsigned_abs(),
        span_days: span_days.round().max(1.0) as u32,
    }));
    alerts
}

/// When an alert was last notified, and the size it reported then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notified {
    pub at: DateTime<Utc>,
    /// Free bytes (low-space alert) or the item's size (growth alert) at notification time.
    pub bytes: u64,
}

/// Contents of [`STATE_FILE`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MonitorState {
    pub low_space: Option<Notified>,
    /// By finding id.
    pub growth: BTreeMap<String, Notified>,
}

/// Drops alerts that were already notified and updates `state` for the ones that remain:
///
/// - **Low space** is notified when free space first falls below the threshold, again when it
///   dropped by another `growth_alert_bytes` since the last notification, and otherwise at most
///   every [`LOW_SPACE_REMINDER_DAYS`]. Climbing back above the threshold resets it.
/// - **Growth** of an item is notified once per `growth_alert_days` window, unless the item grew by
///   another `growth_alert_bytes` since it was last notified.
pub fn filter_new(
    alerts: Vec<Alert>,
    state: &mut MonitorState,
    free_bytes: u64,
    now: DateTime<Utc>,
    config: &Config,
) -> Vec<Alert> {
    if !is_low_space(free_bytes, config) {
        state.low_space = None;
    }
    let window = TimeDelta::days(i64::from(config.growth_alert_days.max(1)));
    state.growth.retain(|_, n| now - n.at < window);

    let step = config.growth_alert_bytes;
    alerts
        .into_iter()
        .filter(|alert| match alert {
            Alert::LowSpace { free_bytes, .. } => {
                let due = state.low_space.is_none_or(|last| {
                    now - last.at >= TimeDelta::days(LOW_SPACE_REMINDER_DAYS)
                        || free_bytes.saturating_add(step) <= last.bytes
                });
                if due {
                    state.low_space = Some(Notified { at: now, bytes: *free_bytes });
                }
                due
            }
            Alert::Growth { key, bytes_now, .. } => {
                let due = state.growth.get(key).is_none_or(|last| *bytes_now >= last.bytes.saturating_add(step));
                if due {
                    state.growth.insert(key.clone(), Notified { at: now, bytes: *bytes_now });
                }
                due
            }
        })
        .collect()
}

/// Reads the state file; missing or unreadable means "nothing notified yet".
pub fn load_state(path: &Path) -> MonitorState {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

/// Writes the state file atomically (temp file + rename).
pub fn save_state(path: &Path, state: &MonitorState) -> Result<(), CoreError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(state).map_err(|e| CoreError::History(e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// After a full scan was recorded: the alerts worth notifying now (already de-duplicated; the
/// state file is updated). `report` must be the scan just recorded in `history`.
pub fn alerts_after_scan(
    history: &History,
    report: &ScanReport,
    config: &Config,
    home: &Path,
) -> Result<Vec<Alert>, CoreError> {
    alerts_after_scan_at(history, report, config, home, Utc::now())
}

pub(crate) fn alerts_after_scan_at(
    history: &History,
    report: &ScanReport,
    config: &Config,
    home: &Path,
    now: DateTime<Utc>,
) -> Result<Vec<Alert>, CoreError> {
    let days = config.growth_alert_days.max(1);
    let growth = history.growth_at(days, now)?;
    let window = history.growth_window_at(days, now)?;
    let free = report.volume.free_bytes;
    let alerts = evaluate(free, &growth, window, config, home);
    let path = config.data_dir.join(STATE_FILE);
    let mut state = load_state(&path);
    let fresh = filter_new(alerts, &mut state, free, now, config);
    save_state(&path, &state)?;
    Ok(fresh)
}

/// A macOS user notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub subtitle: String,
    pub body: String,
}

/// One notification summarizing `alerts` (low space first, then the biggest growth), or `None`
/// when there is nothing to say.
pub fn compose(alerts: &[Alert], free_bytes: u64) -> Option<Notification> {
    struct Grew<'a> {
        label: &'a str,
        path: Option<&'a str>,
        bytes_now: u64,
        delta: u64,
        span_days: u32,
    }
    let low = alerts.iter().find_map(|a| match a {
        Alert::LowSpace { free_bytes, threshold_bytes } => Some((*free_bytes, *threshold_bytes)),
        Alert::Growth { .. } => None,
    });
    let grown: Vec<Grew> = alerts
        .iter()
        .filter_map(|a| match a {
            Alert::Growth { label, path, bytes_now, delta, span_days, .. } => {
                Some(Grew { label, path: path.as_deref(), bytes_now: *bytes_now, delta: *delta, span_days: *span_days })
            }
            Alert::LowSpace { .. } => None,
        })
        .collect();
    let more = match grown.len() {
        0 | 1 => String::new(),
        2 => String::from(" 1 more item also grew fast."),
        n => format!(" {} more items also grew fast.", n - 1),
    };
    let title = String::from("clean-my-mac");
    let low_subtitle = |free| format!("Low disk space: {} free", format_bytes(free));
    match (low, grown.first()) {
        (None, None) => None,
        (Some((free, threshold)), None) => Some(Notification {
            title,
            subtitle: low_subtitle(free),
            body: format!(
                "Below your {} warning level. Open clean-my-mac to see what can go.",
                format_bytes_short(threshold)
            ),
        }),
        (Some((free, _)), Some(g)) => Some(Notification {
            title,
            subtitle: low_subtitle(free),
            body: format!(
                "{} grew +{} in {}{}.{more}",
                g.label,
                format_bytes(g.delta),
                days_text(g.span_days),
                g.path.map(|p| format!(" ({p})")).unwrap_or_default(),
            ),
        }),
        (None, Some(g)) => Some(Notification {
            title,
            subtitle: format!("{} grew +{} in {}", g.label, format_bytes(g.delta), days_text(g.span_days)),
            body: format!(
                "{} is now {}.{more} {} free on the disk.",
                g.path.unwrap_or(g.label),
                format_bytes(g.bytes_now),
                format_bytes(free_bytes)
            ),
        }),
    }
}

/// `osascript` arguments that show `n`. The texts are passed as `argv`, never interpolated into
/// the script, so a file name cannot inject AppleScript.
pub fn osascript_args(n: &Notification) -> Vec<String> {
    vec![
        "-e".into(),
        "on run argv".into(),
        "-e".into(),
        "display notification (item 3 of argv) with title (item 1 of argv) subtitle (item 2 of argv)".into(),
        "-e".into(),
        "end run".into(),
        n.title.clone(),
        n.subtitle.clone(),
        n.body.clone(),
    ]
}

/// Posts `n` through `/usr/bin/osascript` (works from a LaunchAgent in the user's session; macOS
/// lists these notifications under Script Editor).
pub fn post_notification(n: &Notification) -> Result<(), CoreError> {
    let out = crate::process::run_with_timeout("/usr/bin/osascript", &osascript_args(n), NOTIFY_TIMEOUT)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(CoreError::Command(format!("osascript: {}", String::from_utf8_lossy(&out.stderr).trim())))
    }
}

/// `(value, unit)` in decimal units, like Finder and the app.
fn scaled(bytes: u64) -> (f64, &'static str) {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    (value, UNITS[unit])
}

/// Three significant digits, like the app: "41.6 GB", "912 GB", "1.2 TB".
pub fn format_bytes(bytes: u64) -> String {
    match scaled(bytes) {
        (_, "B") => format!("{bytes} B"),
        (value, unit) if value >= 99.95 => format!("{value:.0} {unit}"),
        (value, unit) => format!("{value:.1} {unit}"),
    }
}

/// Compact form for the menu bar: whole units from 10 up ("42 GB", "8.4 GB", "1.2 TB").
pub fn format_bytes_short(bytes: u64) -> String {
    match scaled(bytes) {
        (_, "B") => format!("{bytes} B"),
        (value, unit) if value >= 9.95 => format!("{value:.0} {unit}"),
        (value, unit) => format!("{value:.1} {unit}"),
    }
}

fn days_text(days: u32) -> String {
    if days == 1 { String::from("1 day") } else { format!("{days} days") }
}

/// The path part of a finding id `{rule_id}:{path}`, if it is an absolute path.
fn finding_path(key: &str) -> Option<&str> {
    key.split_once(':').map(|(_, rest)| rest).filter(|rest| rest.starts_with('/'))
}

/// `/Users/me/x` -> `~/x`.
fn tilde(path: &str, home: &Path) -> String {
    match Path::new(path).strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => String::from("~"),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const GB: u64 = 1_000_000_000;

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, 10, 0, 0).unwrap()
    }

    fn config() -> Config {
        Config { data_dir: std::path::PathBuf::from("/nonexistent"), ..Config::default() }
    }

    fn item(key: &str, then: u64, now: u64) -> GrowthItem {
        GrowthItem {
            kind: GrowthKind::Item,
            key: key.into(),
            label: key.rsplit('/').next().unwrap_or(key).into(),
            bytes_then: then,
            bytes_now: now,
            delta: now as i64 - then as i64,
        }
    }

    fn rule(key: &str, delta: i64) -> GrowthItem {
        GrowthItem { kind: GrowthKind::Rule, key: key.into(), label: key.into(), bytes_then: 0, bytes_now: 0, delta }
    }

    fn keys(alerts: &[Alert]) -> Vec<String> {
        alerts
            .iter()
            .map(|a| match a {
                Alert::LowSpace { .. } => String::from("low"),
                Alert::Growth { key, .. } => key.clone(),
            })
            .collect()
    }

    #[test]
    fn low_space_is_strictly_below_the_threshold() {
        let c = config();
        let home = Path::new("/Users/me");
        assert_eq!(keys(&evaluate(49 * GB, &[], None, &c, home)), ["low"]);
        assert!(evaluate(50 * GB, &[], None, &c, home).is_empty());
        assert!(is_low_space(41 * GB, &c) && !is_low_space(120 * GB, &c));
        let custom = Config { low_space_threshold_bytes: 30 * GB, ..config() };
        assert!(evaluate(41 * GB, &[], None, &custom, home).is_empty());
    }

    #[test]
    fn growth_alerts_only_for_items_above_the_threshold() {
        let c = config();
        let home = Path::new("/Users/me");
        let growth = [
            rule("dev.node-modules", 40 * GB as i64), // rules never alert
            item("dev.nm:/Users/me/Developer/a/node_modules", GB, 7 * GB), // +6 GB
            item("db.dumps:/Users/me/dumps/x", 0, 5 * GB), // exactly 5 GB: not "more than"
            item("cache.npm:/Users/me/.npm", 10 * GB, 2 * GB), // shrank
            item("docker:reclaimable", 0, 12 * GB),   // +12 GB, no path
        ];
        let alerts = evaluate(200 * GB, &growth, Some((at(1), at(8))), &c, home);
        assert_eq!(keys(&alerts), ["docker:reclaimable", "dev.nm:/Users/me/Developer/a/node_modules"]);
        match &alerts[1] {
            Alert::Growth { label, path, bytes_now, delta, span_days, .. } => {
                assert_eq!(label, "node_modules");
                assert_eq!(path.as_deref(), Some("~/Developer/a/node_modules"));
                assert_eq!((*bytes_now, *delta, *span_days), (7 * GB, 6 * GB, 7));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&alerts[0], Alert::Growth { path: None, .. }));
        // Without two scans there is no growth to judge.
        assert!(evaluate(200 * GB, &growth, None, &c, home).is_empty());
    }

    #[test]
    fn growth_over_a_longer_span_is_scaled_to_the_window() {
        let c = config();
        let home = Path::new("/Users/me");
        // +8 GB over 21 days is ~2.7 GB per week: no alert.
        let growth = [item("x:/Users/me/x", 0, 8 * GB)];
        assert!(evaluate(200 * GB, &growth, Some((at(1), at(22))), &c, home).is_empty());
        // +8 GB over 10 days is 5.6 GB per week: alert, reported with the real span.
        let alerts = evaluate(200 * GB, &growth, Some((at(1), at(11))), &c, home);
        assert!(matches!(alerts.as_slice(), [Alert::Growth { span_days: 10, delta, .. }] if *delta == 8 * GB));
        // A shorter span than the window is not scaled up.
        let alerts = evaluate(200 * GB, &[item("y:/Users/me/y", 0, 6 * GB)], Some((at(1), at(2))), &c, home);
        assert!(matches!(alerts.as_slice(), [Alert::Growth { span_days: 1, .. }]));
    }

    #[test]
    fn low_space_is_not_repeated_until_it_drops_further_or_a_week_passes() {
        let c = config();
        let mut state = MonitorState::default();
        let low = |free| vec![Alert::LowSpace { free_bytes: free, threshold_bytes: 50 * GB }];

        assert_eq!(filter_new(low(40 * GB), &mut state, 40 * GB, at(1), &c).len(), 1);
        assert!(filter_new(low(38 * GB), &mut state, 38 * GB, at(2), &c).is_empty(), "dropped < 5 GB");
        assert_eq!(filter_new(low(35 * GB), &mut state, 35 * GB, at(3), &c).len(), 1, "dropped 5 GB");
        assert!(filter_new(low(36 * GB), &mut state, 36 * GB, at(9), &c).is_empty(), "6 days later");
        assert_eq!(filter_new(low(36 * GB), &mut state, 36 * GB, at(10), &c).len(), 1, "weekly reminder");

        // Recovering above the threshold resets: the next drop notifies right away.
        assert!(filter_new(vec![], &mut state, 60 * GB, at(11), &c).is_empty());
        assert_eq!(state.low_space, None);
        assert_eq!(filter_new(low(49 * GB), &mut state, 49 * GB, at(12), &c).len(), 1);
    }

    #[test]
    fn growth_is_notified_once_per_window_unless_it_keeps_growing() {
        let c = config();
        let mut state = MonitorState::default();
        let grew = |now| {
            vec![Alert::Growth {
                key: "k".into(),
                label: "k".into(),
                path: None,
                bytes_now: now,
                delta: 6 * GB,
                span_days: 7,
            }]
        };
        assert_eq!(filter_new(grew(10 * GB), &mut state, 200 * GB, at(1), &c).len(), 1);
        assert!(filter_new(grew(12 * GB), &mut state, 200 * GB, at(2), &c).is_empty(), "+2 GB since");
        assert_eq!(filter_new(grew(15 * GB), &mut state, 200 * GB, at(3), &c).len(), 1, "+5 GB since");
        assert!(filter_new(grew(15 * GB), &mut state, 200 * GB, at(9), &c).is_empty());
        // The window passed: the old entry is forgotten and a new week's growth notifies again.
        assert_eq!(filter_new(grew(15 * GB), &mut state, 200 * GB, at(10), &c).len(), 1);
        assert_eq!(state.growth["k"], Notified { at: at(10), bytes: 15 * GB });
    }

    #[test]
    fn state_file_round_trips_and_tolerates_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(STATE_FILE);
        assert_eq!(load_state(&path), MonitorState::default());
        let mut state = MonitorState { low_space: Some(Notified { at: at(1), bytes: 40 * GB }), ..Default::default() };
        state.growth.insert("a:/x".into(), Notified { at: at(2), bytes: 7 * GB });
        save_state(&path, &state).unwrap();
        assert_eq!(load_state(&path), state);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, "{ nope").unwrap();
        assert_eq!(load_state(&path), MonitorState::default());
    }

    #[test]
    fn compose_names_the_item_and_puts_low_space_first() {
        let growth = |key: &str, label: &str, path: Option<&str>, delta| Alert::Growth {
            key: key.into(),
            label: label.into(),
            path: path.map(Into::into),
            bytes_now: 12 * GB,
            delta,
            span_days: 7,
        };
        let low = Alert::LowSpace { free_bytes: 41_600_000_000, threshold_bytes: 50 * GB };
        let nm = growth("a", "node_modules", Some("~/Developer/a/node_modules"), 6_200_000_000);
        let dump = growth("b", "mongo dump", None, 5_500_000_000);

        assert_eq!(compose(&[], 41 * GB), None);

        let n = compose(std::slice::from_ref(&low), 41 * GB).unwrap();
        assert_eq!(n.title, "clean-my-mac");
        assert_eq!(n.subtitle, "Low disk space: 41.6 GB free");
        assert_eq!(n.body, "Below your 50 GB warning level. Open clean-my-mac to see what can go.");

        let n = compose(&[low, nm.clone(), dump.clone()], 41 * GB).unwrap();
        assert_eq!(n.subtitle, "Low disk space: 41.6 GB free");
        assert_eq!(
            n.body,
            "node_modules grew +6.2 GB in 7 days (~/Developer/a/node_modules). 1 more item also grew fast."
        );

        let n = compose(&[nm], 120 * GB).unwrap();
        assert_eq!(n.subtitle, "node_modules grew +6.2 GB in 7 days");
        assert_eq!(n.body, "~/Developer/a/node_modules is now 12.0 GB. 120 GB free on the disk.");
        let n = compose(&[dump], 120 * GB).unwrap();
        assert_eq!(n.body, "mongo dump is now 12.0 GB. 120 GB free on the disk.");
    }

    #[test]
    fn osascript_gets_texts_as_argv_only() {
        let n = Notification { title: "t".into(), subtitle: "\" & do shell script \"x".into(), body: "b".into() };
        let args = osascript_args(&n);
        let script: Vec<&String> = args.iter().skip(1).step_by(2).take(3).collect();
        assert!(script.iter().all(|line| !line.contains("do shell script")), "{script:?}");
        assert_eq!(&args[6..], ["t", "\" & do shell script \"x", "b"]);
    }

    #[test]
    fn byte_formats() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(41_634_000_000), "41.6 GB");
        assert_eq!(format_bytes(912_400_000_000), "912 GB");
        assert_eq!(format_bytes(99_960_000_000), "100 GB");
        assert_eq!(format_bytes(1_234_000_000_000), "1.2 TB");
        assert_eq!(format_bytes_short(41_634_000_000), "42 GB");
        assert_eq!(format_bytes_short(8_400_000_000), "8.4 GB");
        assert_eq!(format_bytes_short(912_400_000_000), "912 GB");
        assert_eq!(format_bytes_short(1_234_000_000_000), "1.2 TB");
        assert_eq!(format_bytes_short(512), "512 B");
    }

    /// A copy of the UI fixture as scan `id` at `started`, with `free` bytes free and the first
    /// finding resized to `first_bytes`.
    fn report(started: DateTime<Utc>, free: u64, first_bytes: u64) -> ScanReport {
        let mut r: ScanReport = serde_json::from_str(include_str!("../../../fixtures/sample-report.json")).unwrap();
        r.id = started.to_rfc3339();
        r.started_at = started;
        r.volume.free_bytes = free;
        r.findings[0].bytes = first_bytes;
        r
    }

    #[test]
    fn alerts_after_scan_uses_history_and_remembers_what_it_sent() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config { data_dir: dir.path().to_path_buf(), ..Config::default() };
        let history = History::open(&dir.path().join("history.sqlite")).unwrap();
        let home = Path::new("/Users/you");

        let first = report(at(1), 120 * GB, 10 * GB);
        history.record_scan(&first).unwrap();
        // One scan: no growth yet, plenty of space.
        assert!(alerts_after_scan_at(&history, &first, &c, home, at(1)).unwrap().is_empty());

        let second = report(at(8), 40 * GB, 17 * GB);
        history.record_scan(&second).unwrap();
        let alerts = alerts_after_scan_at(&history, &second, &c, home, at(8)).unwrap();
        let grown_id = second.findings[0].id.clone();
        assert_eq!(keys(&alerts), vec![String::from("low"), grown_id.clone()]);
        assert!(dir.path().join(STATE_FILE).exists());

        // The next day's identical scan repeats nothing.
        let third = report(at(9), 40 * GB, 17 * GB);
        history.record_scan(&third).unwrap();
        assert!(alerts_after_scan_at(&history, &third, &c, home, at(9)).unwrap().is_empty());
        let state = load_state(&dir.path().join(STATE_FILE));
        assert!(state.low_space.is_some() && state.growth.contains_key(&grown_id));
    }
}
