//! Storage view commands (ARCHITECTURE.md §7, §11): the read-only breakdown of the used space.
//!
//! `measure_storage` walks the disk on a blocking thread (tens of seconds) and emits
//! "storage://progress" (StorageProgress) meanwhile. The result is kept in memory and in
//! `data_dir/last-storage.json`, so reopening the screen or the app shows it at once;
//! `storage_cached` returns it with the found-by-scan marks recomputed for the current last report
//! (cheap, no walk). The UI measures again when a scan finished after the stored measurement.
//! Nothing here writes outside `data_dir` or changes anything on disk.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use cmm_core::Config;
use cmm_core::config::home_dir;
use cmm_core::model::{StorageBreakdown, StorageNode};
use cmm_core::storage;
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::{AppState, BusyGuard, join_err, validate_reveal_path};

pub const STORAGE_PROGRESS_EVENT: &str = "storage://progress";

/// File name of the persisted last breakdown inside `data_dir`.
pub const LAST_STORAGE_FILE: &str = "last-storage.json";

pub struct StorageState {
    path: PathBuf,
    /// `None` until first used, then the persisted breakdown (if any) or the latest measurement.
    cache: Mutex<Option<Option<StorageBreakdown>>>,
    measuring: AtomicBool,
}

impl StorageState {
    pub fn new(config: &Config) -> Self {
        Self {
            path: config.data_dir.join(LAST_STORAGE_FILE),
            cache: Mutex::new(None),
            measuring: AtomicBool::new(false),
        }
    }

    /// The cached breakdown, loading the persisted one on first use.
    fn cached(&self) -> Result<Option<StorageBreakdown>, String> {
        let mut cache = self.cache.lock().map_err(|e| e.to_string())?;
        Ok(cache.get_or_insert_with(|| load(&self.path)).clone())
    }

    fn store(&self, breakdown: &StorageBreakdown) -> Result<(), String> {
        if let Err(e) = save(&self.path, breakdown) {
            eprintln!("cmm: storage breakdown not saved to {}: {e}", self.path.display());
        }
        *self.cache.lock().map_err(|e| e.to_string())? = Some(Some(breakdown.clone()));
        Ok(())
    }
}

/// Reads a persisted breakdown. Missing, unreadable or outdated (does not match the model) → None.
fn load(path: &Path) -> Option<StorageBreakdown> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).map_err(|e| eprintln!("cmm: ignoring unreadable {}: {e}", path.display())).ok()
}

/// Writes atomically (temp file in the same directory, then rename).
fn save(path: &Path, breakdown: &StorageBreakdown) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(breakdown).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// The last measurement with found marks for the current last report, or None if there is none.
#[tauri::command]
pub async fn storage_cached(app: AppHandle) -> Result<Option<StorageBreakdown>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(mut breakdown) = app.state::<StorageState>().cached()? else { return Ok(None) };
        let report = app.state::<AppState>().last_report_clone()?;
        storage::annotate(&mut breakdown, report.as_ref());
        Ok(Some(breakdown))
    })
    .await
    .map_err(join_err)?
}

/// Measures the disk again (read-only walk), with "storage://progress" events.
#[tauri::command]
pub async fn measure_storage(app: AppHandle) -> Result<StorageBreakdown, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<StorageState>();
        let _busy = BusyGuard::acquire(&state.measuring, "a storage measurement")?;
        let report = app.state::<AppState>().last_report_clone()?;
        let breakdown = storage::breakdown(&home_dir(), report.as_ref(), &|progress| {
            let _ = app.emit(STORAGE_PROGRESS_EVENT, progress);
        });
        state.store(&breakdown)?;
        Ok(breakdown)
    })
    .await
    .map_err(join_err)?
}

/// The entries of one folder, sized, for drilling below what the breakdown kept.
#[tauri::command]
pub async fn storage_children(app: AppHandle, path: String) -> Result<StorageNode, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = validate_reveal_path(&path)?;
        let report = app.state::<AppState>().last_report_clone()?;
        Ok(storage::children(dir, report.as_ref()).0)
    })
    .await
    .map_err(join_err)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breakdown() -> StorageBreakdown {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Users/you/Library")).unwrap();
        std::fs::write(dir.path().join("Users/you/Library/a.bin"), vec![1u8; 10_000]).unwrap();
        let env = storage::StorageEnv {
            home: dir.path().join("Users/you"),
            data_root: dir.path().to_path_buf(),
            data_used_bytes: 1_000_000,
            total_bytes: 2_000_000,
            free_bytes: 1_000_000,
            ..storage::StorageEnv::default()
        };
        storage::breakdown_with(&env, None, &storage::BREAKDOWN_TREE, &|_| {})
    }

    #[test]
    fn last_breakdown_round_trips_and_tolerates_missing_or_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config { data_dir: dir.path().join("data"), ..Config::default() };
        let state = StorageState::new(&config);
        assert!(state.cached().unwrap().is_none(), "nothing measured yet");

        let measured = breakdown();
        state.store(&measured).unwrap();
        let path = config.data_dir.join(LAST_STORAGE_FILE);
        assert!(path.exists() && !path.with_extension("json.tmp").exists());
        let reloaded = StorageState::new(&config).cached().unwrap().expect("loaded from disk");
        assert_eq!(serde_json::to_value(&reloaded).unwrap(), serde_json::to_value(&measured).unwrap());

        for corrupt in [&b"{ nope"[..], b"", br#"{"nodes": []}"#] {
            std::fs::write(&path, corrupt).unwrap();
            assert!(StorageState::new(&config).cached().unwrap().is_none());
        }
    }
}
