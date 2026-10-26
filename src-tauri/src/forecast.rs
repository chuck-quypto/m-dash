use serde_json::{json, Value};

struct Resource {
    key: Value,
    label: String,
    unit: &'static str,
    remaining: f64,
    consumed: f64,
    resets_at: Option<u64>,
    // Allowance windows only: the provider's window id and length, for pace alerts.
    window: Option<(Value, u64)>,
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
                    window: window["durationMins"]
                        .as_u64()
                        .filter(|minutes| *minutes > 0)
                        .map(|minutes| (window["id"].clone(), minutes * 60)),
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
                    window: None,
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
            window: None,
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
            window: None,
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
            window: None,
        });
    }
    result
}

// Readings of one resource, newest first, back to `max_age` seconds before `latest`. A gap over
// 15 minutes, a different resource key or a fall in consumption (reset, top-up, correction) ends the run.
fn samples(
    resource: &Resource,
    observations: &[Value],
    latest: u64,
    max_age: u64,
) -> Vec<(u64, f64)> {
    let mut samples = Vec::new();
    let mut newer_at = latest;
    let mut newer_consumed = resource.consumed;
    for observation in observations.iter().rev() {
        let Some(at) = observation["observedAt"].as_u64() else {
            break;
        };
        if at > latest {
            continue;
        }
        if latest - at > max_age || newer_at - at > 900 {
            break;
        }
        let Some(previous) = resources(observation)
            .into_iter()
            .find(|value| value.key == resource.key)
        else {
            break;
        };
        if previous.consumed > newer_consumed + 1e-9 {
            break;
        }
        samples.push((at, previous.consumed));
        newer_at = at;
        newer_consumed = previous.consumed;
    }
    samples
}

// Colors and markers for an allowance window, as agreed in issue #4. Amber ("warning") means the
// allowance may run out before its reset, red ("critical") that it will run out well before it, and
// "surge" that the last stretch ran far above the window's average on an otherwise healthy bar.
fn alert(resource: &Resource, observations: &[Value], latest: u64, result: &mut Value) {
    let remaining = resource.remaining;
    let mut level = if remaining < 5. {
        2
    } else if remaining < 20. {
        1
    } else {
        0
    };
    let mut reason = "low";
    let (Some((id, length)), Some(reset)) = (&resource.window, resource.resets_at) else {
        set_alert(result, level, reason, None);
        return;
    };
    result["windowId"] = id.clone();
    let opened = reset.saturating_sub(*length);
    // The average is meaningless early in a window, so pace rules wait for a tenth of it to pass.
    if latest <= opened || reset <= latest || (latest - opened) * 10 < *length {
        set_alert(result, level, reason, None);
        return;
    }
    let left = (reset - latest) as f64;
    let average = resource.consumed.max(0.) / (latest - opened) as f64;
    let projected = average * left;
    let pace = if projected > 2. * remaining {
        2
    } else if projected > remaining {
        1
    } else {
        0
    };
    let mut runs_out = None;
    if pace > 0 && pace >= level {
        level = pace;
        reason = "pace";
        runs_out = Some(latest as f64 + remaining / average);
    }
    if level == 0 {
        // Recent pace: the last 30 minutes of a window up to five hours long, else the last 2 hours.
        let lookback = if *length <= 5 * 3600 { 1800 } else { 7200 };
        let recent = samples(resource, observations, latest, lookback);
        if let Some(&(first_at, first_consumed)) = recent.last() {
            let span = latest - first_at;
            let used = resource.consumed - first_consumed;
            if recent.len() >= 3 && recent[0].0 == latest && span * 2 >= lookback && used >= 3. {
                let rate = used / span as f64;
                if rate >= 2. * average && rate * left > remaining {
                    reason = "surge";
                    runs_out = Some(latest as f64 + remaining / rate);
                    result["recentMinutes"] = json!(span / 60);
                    result["surgeRatio"] = if average > 0. {
                        json!(rate / average)
                    } else {
                        Value::Null
                    };
                    set_alert(result, 3, reason, runs_out);
                    return;
                }
            }
        }
    }
    set_alert(result, level, reason, runs_out);
}

fn set_alert(result: &mut Value, level: u8, reason: &str, runs_out: Option<f64>) {
    result["alert"] = match level {
        1 => json!("warning"),
        2 => json!("critical"),
        3 => json!("surge"),
        _ => Value::Null,
    };
    result["alertReason"] = if level == 0 {
        Value::Null
    } else {
        json!(reason)
    };
    result["runsOutAt"] = runs_out.map_or(Value::Null, |at| json!(at.round()));
}

