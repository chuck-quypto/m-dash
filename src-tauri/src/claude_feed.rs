//! The installed executable doubles as Claude's bounded, metadata-only statusline helper.
use serde_json::{json, Value};
use std::io::Read;

pub fn run() {
    let result = (|| -> Result<(), String> {
        let directory = std::env::args_os()
            .nth(2)
            .ok_or("Missing bridge directory")?;
        let bytes = std::fs::read(std::path::Path::new(&directory).join("claude-bridge.json"))
            .map_err(|_| "Missing bridge configuration")?;
        let config: crate::claude_setup::Config =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid bridge configuration")?;
        if config.suspended || config.token.is_empty() {
            return Err("Local feed disabled".into());
        }
        let mut input = Vec::new();
        std::io::stdin()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut input)
            .map_err(|_| "Cannot read statusline")?;
        if input.len() > 1024 * 1024 {
            return Err("Statusline too large".into());
        }
        let payload: Value = serde_json::from_slice(&input).map_err(|_| "Invalid statusline")?;
        let (selected, text) = statusline(&payload);
        tauri::async_runtime::block_on(async {
            if let Ok(client) = reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(1))
                .build()
            {
                let _ = client
                    .post(format!(
                        "http://127.0.0.1:{}/claude/{}/statusline",
                        crate::claude_setup::PORT,
                        config.token
                    ))
                    .header("content-type", "application/json")
                    .body(selected.to_string())
                    .send()
                    .await;
            }
        });
        println!("{text}");
        Ok(())
    })();
    if result.is_err() {
        println!("m-dash: local feed unavailable");
    }
}

pub fn statusline(payload: &Value) -> (Value, String) {
    let limits: serde_json::Map<String, Value> = ["five_hour", "seven_day", "spend_limit"].into_iter()
        .filter_map(|key| payload["rate_limits"].get(key).filter(|v| v.is_object()).map(|row|
            (key.to_owned(), json!({"used_percentage":row["used_percentage"],"resets_at":row["resets_at"]}))))
        .collect();
    let selected = json!({"session_id":payload["session_id"],
        "cost":{"total_api_duration_ms":payload["cost"]["total_api_duration_ms"]},
        "rate_limits":limits});
    let parts: Vec<String> = [("five_hour", "5h"), ("seven_day", "7d")]
        .into_iter()
        .filter_map(|(key, label)| {
            payload["rate_limits"][key]["used_percentage"]
                .as_f64()
                .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
                .map(|used| format!("{label} {}% left", 100.0 - used))
        })
        .collect();
    let text = if parts.is_empty() {
        "m-dash: waiting for Claude usage".into()
    } else {
        parts.join(" | ")
    };
    (selected, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feed_preserves_reported_gateway_allowances() {
        let (selected, _) = statusline(
            &json!({"rate_limits":{"spend_limit":{"used_percentage":10,"resets_at":2000}}}),
        );
        assert_eq!(
            selected["rate_limits"]["spend_limit"],
            json!({"used_percentage":10,"resets_at":2000})
        );
    }
    #[test]
    fn feed_forwards_only_accounting_fields_and_displays_remaining_quota() {
        let (selected, text) = statusline(&json!({"session_id":"session-1", "prompt":"private",
            "cost":{"total_api_duration_ms":250,"private":"excluded"},
            "rate_limits":{"five_hour":{"used_percentage":25,"resets_at":2000,"private":"excluded"},"private":"excluded"}}));
        assert_eq!(text, "5h 75% left");
        assert_eq!(
            selected,
            json!({"session_id":"session-1","cost":{"total_api_duration_ms":250},
            "rate_limits":{"five_hour":{"used_percentage":25,"resets_at":2000}}})
        );
    }
}
