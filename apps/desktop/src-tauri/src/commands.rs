//! Tauri commands (ARCHITECTURE.md §7). All async, errors as readable `String`s.
//! Events: "scan://progress" (ScanProgress), "clean://item" (CleanItemResult); scans started from
//! the menu bar also emit "scan://started", "scan://finished" (ScanReport) or "scan://failed"
//! (see `tray.rs`).
//!
//! The daily background scan (LaunchAgent) is installed and removed only through `agent_install` /
//! `agent_uninstall`, i.e. when the user flips the Settings toggle — never automatically.
//!
//! The last full scan is persisted to `data_dir/last-report.json` (contract v2): written after each
//! full scan and after a real clean prunes it, loaded at startup; a missing or unreadable file just
//! means "no scan yet".
//!
//! The stored report is for display and selection only (safety fix 1): `plan_clean` and
//! `execute_clean` re-scan the rules of the selected findings and act only on findings that are
//! still exactly what the user picked (`actions::plan_fresh` / `actions::execute_fresh`). A real
//! clean is refused when the history database (audit log) cannot be opened (safety fix 5).
//!
//! Everything that touches the disk or SQLite runs on a blocking thread so the webview never stalls.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use cmm_core::actions::{self, CleanPlan};
use cmm_core::agent::{self, AgentSpec, AgentStatus, SystemLaunchd};
use cmm_core::config::home_dir;
use cmm_core::history::History;
use cmm_core::model::{
    CleanItemResult, CleanOutcome, CleanRequest, CleanResult, FreeSpacePoint, GrowthItem, RuleDef, ScanOptions,
    ScanReport, VolumeInfo,
};
use cmm_core::monitor::MonitorSettings;
use cmm_core::{Config, CoreError, rules, volume};
use tauri::{AppHandle, Emitter, Manager, State};

pub const SCAN_PROGRESS_EVENT: &str = "scan://progress";
pub const CLEAN_ITEM_EVENT: &str = "clean://item";

/// System Settings › Privacy & Security › Full Disk Access.
const FULL_DISK_ACCESS_URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// File name of the persisted last report inside `data_dir`.
pub const LAST_REPORT_FILE: &str = "last-report.json";

pub struct AppState {
    pub config: Config,
    pub last_report: Mutex<Option<ScanReport>>,
    /// Opened lazily on first use and kept once it succeeds; `None` while unavailable.
    history: Mutex<Option<History>>,
    scanning: AtomicBool,
    cleaning: AtomicBool,
}

impl AppState {
    /// State with the persisted last report (if any) already loaded.
    pub fn new(config: Config) -> Self {
        let last_report = load_report(&config.data_dir.join(LAST_REPORT_FILE));
        Self {
            config,
            last_report: Mutex::new(last_report),
            history: Mutex::new(None),
            scanning: AtomicBool::new(false),
            cleaning: AtomicBool::new(false),
        }
    }

    fn load_rules(&self) -> Result<Vec<RuleDef>, String> {
        rules::load_rules(Some(&self.config.data_dir.join("rules.d"))).map_err(core_err)
    }

    fn history_path(&self) -> PathBuf {
        self.config.data_dir.join("history.sqlite")
    }

    /// Persists `report` as the last report; a failure is logged, never fatal.
    fn persist_report(&self, report: &ScanReport) {
        let path = self.config.data_dir.join(LAST_REPORT_FILE);
        if let Err(e) = save_report(&path, report) {
            eprintln!("cmm: last report not saved to {}: {e}", path.display());
        }
    }

    /// Runs `f` with the history database, opening it first if needed. Blocking: call from a
    /// blocking thread.
    pub(crate) fn with_history<T>(&self, f: impl FnOnce(&History) -> Result<T, CoreError>) -> Result<T, String> {
        let mut slot = self.history.lock().map_err(|e| e.to_string())?;
        if slot.is_none() {
            let path = self.history_path();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("history unavailable: {e}"))?;
            }
            *slot = Some(History::open(&path).map_err(|e| format!("history unavailable: {e}"))?);
        }
        let history = slot.as_ref().expect("history opened above");
        f(history).map_err(core_err)
    }

    fn last_report_clone(&self) -> Result<Option<ScanReport>, String> {
        Ok(self.last_report.lock().map_err(|e| e.to_string())?.clone())
    }

    pub(crate) fn is_scanning(&self) -> bool {
        self.scanning.load(Ordering::SeqCst)
    }
}

/// Clears a busy flag when dropped, so a panic or early return never leaves it stuck.
struct BusyGuard<'a>(&'a AtomicBool);

