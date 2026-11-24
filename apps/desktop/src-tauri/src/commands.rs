//! Tauri commands (ARCHITECTURE.md §7). All async, errors as `String`.
//! Events: "scan://progress" (ScanProgress), "clean://item" (CleanItemResult).

use std::sync::Mutex;

use cmm_core::actions::CleanPlan;
use cmm_core::config::home_dir;
use cmm_core::model::{
    CleanRequest, CleanResult, FreeSpacePoint, GrowthItem, RuleDef, ScanOptions, ScanReport, VolumeInfo,
};
use cmm_core::{Config, rules, volume};
use tauri::{AppHandle, Emitter, State};

pub const SCAN_PROGRESS_EVENT: &str = "scan://progress";
#[allow(dead_code)] // emitted by execute_clean once implemented
pub const CLEAN_ITEM_EVENT: &str = "clean://item";

const NOT_IMPLEMENTED: &str = "not implemented";

pub struct AppState {
    pub config: Config,
    pub last_report: Mutex<Option<ScanReport>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self { config, last_report: Mutex::new(None) }
    }

    fn load_rules(&self) -> Result<Vec<RuleDef>, String> {
        rules::load_rules(Some(&self.config.data_dir.join("rules.d"))).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub async fn get_volume() -> Result<VolumeInfo, String> {
    tauri::async_runtime::spawn_blocking(|| volume::volume_info(&home_dir())).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_rules(state: State<'_, AppState>) -> Result<Vec<RuleDef>, String> {
    state.load_rules()
}

#[tauri::command]
pub async fn start_scan(app: AppHandle, state: State<'_, AppState>, opts: ScanOptions) -> Result<ScanReport, String> {
    let rules = state.load_rules()?;
    let config = state.config.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        cmm_core::scan(&rules, &opts, &config, &|progress| {
            let _ = app.emit(SCAN_PROGRESS_EVENT, progress);
        })
    })
    .await
    .map_err(|e| e.to_string())?;
    *state.last_report.lock().map_err(|e| e.to_string())? = Some(report.clone());
    Ok(report)
}

#[tauri::command]
pub async fn last_report(state: State<'_, AppState>) -> Result<Option<ScanReport>, String> {
    Ok(state.last_report.lock().map_err(|e| e.to_string())?.clone())
}

#[tauri::command]
pub async fn plan_clean(req: CleanRequest) -> Result<CleanPlan, String> {
    let _ = req;
    Err(NOT_IMPLEMENTED.into())
}

#[tauri::command]
pub async fn execute_clean(req: CleanRequest) -> Result<CleanResult, String> {
    let _ = req;
    Err(NOT_IMPLEMENTED.into())
}

#[tauri::command]
pub async fn history_growth(days: u32) -> Result<Vec<GrowthItem>, String> {
    let _ = days;
    Err(NOT_IMPLEMENTED.into())
}

#[tauri::command]
pub async fn history_free_space(days: u32) -> Result<Vec<FreeSpacePoint>, String> {
    let _ = days;
    Err(NOT_IMPLEMENTED.into())
}

#[tauri::command]
pub async fn reveal_in_finder(path: String) -> Result<(), String> {
    let _ = path;
    Err(NOT_IMPLEMENTED.into())
}

#[tauri::command]
pub async fn open_full_disk_access_settings() -> Result<(), String> {
    Err(NOT_IMPLEMENTED.into())
}
