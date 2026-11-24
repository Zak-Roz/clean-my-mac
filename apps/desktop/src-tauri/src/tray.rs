//! Menu bar item (Tauri 2 tray): free space as the title next to a small gauge whose needle shows
//! how full the disk is; the gauge turns orange below `low_space_threshold_bytes`. The menu has
//! Open, Scan Now, the last scan time, the items that grew most in the last `growth_alert_days`
//! days, and Quit.
//!
//! Free space is one `statfs` (`volume::capacity`) a minute — no Trash walk, no scan. History
//! (last scan, growth) is re-read every ten minutes and right after a scan or a clean, so a scan
//! recorded by the background LaunchAgent appears without a restart. Menu texts are updated in
//! place; the menu is only rebuilt when the growth list changes.

use std::f64::consts::PI;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Local, Utc};
use cmm_core::Config;
use cmm_core::config::home_dir;
use cmm_core::history::History;
use cmm_core::model::{GrowthItem, GrowthKind};
use cmm_core::monitor::{format_bytes, format_bytes_short, is_low_space};
use cmm_core::volume;
use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuItem, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::AppState;

pub const TRAY_ID: &str = "main";
/// Payload: the screen to show ("history").
pub const NAVIGATE_EVENT: &str = "app://navigate";
/// A scan started from the menu bar (the window did not start it).
pub const SCAN_STARTED_EVENT: &str = "scan://started";
/// Payload: the `ScanReport` of a scan started from the menu bar.
pub const SCAN_FINISHED_EVENT: &str = "scan://finished";
/// Payload: the error message of a scan started from the menu bar.
pub const SCAN_FAILED_EVENT: &str = "scan://failed";

const FREE_SPACE_EVERY: Duration = Duration::from_secs(60);
const HISTORY_EVERY: Duration = Duration::from_secs(600);
/// Pixel size of the icon (18 pt menu bar at 2x).
const ICON_PX: u32 = 36;
const GROWTH_LINES: usize = 3;
const MAX_LINE_CHARS: usize = 64;
/// macOS system orange.
const WARNING_RGB: [u8; 3] = [255, 149, 0];

/// What the menu shows from the history database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistorySummary {
    pub last_scan: Option<DateTime<Utc>>,
    /// At least two scans exist, so growth can be computed.
    pub has_growth_window: bool,
    /// `+6.2 GB  node_modules — Developer/app`, biggest first, at most three.
    pub growth_lines: Vec<String>,
}

/// Everything the menu bar item displays. Pure data, built by [`model`].
#[derive(Debug, Clone, PartialEq)]
pub struct TrayModel {
    pub title: String,
    pub tooltip: String,
    pub warning: bool,
    /// Used share of the disk, 0..=1 (the gauge needle).
    pub used_fraction: f64,
    pub header: String,
    pub scan_label: String,
    pub scanning: bool,
    pub last_scan: String,
    pub growth_header: String,
    pub growth_lines: Vec<String>,
    /// Shown (disabled) instead of growth lines when there are none.
    pub growth_placeholder: Option<String>,
}

/// Builds the menu bar contents from the cheap free-space reading and the history summary.
pub fn model(
    capacity: Option<(u64, u64)>,
    history: &HistorySummary,
    scanning: bool,
    config: &Config,
    now: DateTime<Local>,
) -> TrayModel {
    let threshold = format_bytes_short(config.low_space_threshold_bytes);
    let (title, header, warning, used_fraction) = match capacity {
        Some((total, free)) if total > 0 => {
            let warning = is_low_space(free, config);
            let mut header = format!("{} free of {}", format_bytes(free), format_bytes(total));
            if warning {
                header.push_str(&format!(" — below {threshold}"));
            }
            (format_bytes_short(free), header, warning, 1.0 - free as f64 / total as f64)
        }
        _ => (String::from("—"), String::from("Free space unavailable"), false, 0.0),
    };
    let tooltip = if warning {
        format!("clean-my-mac — {title} free, below your {threshold} warning level")
    } else {
        format!("clean-my-mac — {title} free")
    };
    let days = config.growth_alert_days.max(1);
    let growth_placeholder = if !history.has_growth_window {
        Some(String::from("Needs two scans to show growth"))
    } else if history.growth_lines.is_empty() {
        Some(String::from("Nothing grew"))
    } else {
        None
    };
    TrayModel {
        title,
        tooltip,
        warning,
        used_fraction: used_fraction.clamp(0.0, 1.0),
        header,
        scan_label: String::from(if scanning { "Scanning…" } else { "Scan Now" }),
        scanning,
        last_scan: match history.last_scan {
            Some(at) => format!("Last scan: {}", when(at.with_timezone(&Local), now)),
            None => String::from("No scan recorded yet"),
        },
        growth_header: format!("Grew in the last {days} {}", if days == 1 { "day" } else { "days" }),
        growth_lines: history.growth_lines.clone(),
        growth_placeholder,
    }
}