impl<'a> BusyGuard<'a> {
    fn acquire(flag: &'a AtomicBool, what: &str) -> Result<Self, String> {
        if flag.swap(true, Ordering::SeqCst) {
            return Err(format!("{what} is already running"));
        }
        Ok(Self(flag))
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Reads a persisted report. Missing, unreadable or outdated (does not match the model) → `None`.
fn load_report(path: &Path) -> Option<ScanReport> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("cmm: ignoring {}: {e}", path.display());
            return None;
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| eprintln!("cmm: ignoring unreadable {}: {e}", path.display())).ok()
}

/// Writes `report` atomically (temp file in the same directory, then rename).
fn save_report(path: &Path, report: &ScanReport) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec(report).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn core_err(e: CoreError) -> String {
    e.to_string()
}

/// The error a real clean returns when the history database (audit log) cannot be opened.
fn refuse_without_history(e: &str) -> String {
    format!("Nothing was cleaned: the audit log can't be written ({e}). Every removal is recorded there first.")
}

fn join_err(e: tauri::Error) -> String {
    format!("background task failed: {e}")
}

/// Drops the findings a real (non-dry-run) clean handled successfully, so the stored report and the
/// UI do not offer them again.
fn prune_cleaned(report: &mut ScanReport, result: &CleanResult) {
    if result.dry_run {
        return;
    }
    let done: std::collections::HashSet<&str> =
        result.items.iter().filter(|i| i.outcome == CleanOutcome::Done).map(|i| i.finding_id.as_str()).collect();
    report.findings.retain(|f| !done.contains(f.id.as_str()));
}

/// `open -R` refuses relative paths with a confusing error and would treat a leading `-` as an
/// option, so only absolute paths that exist are passed on.
fn validate_reveal_path(path: &str) -> Result<&Path, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("not an absolute path: {path}"));
    }
    if p.symlink_metadata().is_err() {
        return Err(format!("{path} no longer exists"));
    }
    Ok(p)
}

fn run_open(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let output = Command::new("/usr/bin/open").args(args).output().map_err(|e| format!("open failed: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("open failed: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

#[tauri::command]
pub async fn get_volume() -> Result<VolumeInfo, String> {
    tauri::async_runtime::spawn_blocking(|| volume::volume_info(&home_dir())).await.map_err(join_err)
}

#[tauri::command]
pub async fn list_rules(state: State<'_, AppState>) -> Result<Vec<RuleDef>, String> {
    state.load_rules()
}

#[tauri::command]
pub async fn start_scan(app: AppHandle, opts: ScanOptions) -> Result<ScanReport, String> {
    tauri::async_runtime::spawn_blocking(move || run_scan(&app, &opts)).await.map_err(join_err)?
}

/// A full scan, as the menu bar's "Scan Now" runs it. Blocking.
pub(crate) fn run_full_scan(app: &AppHandle) -> Result<ScanReport, String> {
    run_scan(app, &ScanOptions::default())
}

/// Scans with progress events, records full scans to history, keeps the report and refreshes the
/// menu bar. Blocking: call from a blocking thread.
fn run_scan(app: &AppHandle, opts: &ScanOptions) -> Result<ScanReport, String> {
    let result = (|| {
        let state = app.state::<AppState>();
        let _busy = BusyGuard::acquire(&state.scanning, "a scan")?;
        crate::tray::refresh(app, false); // "Scanning…"
        let rules = state.load_rules()?;
        let report = cmm_core::scan(&rules, opts, &state.config, &|progress| {
            let _ = app.emit(SCAN_PROGRESS_EVENT, progress);
        });
        // History compares whole scans, so only unfiltered ones are recorded (contract v2). It is
        // a nice-to-have: a failure here must not lose the scan.
        if cmm_core::scan::is_full_scan(opts) {
            if let Err(e) = state.with_history(|h| h.record_scan(&report)) {
                eprintln!("cmm: scan not recorded to history: {e}");
            }
            state.persist_report(&report);
        }
        *state.last_report.lock().map_err(|e| e.to_string())? = Some(report.clone());
        Ok(report)
    })();
    crate::tray::refresh(app, true);
    result
}

#[tauri::command]
pub async fn last_report(state: State<'_, AppState>) -> Result<Option<ScanReport>, String> {
    state.last_report_clone()
}

/// Plans against a fresh scan of the selected findings' rules (it shows what would really happen;
/// whatever changed since the stored scan is listed as "no longer matches the scan").
#[tauri::command]
pub async fn plan_clean(app: AppHandle, req: CleanRequest) -> Result<CleanPlan, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let report = state.last_report_clone()?.ok_or("No scan yet — run a scan first")?;
        let rules = state.load_rules()?;
        actions::plan_fresh(&rules, &report, &req, &state.config, &home_dir()).map_err(core_err)
    })
    .await
    .map_err(join_err)?
}

