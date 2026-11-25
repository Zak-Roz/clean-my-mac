//! Tauri shell: a thin layer of commands over cmm-core (ARCHITECTURE.md §7), plus the menu bar
//! item (`tray.rs`). Closing the window hides it; the app keeps running in the menu bar until
//! Quit (menu bar or ⌘Q). Clicking the Dock icon shows the window again.

mod commands;
mod preview;
mod storage;
mod tray;

use cmm_core::Config;
use tauri::{Manager, RunEvent, WindowEvent};

pub fn run() {
    let config = Config::load();
    let app = tauri::Builder::default()
        .manage(storage::StorageState::new(&config))
        .manage(commands::AppState::new(config))
        .manage(preview::PreviewState::default())
        .setup(|app| {
            if let Err(e) = tray::setup(app.handle()) {
                // The window still works without the menu bar item.
                eprintln!("cmm: menu bar item unavailable: {e}");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && window.app_handle().tray_by_id(tray::TRAY_ID).is_some()
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_volume,
            commands::list_rules,
            commands::start_scan,
            commands::last_report,
            commands::plan_clean,
            commands::execute_clean,
            commands::history_growth,
            commands::history_free_space,
            commands::reveal_in_finder,
            commands::reveal_trash,
            commands::open_full_disk_access_settings,
            commands::monitor_settings,
            commands::agent_status,
            commands::agent_install,
            commands::agent_uninstall,
            preview::thumbnail,
            preview::quick_look,
            storage::storage_cached,
            storage::measure_storage,
            storage::storage_children,
        ])
        .build(tauri::generate_context!())
        .expect("error while building clean-my-mac");
    app.run(|app, event| {
        if let RunEvent::Reopen { .. } = event {
            tray::show_main(app);
        }
    });
}