/// "today 10:02", "yesterday 10:02", "Nov 20, 10:02", "Nov 20, 2025".
fn when(at: DateTime<Local>, now: DateTime<Local>) -> String {
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        0 => format!("today {}", at.format("%H:%M")),
        1 => format!("yesterday {}", at.format("%H:%M")),
        _ if at.year() == now.year() => at.format("%b %-d, %H:%M").to_string(),
        _ => at.format("%b %-d, %Y").to_string(),
    }
}

/// One menu line per growing finding: `+6.2 GB  node_modules — Developer/app`.
pub fn growth_lines(growth: &[GrowthItem]) -> Vec<String> {
    let mut items: Vec<&GrowthItem> = growth.iter().filter(|g| g.kind == GrowthKind::Item && g.delta > 0).collect();
    items.sort_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.key.cmp(&b.key)));
    items
        .into_iter()
        .take(GROWTH_LINES)
        .map(|g| {
            let place = g
                .key
                .split_once(':')
                .map(|(_, rest)| rest)
                .filter(|rest| rest.starts_with('/'))
                .and_then(parent_tail)
                .map(|p| format!(" — {p}"))
                .unwrap_or_default();
            truncate(&format!("+{}  {}{place}", format_bytes(g.delta.unsigned_abs()), g.label), MAX_LINE_CHARS)
        })
        .collect()
}

/// The last two components of the parent folder: `/Users/me/Developer/app/node_modules` -> `Developer/app`.
fn parent_tail(path: &str) -> Option<String> {
    let parent = std::path::Path::new(path).parent()?;
    let parts: Vec<String> =
        parent.components().rev().take(2).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let tail: Vec<&str> = parts.iter().rev().map(String::as_str).filter(|p| *p != "/").collect();
    (!tail.is_empty()).then(|| tail.join("/"))
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max - 1).collect::<String>())
    }
}

/// Last scan and growth from the history database.
pub fn summarize(history: &History, config: &Config) -> Result<HistorySummary, cmm_core::CoreError> {
    let days = config.growth_alert_days.max(1);
    // Every scan of the last ten years: a few thousand rows at most, read every ten minutes.
    let last_scan = history.free_space(3650)?.last().map(|p| p.at);
    let has_growth_window = history.growth_window(days)?.is_some();
    Ok(HistorySummary { last_scan, has_growth_window, growth_lines: growth_lines(&history.growth(days)?) })
}

/// RGBA pixels (`size`×`size`) of the gauge: a half ring, solid up to the needle and faint after
/// it, with the needle at `fraction` (0 = left, 1 = right). Drawn with distance-based
/// anti-aliasing so no image files or decoders are needed.
pub fn gauge_icon(size: u32, fraction: f64, rgb: [u8; 3]) -> Vec<u8> {
    let scale = f64::from(size) / 18.0; // pixels per point
    let (cx, cy, radius, ring) = (9.0, 12.4, 7.0, 1.8);
    let (hub, needle_len, needle_w) = (1.7, 5.2, 1.6);
    let theta = PI * fraction.clamp(0.0, 1.0);
    let tip = (cx - needle_len * theta.cos(), cy - needle_len * theta.sin());
    let cover = |distance_pt: f64| (distance_pt * scale + 0.5).clamp(0.0, 1.0);

    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (x, y) = ((f64::from(px) + 0.5) / scale, (f64::from(py) + 0.5) / scale);
            // Angle over the top, 0 at the left end, PI at the right end (end caps below the
            // center belong to their end).
            let phi = if y <= cy {
                (cy - y).atan2(cx - x)
            } else if x < cx {
                0.0
            } else {
                PI
            };
            let ring_distance = if y <= cy {
                ((x - cx).hypot(y - cy) - radius).abs()
            } else {
                (x - (cx - radius)).hypot(y - cy).min((x - (cx + radius)).hypot(y - cy))
            };
            let used = if phi <= theta + 1e-9 { 1.0 } else { 0.35 };
            let ring_alpha = cover(ring / 2.0 - ring_distance) * used;
            let needle_alpha = cover(needle_w / 2.0 - segment_distance((x, y), (cx, cy), tip));
            let hub_alpha = cover(hub - (x - cx).hypot(y - cy));
            let alpha = ring_alpha.max(needle_alpha).max(hub_alpha);
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (alpha * 255.0).round() as u8]);
        }
    }
    rgba
}

