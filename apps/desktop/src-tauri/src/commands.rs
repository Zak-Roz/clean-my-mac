//! Tauri commands (ARCHITECTURE.md §7). All async, errors as readable `String`s.
//! Events: "scan://progress" (ScanProgress), "clean://item" (CleanItemResult).
//!
//! Everything that touches the disk or SQLite runs on a blocking thread so the webview never stalls.
//! Core functions that are still skeleton stubs surface as `"not implemented: …"` errors, which the
//! UI shows as "not available in this build yet" instead of failing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use cmm_core::actions::{self, CleanPlan};
use cmm_core::config::home_dir;
use cmm_core::history::History;
use cmm_core::model::{
    CleanItemResult, CleanOutcome, CleanRequest, CleanResult, FreeSpacePoint, GrowthItem, RuleDef, ScanOptions,
    ScanReport, VolumeInfo,
};
use cmm_core::{Config, CoreError, rules, volume};
use tauri::{AppHandle, Emitter, Manager, State};

pub const SCAN_PROGRESS_EVENT: &str = "scan://progress";
pub const CLEAN_ITEM_EVENT: &str = "clean://item";

/// System Settings › Privacy & Security › Full Disk Access.
const FULL_DISK_ACCESS_URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

pub struct AppState {
    pub config: Config,
    pub last_report: Mutex<Option<ScanReport>>,
    /// Opened lazily on first use and kept once it succeeds; `None` while unavailable.
    history: Mutex<Option<History>>,
    scanning: AtomicBool,
    cleaning: AtomicBool,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            last_report: Mutex::new(None),
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

    /// Runs `f` with the history database, opening it first if needed. Blocking: call from a
    /// blocking thread.
    fn with_history<T>(&self, f: impl FnOnce(&History) -> Result<T, CoreError>) -> Result<T, String> {
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

fn core_err(e: CoreError) -> String {
    e.to_string()
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
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let _busy = BusyGuard::acquire(&state.scanning, "a scan")?;
        let rules = state.load_rules()?;
        let report = cmm_core::scan(&rules, &opts, &state.config, &|progress| {
            let _ = app.emit(SCAN_PROGRESS_EVENT, progress);
        });
        // History is a nice-to-have: a failure here must not lose the scan.
        if let Err(e) = state.with_history(|h| h.record_scan(&report)) {
            eprintln!("cmm: scan not recorded to history: {e}");
        }
        *state.last_report.lock().map_err(|e| e.to_string())? = Some(report.clone());
        Ok(report)
    })
    .await
    .map_err(join_err)?
}

#[tauri::command]
pub async fn last_report(state: State<'_, AppState>) -> Result<Option<ScanReport>, String> {
    state.last_report_clone()
}

#[tauri::command]
pub async fn plan_clean(app: AppHandle, req: CleanRequest) -> Result<CleanPlan, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let report = state.last_report_clone()?.ok_or("No scan yet — run a scan first")?;
        actions::plan(&report, &req).map_err(core_err)
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
        let emit = |item: &CleanItemResult| {
            let _ = app.emit(CLEAN_ITEM_EVENT, item);
        };
        // The audit log (§6.6) is written by `execute` when history is available. Without it the
        // clean still runs, exactly like `cmm clean` does.
        let result = match state.with_history(|h| Ok(actions::execute(&report, &req, Some(h), &emit))) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("cmm: cleaning without history: {e}");
                actions::execute(&report, &req, None, &emit)
            }
        };
        if let Some(stored) = state.last_report.lock().map_err(|e| e.to_string())?.as_mut() {
            prune_cleaned(stored, &result);
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

#[tauri::command]
pub async fn open_full_disk_access_settings() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| run_open(&[FULL_DISK_ACCESS_URL.as_ref()])).await.map_err(join_err)?
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
    fn history_reports_unavailable_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config { data_dir: dir.path().join("data"), ..Config::default() };
        let state = AppState::new(config);
        // Works whether S3's History is implemented (Ok) or still a stub (readable Err).
        match state.with_history(|h| h.free_space(7)) {
            Ok(points) => assert!(points.is_empty()),
            Err(e) => assert!(e.contains("history"), "{e}"),
        }
    }
}
