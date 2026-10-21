mod cache;
mod sync;
use tauri::Manager;

fn cache_directory(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|_| "Sample storage unavailable".into())
}
#[tauri::command]
fn read_sample(app: tauri::AppHandle) -> Result<Option<String>, String> {
    cache::read(&cache_directory(&app)?)
}
#[tauri::command]
fn write_sample(app: tauri::AppHandle, contents: String) -> Result<(), String> {
    cache::write(&cache_directory(&app)?, &contents)
}
#[tauri::command]
fn clear_sample(app: tauri::AppHandle) -> Result<(), String> {
    cache::clear(&cache_directory(&app)?)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(sync::plugin())
        .manage(sync::State::default())
        .invoke_handler(tauri::generate_handler![
            sync::live_status,
            sync::live_pair,
            sync::live_pull,
            sync::live_clear,
            read_sample,
            write_sample,
            clear_sample
        ])
        .run(tauri::generate_context!())
        .expect("error running m-dash companion");
}