#[tauri::command]
pub async fn execute_clean(app: AppHandle, req: CleanRequest) -> Result<CleanResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let _busy = BusyGuard::acquire(&state.cleaning, "a clean")?;
        let report = state.last_report_clone()?.ok_or("No scan yet — run a scan first")?;
        let rules = state.load_rules()?;
        let home = home_dir();
        let emit = |item: &CleanItemResult| {
            let _ = app.emit(CLEAN_ITEM_EVENT, item);
        };
        let run = |history: Option<&History>| {
            actions::execute_fresh(&rules, &report, &req, &state.config, &home, history, &emit)
        };
        // Every executed action goes to the audit log (§6.6): a real clean without it is refused.
        let result = match state.with_history(|h| Ok(run(Some(h)))) {
            Ok(result) => result,
            Err(e) if req.dry_run => {
                eprintln!("cmm: dry run without history: {e}");
                run(None)
            }
            Err(e) => return Err(refuse_without_history(&e)),
        };
        if let Some(stored) = state.last_report.lock().map_err(|e| e.to_string())?.as_mut()
            && !result.dry_run
        {
            prune_cleaned(stored, &result);
            state.persist_report(stored);
        }
        if !result.dry_run {
            crate::tray::refresh(&app, false);
        }
        Ok(result)
    })
    .await
    .map_err(join_err)?
}

#[tauri::command]
pub async fn history_growth(app: AppHandle, days: u32) -> Result<Vec<GrowthItem>, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<AppState>().with_history(|h| h.growth(days)))
        .await
        .map_err(join_err)?
}

#[tauri::command]
pub async fn history_free_space(app: AppHandle, days: u32) -> Result<Vec<FreeSpacePoint>, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<AppState>().with_history(|h| h.free_space(days)))
        .await
        .map_err(join_err)?
}

#[tauri::command]
pub async fn reveal_in_finder(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = validate_reveal_path(&path)?;
        run_open(&["-R".as_ref(), p.as_os_str()])
    })
    .await
    .map_err(join_err)?
}

/// Opens `~/.Trash` in Finder (contract v2: there is no "Empty Trash" action; emptying it would
/// also erase unrelated files, so the user does that in Finder).
#[tauri::command]
pub async fn reveal_trash() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let trash = home_dir().join(".Trash");
        let p = validate_reveal_path(trash.to_str().ok_or("home path is not UTF-8")?)?;
        run_open(&[p.as_os_str()])
    })
    .await
    .map_err(join_err)?
}

#[tauri::command]
pub async fn open_full_disk_access_settings() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| run_open(&[FULL_DISK_ACCESS_URL.as_ref()])).await.map_err(join_err)?
}

/// Thresholds for the menu bar warning and the background scan's notifications.
#[tauri::command]
pub async fn monitor_settings(state: State<'_, AppState>) -> Result<MonitorSettings, String> {
    Ok(MonitorSettings::from(&state.config))
}

/// The `cmm` binary the agent would run: a `cmm` bundled next to the app, the usual install
/// locations, then `$PATH` (a Finder-launched app has a minimal `PATH`). Verified with `--version`.
fn resolve_cmm(home: &Path) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    agent::find_cmm(home, exe.as_deref(), std::env::var_os("PATH").as_deref())
}

fn agent_status_now(config: &Config) -> AgentStatus {
    let home = home_dir();
    let mut status = agent::status(&home, config, &SystemLaunchd);
    status.resolved_cmm = resolve_cmm(&home).map(|p| p.to_string_lossy().into_owned());
    status
}

/// The message shown when no `cmm` binary can be found for the background scan.
const CMM_NOT_FOUND: &str = "The cmm command-line tool was not found (looked next to the app, in ~/.cargo/bin, \
~/.local/bin, /opt/homebrew/bin, /usr/local/bin and PATH). Install it with `cargo install --path crates/cmm-cli`, \
then try again.";

#[tauri::command]
pub async fn agent_status(app: AppHandle) -> Result<AgentStatus, String> {
    tauri::async_runtime::spawn_blocking(move || Ok(agent_status_now(&app.state::<AppState>().config)))
        .await
        .map_err(join_err)?
}

/// Installs the daily background scan. Only ever called from the Settings toggle.
#[tauri::command]
pub async fn agent_install(app: AppHandle) -> Result<AgentStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let config = &app.state::<AppState>().config;
        let home = home_dir();
        let cmm = resolve_cmm(&home).ok_or(CMM_NOT_FOUND)?;
        agent::install(&AgentSpec::new(cmm, config), &home, config, &SystemLaunchd).map_err(core_err)?;
        Ok(agent_status_now(config))
    })
    .await
    .map_err(join_err)?
}

