use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Billing {
    subscription_tier: Option<String>,
    config: Config,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    credit_usage_percent: Option<f64>,
    current_period: Option<Period>,
    prepaid_balance: Option<Cent>,
    on_demand_cap: Option<Cent>,
    on_demand_used: Option<Cent>,
    is_unified_billing_user: Option<bool>,
}

#[derive(Deserialize)]
struct Cent {
    #[serde(default)]
    val: i64,
}

#[derive(Deserialize)]
struct Period {
    start: Option<String>,
    end: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

pub fn normalize(identity: Value, billing: Value, now: u64) -> Result<Value, String> {
    let principal = identity["principalId"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("Grok did not identify the signed-in account.")?;
    let kind = identity["principalType"]
        .as_str()
        .ok_or("Grok did not identify the account type.")?;
    let team = identity["teamId"]
        .as_str()
        .ok_or("Grok did not identify the account team.")?;
    let billing: Billing = serde_json::from_value(billing)
        .map_err(|_| "Grok returned an unexpected billing format.")?;
    let config = billing.config;
    if config
        .credit_usage_percent
        .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err("Grok returned an invalid allowance percentage.".into());
    }
    Ok(
        json!({"provider":"grok","accountId":format!("grok:{}",json!([kind,principal,team])),
        "observedAt":now,"plan":billing.subscription_tier,
        "remainingPercent":config.credit_usage_percent.map(|value| (100.0-value).max(0.0)),
        "prepaidCents":config.prepaid_balance.map(|value| value.val),
        "onDemandCapCents":config.on_demand_cap.map(|value| value.val),
        "onDemandUsedCents":config.on_demand_used.map(|value| value.val),
        "periodStart":config.current_period.as_ref().and_then(|period| period.start.as_ref()),
        "periodEnd":config.current_period.as_ref().and_then(|period| period.end.as_ref()),
        "periodType":config.current_period.as_ref().and_then(|period| period.kind.as_ref()),
        "unified":config.is_unified_billing_user}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prepaid_cents_do_not_become_subscription_allowance_and_missing_percentage_stays_unknown() {
        let snapshot = normalize(json!({"principalType":"user","principalId":"fixture","teamId":"team"}), json!({
            "subscription_tier":"SuperGrok", "config":{
                "prepaidBalance":{"val":375},"onDemandCap":{"val":0},"onDemandUsed":{"val":0},
                "currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","end":"2026-10-17T17:00:00Z"},"isUnifiedBillingUser":true
            }
        }), 100).unwrap();
        assert_eq!(snapshot["prepaidCents"], 375);
        assert!(snapshot["remainingPercent"].is_null());
        assert_eq!(snapshot["periodEnd"], "2026-10-17T17:00:00Z");
        assert_eq!(snapshot["observedAt"], 100);
        assert!(snapshot["accountId"].as_str().unwrap().contains("fixture"));
    }
}
