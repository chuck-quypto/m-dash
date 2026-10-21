use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Limits {
    account_id: Option<String>,
    ordinary_usage_allowed: Option<bool>,
    rate_limits: Bucket,
    rate_limits_by_limit_id: Option<BTreeMap<String, Bucket>>,
    rate_limit_reset_credits: Option<ResetInventory>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bucket {
    limit_id: Option<String>,
    limit_name: Option<String>,
    primary: Option<Window>,
    secondary: Option<Window>,
    credits: Option<Credits>,
    spend_control_reached: Option<bool>,
    individual_limit: Option<SpendControl>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    used_percent: f64,
    window_duration_mins: Option<u64>,
    resets_at: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credits {
    balance: Option<String>,
    unlimited: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpendControl {
    limit: String,
    used: String,
    remaining_percent: f64,
    resets_at: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetInventory {
    available_count: u64,
    credits: Option<Vec<Reset>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reset {
    title: Option<String>,
    expires_at: Option<u64>,
    status: String,
}

pub fn normalize(limits: Value) -> Result<Value, String> {
    let limits: Limits = serde_json::from_value(limits).map_err(|_| {
        "Codex returned an unexpected quota format. Update Codex and retry.".to_string()
    })?;
    let account_id = limits
        .account_id
        .filter(|id| !id.trim().is_empty())
        .ok_or("Codex did not identify the account for these limits. No history was combined.")?;
    let buckets = limits
        .rate_limits_by_limit_id
        .filter(|map| !map.is_empty())
        .unwrap_or_else(|| {
            BTreeMap::from([(
                limits
                    .rate_limits
                    .limit_id
                    .clone()
                    .unwrap_or_else(|| "codex".into()),
                limits.rate_limits,
            )])
        });
    let mut windows = Vec::new();
    let mut credits = Vec::new();
    for (id, bucket) in buckets {
        let label = bucket.limit_name.unwrap_or_else(|| {
            if id == "codex" {
                "Codex".into()
            } else {
                id.clone()
            }
        });
        for (kind, window) in [("primary", bucket.primary), ("secondary", bucket.secondary)] {
            if let Some(window) = window {
                if !window.used_percent.is_finite() || window.used_percent < 0.0 {
                    return Err("Codex returned an invalid usage percentage.".into());
                }
                windows.push(json!({"id":format!("{id}/{kind}"),"label":label,
                    "usedPercent":window.used_percent,"remainingPercent":(100.0-window.used_percent).max(0.0),
                    "durationMins":window.window_duration_mins.filter(|value| *value > 0),
                    "resetsAt":window.resets_at,"blocked":bucket.spend_control_reached}));
            }
        }
        if let Some(control) = bucket.individual_limit {
            if !control.remaining_percent.is_finite()
                || !(0.0..=100.0).contains(&control.remaining_percent)
            {
                return Err("Codex returned an invalid spending limit.".into());
            }
            windows.push(json!({"id":format!("{id}/spending"),"label":format!("{label} spending limit"),
                "usedPercent":100.0-control.remaining_percent,"remainingPercent":control.remaining_percent,
                "durationMins":null,"resetsAt":control.resets_at,"blocked":bucket.spend_control_reached,
                "limit":control.limit,"used":control.used}));
        }
        if let Some(credit) = bucket.credits {
            if let Some(balance) = &credit.balance {
                if !balance.parse::<f64>().is_ok_and(|value| value.is_finite()) {
                    return Err("Codex returned an invalid credit balance.".into());
                }
            }
            credits.push(json!({"id":id,"label":label,"balance":credit.balance,"unlimited":credit.unlimited}));
        }
    }
    let available_resets = limits
        .rate_limit_reset_credits
        .as_ref()
        .map(|inventory| inventory.available_count);
    let reset_details: Vec<Value> = limits
        .rate_limit_reset_credits
        .and_then(|inventory| inventory.credits)
        .unwrap_or_default()
        .into_iter()
        .filter(|credit| credit.status == "available")
        .map(|credit| json!({"title":credit.title,"expiresAt":credit.expires_at}))
        .collect();
    Ok(
        json!({"provider":"codex","accountId":account_id,"ordinaryUsageAllowed":limits.ordinary_usage_allowed,
        "windows":windows,"credits":credits,"availableResets":available_resets,"resetDetails":reset_details}),
    )
}

pub fn finish_snapshot(
    limits: Value,
    usage: Result<Value, String>,
    plan: Option<&str>,
    now: u64,
) -> Result<Value, String> {
    let mut snapshot = normalize(limits)?;
    snapshot["observedAt"] = json!(now);
    snapshot["plan"] = json!(plan);
    snapshot["lifetimeTokens"] = Value::Null;
    snapshot["peakDailyTokens"] = Value::Null;
    snapshot["dailyUsage"] = json!([]);
    snapshot["usageError"] = Value::Null;
    match usage.and_then(normalize_usage) {
        Ok(usage) => {
            snapshot["lifetimeTokens"] = usage["lifetimeTokens"].clone();
            snapshot["peakDailyTokens"] = usage["peakDailyTokens"].clone();
            snapshot["dailyUsage"] = usage["dailyUsage"].clone();
        }
        Err(message) => snapshot["usageError"] = json!(message),
    }
    Ok(snapshot)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    summary: UsageSummary,
    daily_usage_buckets: Option<Vec<DailyUsage>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageSummary {
    lifetime_tokens: Option<u64>,
    peak_daily_tokens: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DailyUsage {
    start_date: String,
    tokens: u64,
}

fn normalize_usage(value: Value) -> Result<Value, String> {
    let usage: Usage = serde_json::from_value(value)
        .map_err(|_| "Codex token history has an unexpected format.")?;
    let mut days = BTreeMap::new();
    for day in usage.daily_usage_buckets.unwrap_or_default() {
        let date = day.start_date.as_bytes();
        if date.len() != 10
            || date[4] != b'-'
            || date[7] != b'-'
            || !date
                .iter()
                .enumerate()
                .all(|(index, char)| index == 4 || index == 7 || char.is_ascii_digit())
            || day.tokens > i64::MAX as u64
        {
            return Err("Codex token history contains an invalid day or token total.".into());
        }
        days.insert(day.start_date, day.tokens);
    }
    Ok(
        json!({"lifetimeTokens":usage.summary.lifetime_tokens,"peakDailyTokens":usage.summary.peak_daily_tokens,
        "dailyUsage":days.into_iter().map(|(date,tokens)| json!({"date":date,"tokens":tokens})).collect::<Vec<_>>() }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_days_remain_account_totals_not_summed_local_usage() {
        let snapshot = finish_snapshot(json!({"accountId":"fixture","rateLimits":{}}), Ok(json!({
            "summary":{"lifetimeTokens":9000,"peakDailyTokens":5000},
            "dailyUsageBuckets":[{"startDate":"2026-10-02","tokens":2000},{"startDate":"2026-10-01","tokens":5000}]
        })), Some("pro"), 100).unwrap();
        assert_eq!(snapshot["lifetimeTokens"], 9000);
        assert_eq!(
            snapshot["dailyUsage"][0],
            json!({"date":"2026-10-01","tokens":5000})
        );
        assert!(snapshot["usageError"].is_null());
    }

    #[test]
    fn failed_token_read_keeps_fresh_quotas_and_marks_token_totals_unavailable() {
        let snapshot = finish_snapshot(
            json!({"accountId":"fixture","rateLimits":{}}),
            Err("Token service unavailable".into()),
            Some("pro"),
            100,
        )
        .unwrap();
        assert_eq!(snapshot["observedAt"], 100);
        assert_eq!(snapshot["plan"], "pro");
        assert!(snapshot["lifetimeTokens"].is_null());
        assert_eq!(snapshot["dailyUsage"], json!([]));
        assert_eq!(snapshot["usageError"], "Token service unavailable");
    }

    #[test]
    fn account_snapshot_prefers_named_buckets_without_duplicating_legacy_allowance() {
        let normalized = normalize(json!({
            "accountId":"fixture-account", "ordinaryUsageAllowed":false,
            "rateLimits":{"primary":{"usedPercent":99}},
            "rateLimitsByLimitId":{"codex":{
                "primary":{"usedPercent":41,"windowDurationMins":10080,"resetsAt":2000000000},
                "credits":{"balance":null,"hasCredits":false,"unlimited":false}
            }},
            "rateLimitResetCredits":{"availableCount":3,"credits":[]}
        }))
        .unwrap();
        assert_eq!(normalized["windows"].as_array().unwrap().len(), 1);
        assert_eq!(normalized["windows"][0]["remainingPercent"], 59.0);
        assert_eq!(normalized["ordinaryUsageAllowed"], false);
        assert_eq!(normalized["availableResets"], 3);
        assert!(normalized["credits"][0]["balance"].is_null());
    }
}
