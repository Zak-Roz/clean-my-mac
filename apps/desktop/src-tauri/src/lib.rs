//! Tauri shell: a thin layer of commands over cmm-core (ARCHITECTURE.md §7).

mod commands;

use cmm_core::Config;

pub fn run() {
    tauri::Builder::default()
        .manage(commands::AppState::new(Config::load()))
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running clean-my-mac");
}
