use serde_json::{json, Value};

struct Resource {
    key: Value,
    label: String,
    unit: &'static str,
    remaining: f64,
    consumed: f64,
    resets_at: Option<u64>,
}

fn resources(snapshot: &Value) -> Vec<Resource> {
    let mut result = Vec::new();
    if let Some(windows) = snapshot["windows"].as_array() {
        for window in windows {
            if let Some(remaining) = window["remainingPercent"]
                .as_f64()
                .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
            {
                result.push(Resource {
                    key: json!([
                        "quota",
                        window["id"],
                        window["resetsAt"],
                        window["durationMins"],
                        snapshot["plan"]
                    ]),
                    label: window["label"].as_str().unwrap_or("Allowance").to_string(),
                    unit: "percentage points",
                    remaining,
                    consumed: 100. - remaining,
                    resets_at: window["resetsAt"].as_u64(),
                });
            }
        }
    }
    if let Some(credits) = snapshot["credits"].as_array() {
        for credit in credits {
            let balance = credit["balance"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|n| n.is_finite());
            if let Some(remaining) = balance.filter(|_| credit["unlimited"] != true) {
                result.push(Resource {
                    key: json!([
                        "credit",
                        credit["id"],
                        snapshot["plan"],
                        snapshot["availableResets"]
                    ]),
                    label: format!("{} credits", credit["label"].as_str().unwrap_or("Provider")),
                    unit: "credits",
                    remaining,
                    consumed: -remaining,
                    resets_at: None,
                });
            }
        }
    }
    if let (Some(remaining), Some(consumed), Some(total)) = (
        snapshot["balanceUsd"].as_f64(),
        snapshot["totalUsageUsd"].as_f64(),
        snapshot["totalCreditsUsd"].as_f64(),
    ) {
        result.push(Resource {
            key: json!(["openrouter-usd", total]),
            label: "Prepaid balance".into(),
            unit: "USD",
            remaining,
            consumed,
            resets_at: None,
        });
    }
    if let Some(cents) = snapshot["prepaidCents"].as_f64() {
        result.push(Resource {
            key: json!(["grok-usd", snapshot["plan"], snapshot["periodEnd"]]),
            label: "Prepaid balance".into(),
            unit: "USD",
            remaining: cents / 100.,
            consumed: -cents / 100.,
            resets_at: None,
        });
    }
    if let Some(remaining) = snapshot["remainingPercent"].as_f64() {
        let resets_at = snapshot["periodEnd"]
            .as_str()
            .and_then(|text| {
                time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .and_then(|date| u64::try_from(date.unix_timestamp()).ok());
        result.push(Resource {
            key: json!(["grok-quota", snapshot["plan"], snapshot["periodEnd"]]),
            label: "Included allowance".into(),
            unit: "percentage points",
            remaining,
            consumed: 100. - remaining,
            resets_at,
        });
    }
    result
}

pub fn estimate(snapshot: &Value, observations: &[Value], now: u64) -> Vec<Value> {
    let latest = snapshot["observedAt"].as_u64().unwrap_or(0);
    resources(snapshot).into_iter().map(|resource| {
        let mut result = json!({"label":resource.label,"unit":resource.unit,"observedAt":latest,"perHour":null,"exhaustsAt":null,"resetsAt":resource.resets_at,
            "resetBeforeExhaustion":false,"sampleCount":0,"sampleMinutes":0,"reason":"Need at least 3 comparable readings across 15 minutes."});
        if now.saturating_sub(latest)>360 || latest>now {
            result["reason"]=json!("Latest reading is stale; refresh or resume activity."); return result;
        }
        if resource.resets_at.is_some_and(|reset|reset<=now) {
            result["reason"]=json!("The recorded reset has passed; waiting for a new window."); return result;
        }
        let mut samples = Vec::new();
        let mut newer_at = latest;
        let mut newer_consumed = resource.consumed;
        for observation in observations.iter().rev() {
            let Some(at) = observation["observedAt"].as_u64() else { break };
            if at>latest {continue}
            if latest-at>6*3600 || newer_at-at>900 { break }
            let Some(previous) = resources(observation).into_iter().find(|value|value.key==resource.key) else {break};
            if previous.consumed>newer_consumed+1e-9 { break }
            samples.push((at,previous.consumed));
            newer_at=at; newer_consumed=previous.consumed;
        }
        let Some(&(first_at,first_consumed)) = samples.last() else {return result};
        let elapsed = latest-first_at;
        result["sampleCount"]=json!(samples.len()); result["sampleMinutes"]=json!(elapsed/60);
        if samples.len()<3 || elapsed<900 || samples[0].0!=latest { return result }
        let rate = (resource.consumed-first_consumed)*3600./elapsed as f64;
        if !rate.is_finite() || rate<0. { return result }
        result["perHour"]=json!(rate);
        if rate<=1e-9 { result["reason"]=json!("No consumption observed in this interval."); return result }
        let exhausts_at = latest as f64 + resource.remaining.max(0.)/rate*3600.;
        // Very distant dates imply false precision and may exceed a platform's date range.
        if !exhausts_at.is_finite() || exhausts_at-latest as f64>366.*86400. {
            result["reason"]=json!("At this pace, exhaustion is beyond one year; no date estimated."); return result;
        }
        result["exhaustsAt"]=json!(exhausts_at);
        result["resetBeforeExhaustion"]=json!(resource.resets_at.is_some_and(|reset|(reset as f64)<=exhausts_at));
        result["reason"]=Value::Null;
        result
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn quota(at: u64, left: f64, reset: u64) -> Value {
        json!({"observedAt":at,"plan":"pro","windows":[{"id":"weekly","label":"Weekly","durationMins":10080,"remainingPercent":left,"resetsAt":reset}]})
    }
    #[test]
    fn quota_rate_needs_time_and_comparable_windows_and_stops_at_reset() {
        let observations = vec![
            quota(1000, 80., 10000),
            quota(1600, 75., 10000),
            quota(2200, 70., 10000),
        ];
        let result = estimate(&observations[2], &observations, 2200);
        assert_eq!(result[0]["perHour"], 30.);
        assert_eq!(result[0]["exhaustsAt"], 10600.);
        assert_eq!(result[0]["resetBeforeExhaustion"], true);
        assert!(estimate(&observations[2], &observations[..2], 2200)[0]["perHour"].is_null());
        assert!(estimate(&observations[2], &observations, 2600)[0]["perHour"].is_null());
        let reset = quota(2300, 99., 20000);
        assert!(estimate(
            &reset,
            &[
                observations[0].clone(),
                observations[2].clone(),
                reset.clone()
            ],
            2300
        )[0]["perHour"]
            .is_null());
        let gap = quota(4200, 60., 10000);
        assert!(estimate(
            &gap,
            &[
                observations[0].clone(),
                observations[2].clone(),
                gap.clone()
            ],
            4200
        )[0]["perHour"]
            .is_null());
    }
    fn credit(at: u64, purchased: f64, used: f64) -> Value {
        json!({"provider":"openrouter","observedAt":at,"totalCreditsUsd":purchased,"totalUsageUsd":used,"balanceUsd":purchased-used})
    }
    #[test]
    fn dollar_rate_restarts_after_topups_corrections_and_zero_use_has_no_eta() {
        let observations = vec![
            credit(1000, 10., 1.),
            credit(1600, 10., 2.),
            credit(2200, 10., 3.),
        ];
        let result = estimate(&observations[2], &observations, 2200);
        assert_eq!(result[0]["perHour"], 6.);
        assert_eq!(result[0]["exhaustsAt"], 6400.);
        for changed in [credit(2300, 20., 3.), credit(2300, 10., 0.5)] {
            assert!(estimate(
                &changed,
                &[
                    observations[0].clone(),
                    observations[2].clone(),
                    changed.clone()
                ],
                2300
            )[0]["perHour"]
                .is_null());
        }
        let idle = vec![
            credit(1000, 10., 1.),
            credit(1600, 10., 1.),
            credit(2200, 10., 1.),
        ];
        let result = estimate(&idle[2], &idle, 2200);
        assert_eq!(result[0]["perHour"], 0.);
        assert!(result[0]["exhaustsAt"].is_null());
    }
}
