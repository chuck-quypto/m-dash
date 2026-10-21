fn main() {
    let snapshot = tauri::async_runtime::block_on(m_dash_lib::claude::collect())
        .unwrap_or_else(|message| panic!("{message}"));
    println!(
        "{}",
        serde_json::json!({
            "observedAt": snapshot["quotaObservedAt"],
            "windows": snapshot["windows"],
            "source": snapshot["source"]
        })
    );
}