#[tauri::command]
pub async fn agent_uninstall(app: AppHandle) -> Result<AgentStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let config = &app.state::<AppState>().config;
        agent::uninstall(&home_dir(), &SystemLaunchd).map_err(core_err)?;
        Ok(agent_status_now(config))
    })
    .await
    .map_err(join_err)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use cmm_core::model::CleanMode;

    fn fixture() -> ScanReport {
        serde_json::from_str(include_str!("../../../../fixtures/sample-report.json")).unwrap()
    }

    fn item(id: &str, outcome: CleanOutcome) -> CleanItemResult {
        CleanItemResult { finding_id: id.into(), outcome, bytes: 1, message: None }
    }

    #[test]
    fn prune_cleaned_drops_only_successful_items() {
        let mut report = fixture();
        let before = report.findings.len();
        let first = report.findings[0].id.clone();
        let second = report.findings[1].id.clone();
        let result = CleanResult {
            dry_run: false,
            mode: CleanMode::Trash,
            items: vec![item(&first, CleanOutcome::Done), item(&second, CleanOutcome::Failed)],
            freed_bytes: 0,
            moved_to_trash_bytes: 1,
        };
        prune_cleaned(&mut report, &result);
        assert_eq!(report.findings.len(), before - 1);
        assert!(report.findings.iter().all(|f| f.id != first));
        assert!(report.findings.iter().any(|f| f.id == second));
    }

    #[test]
    fn prune_cleaned_ignores_dry_runs() {
        let mut report = fixture();
        let before = report.findings.len();
        let id = report.findings[0].id.clone();
        let result = CleanResult {
            dry_run: true,
            mode: CleanMode::Trash,
            items: vec![item(&id, CleanOutcome::Done)],
            freed_bytes: 0,
            moved_to_trash_bytes: 0,
        };
        prune_cleaned(&mut report, &result);
        assert_eq!(report.findings.len(), before);
    }

    #[test]
    fn reveal_rejects_relative_option_like_and_missing_paths() {
        assert!(validate_reveal_path("relative/path").is_err());
        assert!(validate_reveal_path("-a").is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_reveal_path(dir.path().to_str().unwrap()).is_ok());
        assert!(validate_reveal_path(dir.path().join("missing").to_str().unwrap()).is_err());
    }

    #[test]
    fn busy_guard_rejects_reentry_and_resets_on_drop() {
        let flag = AtomicBool::new(false);
        {
            let _guard = BusyGuard::acquire(&flag, "a scan").unwrap();
            assert_eq!(BusyGuard::acquire(&flag, "a scan").err().unwrap(), "a scan is already running");
        }
        assert!(BusyGuard::acquire(&flag, "a scan").is_ok());
    }

    #[test]
    fn last_report_round_trips_and_tolerates_missing_or_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config { data_dir: dir.path().join("data"), ..Config::default() };
        let path = config.data_dir.join(LAST_REPORT_FILE);

        // Missing data dir and file: no report, no panic.
        assert!(AppState::new(config.clone()).last_report_clone().unwrap().is_none());

        let report = fixture();
        save_report(&path, &report).unwrap();
        assert!(!path.with_extension("json.tmp").exists(), "temp file renamed away");
        let loaded = AppState::new(config.clone()).last_report_clone().unwrap().expect("loaded at startup");
        assert_eq!(serde_json::to_value(&loaded).unwrap(), serde_json::to_value(&report).unwrap());

        for corrupt in [&b"{ not json"[..], b"", br#"{"id": "x"}"#] {
            std::fs::write(&path, corrupt).unwrap();
            assert!(AppState::new(config.clone()).last_report_clone().unwrap().is_none());
        }
    }

    #[test]
    fn real_clean_is_refused_when_history_cannot_open() {
        let dir = tempfile::tempdir().unwrap();
        // `data_dir` is a file: the history database can never be created inside it.
        let data_dir = dir.path().join("data");
        std::fs::write(&data_dir, "not a directory").unwrap();
        let state = AppState::new(Config { data_dir, ..Config::default() });
        let err = state.with_history(|_| Ok(())).unwrap_err();
        assert!(err.contains("history unavailable"), "{err}");
        let msg = refuse_without_history(&err);
        assert!(msg.starts_with("Nothing was cleaned: the audit log can't be written"), "{msg}");
    }

    #[test]
    fn history_is_created_on_first_use() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config { data_dir: dir.path().join("data"), ..Config::default() };
        let state = AppState::new(config);
        let points = state.with_history(|h| h.free_space(7)).unwrap();
        assert!(points.is_empty());
        assert!(dir.path().join("data/history.sqlite").exists());
    }
}