fn segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// Template (black, follows the menu bar's light/dark look) normally; orange when space is low.
fn icon_for(model: &TrayModel) -> (Image<'static>, bool) {
    let rgb = if model.warning { WARNING_RGB } else { [0, 0, 0] };
    (Image::new_owned(gauge_icon(ICON_PX, model.used_fraction, rgb), ICON_PX, ICON_PX), !model.warning)
}

/// The menu items whose text changes in place.
struct Handles {
    header: MenuItem<tauri::Wry>,
    scan: MenuItem<tauri::Wry>,
    last_scan: MenuItem<tauri::Wry>,
}

#[derive(Default)]
struct Shown {
    model: Option<TrayModel>,
    icon_key: Option<(bool, u32)>,
    handles: Option<Handles>,
}

/// Tray bookkeeping, managed as Tauri state.
#[derive(Default)]
pub struct TrayState {
    history: Mutex<(HistorySummary, Option<Instant>)>,
    shown: Mutex<Shown>,
}

/// Creates the menu bar item and starts the minute timer. Called once from `setup`.
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    app.manage(TrayState::default());
    let config = app.state::<AppState>().config.clone();
    let initial = model(volume::capacity(&home_dir()), &HistorySummary::default(), false, &config, Local::now());
    let (icon, template) = icon_for(&initial);
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(template)
        .title(&initial.title)
        .tooltip(&initial.tooltip)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .build(app)?;
    let handle = app.clone();
    std::thread::Builder::new().name("tray-refresh".into()).spawn(move || {
        loop {
            refresh(&handle, false);
            std::thread::sleep(FREE_SPACE_EVERY);
        }
    })?;
    Ok(())
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "open" => show_main(app),
        "scan" => {
            let app = app.clone();
            std::thread::spawn(move || scan_from_tray(&app));
        }
        "history" => {
            show_main(app);
            let _ = app.emit(NAVIGATE_EVENT, "history");
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

/// Shows, un-minimizes and focuses the main window.
pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn scan_from_tray(app: &AppHandle) {
    if app.state::<AppState>().is_scanning() {
        return;
    }
    let _ = app.emit(SCAN_STARTED_EVENT, ());
    match crate::commands::run_full_scan(app) {
        Ok(report) => {
            let _ = app.emit(SCAN_FINISHED_EVENT, report);
        }
        Err(e) => {
            eprintln!("cmm: menu bar scan failed: {e}");
            let _ = app.emit(SCAN_FAILED_EVENT, e);
        }
    }
}

/// Re-reads free space (and history when `reload_history` or ten minutes passed) and updates the
/// menu bar item. A no-op before the tray exists. Blocking (SQLite): never call on the main thread.
pub fn refresh(app: &AppHandle, reload_history: bool) {
    let (Some(tray), Some(state)) = (app.tray_by_id(TRAY_ID), app.try_state::<TrayState>()) else { return };
    let app_state = app.state::<AppState>();
    let config = &app_state.config;

    let summary = {
        let Ok(mut cached) = state.history.lock() else { return };
        let stale = cached.1.is_none_or(|at| at.elapsed() >= HISTORY_EVERY);
        if reload_history || stale {
            match app_state.with_history(|h| summarize(h, config)) {
                Ok(summary) => *cached = (summary, Some(Instant::now())),
                Err(e) => {
                    eprintln!("cmm: menu bar history unavailable: {e}");
                    cached.1 = Some(Instant::now());
                }
            }
        }
        cached.0.clone()
    };
    let next = model(volume::capacity(&home_dir()), &summary, app_state.is_scanning(), config, Local::now());

    let Ok(mut shown) = state.shown.lock() else { return };
    if shown.model.as_ref() == Some(&next) {
        return;
    }
    let _ = tray.set_title(Some(&next.title));
    let _ = tray.set_tooltip(Some(&next.tooltip));
    let icon_key = (next.warning, (next.used_fraction * 36.0).round() as u32);
    if shown.icon_key != Some(icon_key) {
        let (icon, template) = icon_for(&next);
        let _ = tray.set_icon_with_as_template(Some(icon), template);
        shown.icon_key = Some(icon_key);
    }
    let same_structure = shown
        .model
        .as_ref()
        .is_some_and(|m| m.growth_lines == next.growth_lines && m.growth_placeholder == next.growth_placeholder);
    match shown.handles.as_ref().filter(|_| same_structure) {
        Some(h) => {
            let _ = h.header.set_text(&next.header);
            let _ = h.scan.set_text(&next.scan_label);
            let _ = h.scan.set_enabled(!next.scanning);
            let _ = h.last_scan.set_text(&next.last_scan);
        }
        None => match build_menu(app, &next) {
            Ok((menu, handles)) => {
                let _ = tray.set_menu(Some(menu));
                shown.handles = Some(handles);
            }
            Err(e) => eprintln!("cmm: menu bar menu not built: {e}"),
        },
    }
    shown.model = Some(next);
}

fn build_menu(app: &AppHandle, m: &TrayModel) -> tauri::Result<(Menu<tauri::Wry>, Handles)> {
    let disabled = |id: &str, text: &str| MenuItemBuilder::with_id(id, text).enabled(false).build(app);
    let header = disabled("header", &m.header)?;
    let open = MenuItemBuilder::with_id("open", "Open clean-my-mac").build(app)?;
    let scan = MenuItemBuilder::with_id("scan", &m.scan_label).enabled(!m.scanning).build(app)?;
    let last_scan = disabled("last-scan", &m.last_scan)?;
    let growth_header = disabled("growth-header", &m.growth_header)?;

    let mut builder = MenuBuilder::new(app)
        .item(&header)
        .separator()
        .item(&open)
        .item(&scan)
        .separator()
        .item(&last_scan)
        .item(&growth_header);
    for line in &m.growth_lines {
        // Clicking a growth line opens History.
        builder = builder.item(&MenuItemBuilder::with_id("history", format!("    {line}")).build(app)?);
    }
    if let Some(text) = &m.growth_placeholder {
        builder = builder.item(&disabled("growth-none", &format!("    {text}"))?);
    }
    let quit = MenuItemBuilder::with_id("quit", "Quit clean-my-mac").accelerator("CmdOrCtrl+Q").build(app)?;
    let menu = builder.separator().item(&quit).build()?;
    Ok((menu, Handles { header, scan, last_scan }))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    const GB: u64 = 1_000_000_000;

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn item(key: &str, label: &str, delta: i64) -> GrowthItem {
        GrowthItem { kind: GrowthKind::Item, key: key.into(), label: label.into(), bytes_then: 0, bytes_now: 0, delta }
    }

    #[test]
    fn model_shows_free_space_and_warns_below_the_threshold() {
        let config = Config::default();
        let now = local(2026, 11, 24, 12, 0);
        let summary = HistorySummary {
            last_scan: Some(local(2026, 11, 24, 10, 2).with_timezone(&Utc)),
            has_growth_window: true,
            growth_lines: vec!["+6.2 GB  node_modules — Developer/app".into()],
        };
        let m = model(Some((494_400_000_000, 41_600_000_000)), &summary, false, &config, now);
        assert_eq!(m.title, "42 GB");
        assert!(m.warning);
        assert_eq!(m.header, "41.6 GB free of 494 GB — below 50 GB");
        assert_eq!(m.tooltip, "clean-my-mac — 42 GB free, below your 50 GB warning level");
        assert_eq!(m.last_scan, "Last scan: today 10:02");
        assert_eq!(m.scan_label, "Scan Now");
        assert_eq!(m.growth_header, "Grew in the last 7 days");
        assert_eq!(m.growth_placeholder, None);
        assert!((m.used_fraction - (1.0 - 41.6 / 494.4)).abs() < 1e-9);

        let m = model(Some((494_400_000_000, 120 * GB)), &HistorySummary::default(), true, &config, now);
        assert!(!m.warning);
        assert_eq!(m.title, "120 GB");
        assert_eq!(m.header, "120 GB free of 494 GB");
        assert_eq!(m.scan_label, "Scanning…");
        assert_eq!(m.last_scan, "No scan recorded yet");
        assert_eq!(m.growth_placeholder.as_deref(), Some("Needs two scans to show growth"));

        let custom = Config { low_space_threshold_bytes: 150 * GB, growth_alert_days: 1, ..Config::default() };
        let quiet = HistorySummary { has_growth_window: true, ..Default::default() };
        let m = model(Some((494_400_000_000, 120 * GB)), &quiet, false, &custom, now);
        assert!(m.warning, "the threshold comes from the config");
        assert_eq!(m.growth_header, "Grew in the last 1 day");
        assert_eq!(m.growth_placeholder.as_deref(), Some("Nothing grew"));

        let m = model(None, &quiet, false, &config, now);
        assert_eq!((m.title.as_str(), m.warning), ("—", false));
    }

    #[test]
    fn last_scan_time_is_relative_to_today() {
        let now = local(2026, 11, 24, 12, 0);
        assert_eq!(when(local(2026, 11, 24, 9, 5), now), "today 09:05");
        assert_eq!(when(local(2026, 11, 23, 22, 0), now), "yesterday 22:00");
        assert_eq!(when(local(2026, 11, 3, 10, 0), now), "Nov 3, 10:00");
        assert_eq!(when(local(2025, 12, 30, 10, 0), now), "Dec 30, 2025");
    }

    #[test]
    fn growth_lines_take_the_top_three_items() {
        let growth = [
            GrowthItem { kind: GrowthKind::Rule, ..item("dev.nm", "dev.nm", 90 * GB as i64) },
            item("dev.nm:/Users/me/Developer/app/node_modules", "node_modules", 6_200_000_000),
            item("docker:reclaimable", "Docker reclaimable data", 9 * GB as i64),
            item("x:/Users/me/x", "x", -3 * GB as i64),
            item("db:/Users/me/dumps/mongo_28_01_2025", "mongo_28_01_2025", 2 * GB as i64),
            item("y:/y", "y", GB as i64),
        ];
        assert_eq!(
            growth_lines(&growth),
            [
                "+9.0 GB  Docker reclaimable data",
                "+6.2 GB  node_modules — Developer/app",
                "+2.0 GB  mongo_28_01_2025 — me/dumps",
            ]
        );
        let long = item(&format!("r:/Users/me/{}/x", "a".repeat(80)), &"n".repeat(80), GB as i64);
        let line = &growth_lines(&[long])[0];
        assert_eq!(line.chars().count(), MAX_LINE_CHARS);
        assert!(line.ends_with('…'));
    }

    #[test]
    fn gauge_icon_is_drawn_in_the_given_color_with_a_transparent_background() {
        let px = gauge_icon(ICON_PX, 0.9, WARNING_RGB);
        assert_eq!(px.len(), (ICON_PX * ICON_PX * 4) as usize);
        let alpha = |x: u32, y: u32| px[((y * ICON_PX + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "corners are transparent");
        assert_eq!(alpha(ICON_PX - 1, ICON_PX - 1), 0);
        assert!(px.chunks(4).all(|p| p[..3] == WARNING_RGB), "one color, shape in alpha");
        // The hub at the center of the ring is opaque.
        let hub = ((12.4 / 18.0) * f64::from(ICON_PX)) as u32;
        assert_eq!(alpha(ICON_PX / 2, hub), 255);
        // Left end of the ring (used) is drawn stronger than the right end (unused) at 10 %.
        let low = gauge_icon(ICON_PX, 0.1, [0, 0, 0]);
        let ring_y = ((12.0 / 18.0) * f64::from(ICON_PX)) as u32;
        let at = |v: &[u8], x: u32| v[((ring_y * ICON_PX + x) * 4 + 3) as usize];
        let left = (2.0 / 18.0 * f64::from(ICON_PX)) as u32;
        let right = (16.0 / 18.0 * f64::from(ICON_PX)) as u32;
        assert!(at(&low, left) > 200 && at(&low, right) < 120, "{} {}", at(&low, left), at(&low, right));
        // Nearly full: both ends solid.
        let full = gauge_icon(ICON_PX, 1.0, [0, 0, 0]);
        assert!(at(&full, right) > 200);
    }
}
