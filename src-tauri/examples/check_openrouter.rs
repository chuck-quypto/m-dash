fn main() {
    let snapshot = tauri::async_runtime::block_on(m_dash_lib::openrouter::collect())
        .unwrap_or_else(|error| panic!("{}", error.message));
    println!(
        "{}",
        serde_json::json!({
            "balanceUsd":snapshot["balanceUsd"],
            "dailyBuckets":snapshot["dailyUsage"].as_array().map(Vec::len),
            "usageError":snapshot["usageError"]
        })
    );
    assert!(
        snapshot["usageError"].is_null(),
        "OpenRouter daily history could not be imported"
    );
}