pub fn estimate(snapshot: &Value, observations: &[Value], now: u64) -> Vec<Value> {
    let latest = snapshot["observedAt"].as_u64().unwrap_or(0);
    resources(snapshot).into_iter().map(|resource| {
        let mut result = json!({"label":resource.label,"unit":resource.unit,"observedAt":latest,"perHour":null,"exhaustsAt":null,"resetsAt":resource.resets_at,
            "resetBeforeExhaustion":false,"sampleCount":0,"sampleMinutes":0,"reason":"Need at least 3 comparable readings across 15 minutes.",
            "windowId":null,"alert":null,"alertReason":null,"runsOutAt":null,"recentMinutes":null,"surgeRatio":null});
        if now.saturating_sub(latest)>360 || latest>now {
            result["reason"]=json!("Latest reading is stale; refresh or resume activity."); return result;
        }
        if resource.resets_at.is_some_and(|reset|reset<=now) {
            result["reason"]=json!("The recorded reset has passed; waiting for a new window."); return result;
        }
        if resource.unit == "percentage points" {
            alert(&resource, observations, latest, &mut result);
        }
        let samples = samples(&resource, observations, latest, 6*3600);
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
    const WEEK: u64 = 7 * 86400;

    // A weekly window opened at 0 and resetting at WEEK, read at `at` with `left` percent remaining.
    fn alert_at(at: u64, left: f64, history: &[(u64, f64)]) -> Value {
        let mut observations: Vec<Value> =
            history.iter().map(|&(t, l)| quota(t, l, WEEK)).collect();
        let snapshot = quota(at, left, WEEK);
        observations.push(snapshot.clone());
        estimate(&snapshot, &observations, at).remove(0)
    }

    #[test]
    fn window_average_pace_sets_amber_and_red() {
        let half = WEEK / 2;
        let on_track = alert_at(half, 50., &[]);
        assert_eq!(
            (on_track["alert"].clone(), on_track["windowId"].clone()),
            (Value::Null, json!("weekly"))
        );
        let amber = alert_at(half, 40., &[]);
        assert_eq!(
            (amber["alert"].as_str(), amber["alertReason"].as_str()),
            (Some("warning"), Some("pace"))
        );
        assert_eq!(
            amber["runsOutAt"],
            json!((half as f64 + 40. / (60. / half as f64)).round())
        );
        let red = alert_at(half, 30., &[]);
        assert_eq!(
            (red["alert"].as_str(), red["alertReason"].as_str()),
            (Some("critical"), Some("pace"))
        );
        let exactly_twice = alert_at(half, 100. / 3., &[]);
        assert_eq!(
            exactly_twice["alert"].as_str(),
            Some("warning"),
            "red needs more than twice what is left"
        );
    }

    #[test]
    fn low_remaining_rules_apply_without_pace_and_early_windows_skip_pace() {
        let late = WEEK * 9 / 10;
        assert_eq!(
            alert_at(late, 15., &[])["alertReason"].as_str(),
            Some("low")
        );
        assert_eq!(alert_at(late, 15., &[])["alert"].as_str(), Some("warning"));
        assert_eq!(alert_at(late, 4., &[])["alert"].as_str(), Some("critical"));
        let early = WEEK / 20;
        assert!(
            alert_at(early, 50., &[])["alert"].is_null(),
            "pace waits for a tenth of the window"
        );
        assert_eq!(
            alert_at(WEEK / 10, 50., &[])["alert"].as_str(),
            Some("critical")
        );
    }

    #[test]
    fn a_fast_recent_stretch_on_a_healthy_bar_is_a_surge() {
        let half = WEEK / 2;
        let ramp = |from: f64| -> Vec<(u64, f64)> {
            (0..12)
                .map(|step| {
                    (
                        half - 7200 + step * 600,
                        100. - (from + (40. - from) * step as f64 / 12.),
                    )
                })
                .collect()
        };
        let surge = alert_at(half, 60., &ramp(34.));
        assert_eq!(
            (surge["alert"].as_str(), surge["alertReason"].as_str()),
            (Some("surge"), Some("surge"))
        );
        assert_eq!(surge["recentMinutes"], 120);
        assert!(surge["surgeRatio"].as_f64().unwrap() > 6.);
        assert!(surge["runsOutAt"].as_f64().unwrap() < WEEK as f64);
        assert!(
            alert_at(half, 60., &ramp(38.))["alert"].is_null(),
            "under 3 points is noise"
        );
        assert!(
            alert_at(half, 59., &[(half - 2400, 46.), (half - 1200, 44.)])["alert"].is_null(),
            "40 minutes of readings is too short"
        );
        assert_eq!(
            alert_at(half, 40., &ramp(14.))["alert"].as_str(),
            Some("warning"),
            "amber beats a surge"
        );
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
