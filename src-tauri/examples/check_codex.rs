fn main() {
    match tauri::async_runtime::block_on(m_dash_lib::collector::collect()) {
        Ok(snapshot) => println!(
            "{}",
            serde_json::to_string_pretty(&snapshot).expect("snapshot JSON")
        ),
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}
