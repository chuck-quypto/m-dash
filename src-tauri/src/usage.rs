//! Provider-neutral accounting records shared by the token collectors and the burn ledger.

/// One model request's accounting metadata. Counts are tokens; cost is the provider's
/// API-equivalent estimate in millionths of a US dollar when it is reported.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    /// Where the request ran: `cli`, `desktop`, `cloud`, an SDK entrypoint, or `codex`.
    pub surface: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub speed: Option<String>,
    /// The provider's purpose label, such as a main thread, subagent or compaction.
    pub query_source: Option<String>,
    /// Present for subagent requests.
    pub agent: Option<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: Option<i64>,
    pub cost_micros: Option<i64>,
}

/// Allowance windows in a normalized snapshot, as (window id, used percent, reset time).
pub fn windows(snapshot: &serde_json::Value) -> Vec<(String, f64, Option<i64>)> {
    snapshot["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|window| {
            let id = window["id"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 128)?;
            let used = window["usedPercent"].as_f64().or_else(|| {
                window["remainingPercent"]
                    .as_f64()
                    .map(|remaining| 100.0 - remaining)
            })?;
            used.is_finite()
                .then(|| (id.to_owned(), used, window["resetsAt"].as_i64()))
        })
        .collect()
}
