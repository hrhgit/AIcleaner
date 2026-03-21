mod backend;
mod organizer_runtime;
mod persist;
mod scan_runtime;

use backend::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            let state = AppState::bootstrap(data_dir)?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            backend::settings_get,
            backend::settings_save,
            backend::credentials_get,
            backend::credentials_save,
            backend::settings_get_provider_models,
            backend::settings_browse_folder,
            backend::system_get_privilege,
            backend::system_request_elevation,
            backend::files_open_location,
            backend::files_clean,
            backend::scan_get_active,
            backend::scan_list_history,
            backend::scan_find_latest_for_path,
            backend::scan_delete_history,
            backend::scan_start,
            backend::scan_stop,
            backend::scan_get_result,
            backend::organize_get_capability,
            backend::organize_start,
            backend::organize_stop,
            backend::organize_get_result,
            backend::organize_apply,
            backend::organize_rollback
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
