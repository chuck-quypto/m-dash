//! The burn ledger: compares how far each allowance window moved with the tokens observed
//! on this computer over the same time, and reports what the observed tokens cannot explain.
//!
//! Providers publish neither the token weights nor the scale behind their percentages, so the
//! rate is learned from the account's own history. Unseen usage (cloud sessions, chat apps,
//! background agents, other computers) can only add to a window, so the baseline rate comes
//! from the days that needed the fewest points per token, and anything above it is reported
//! as unattributed rather than as an error.
use serde_json::{json, Value};
use std::collections::BTreeMap;

const DAY: i64 = 86_400;
/// Days with fewer weighted tokens than this give too noisy a ratio to fit.
const MIN_WEIGHTED: f64 = 50_000.0;
/// Active days needed before rates are shown.
const MIN_DAYS: usize = 3;

pub struct Reading {
    pub window: String,
    pub at: i64,
    pub used: f64,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Default)]
pub struct Usage {
    pub at: i64,
    pub surface: String,
    pub model: String,
    pub query_source: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cost_micros: Option<i64>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Provider {
    Claude,
    Codex,
}

/// Relative price of each token type against fresh input, from the providers' published
/// price lists: Anthropic API prices for Claude and the Codex credit rate card for Codex
/// (which counts reasoning inside output). Model price differences are not applied.
fn weighted(provider: Provider, usage: &Usage) -> f64 {
    let (output, read, write) = match provider {
        Provider::Claude => (5.0, 0.1, 1.25),
        Provider::Codex => (6.0, 0.1, 1.0),
    };
    usage.input as f64
        + output * usage.output as f64
        + read * usage.cache_read as f64
        + write * usage.cache_write as f64
}

fn date(at: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(at.div_euclid(DAY) * DAY)
        .map(|day| {
            format!(
                "{:04}-{:02}-{:02}",
                day.year(),
                u8::from(day.month()),
                day.day()
            )
        })
        .unwrap_or_default()
}

/// A stretch between two readings of one window that moved, with the usage observed in it.
struct Segment {
    end: i64,
    points: f64,
    weighted: f64,
    cost: Option<f64>,
}

/// Splits a window's readings into stretches that each end where the percentage changed.
/// Readings are whole or rounded numbers, so a stretch that ends on a change carries a full
/// step and the usage that produced it. A reset (a drop of over a point or a new reset time) starts
/// over; usage before it belongs to the previous window.
fn segments(
    readings: &[&Reading],
    usage: &[Usage],
    provider: Provider,
) -> (Vec<Segment>, Vec<i64>) {
    let mut segments = Vec::new();
    let mut idle = Vec::new();
    let mut start: Option<&Reading> = None;
    for reading in readings {
        let Some(from) = start else {
            start = Some(reading);
            continue;
        };
        // Sources round differently (a poll's 41 after a log's 41.3), so a drop under one
        // point is noise; a larger drop or a new reset time is a new window.
        let reset = reading.used < from.used - 1.0
            || matches!((from.resets_at, reading.resets_at), (Some(a), Some(b)) if (a - b).abs() > 600);
        if reset {
            start = Some(reading);
            continue;
        }
        if reading.used <= from.used {
            continue;
        }
        // `usage` is sorted by time, so each stretch is one contiguous slice.
        let window = &usage[usage.partition_point(|row| row.at <= from.at)
            ..usage.partition_point(|row| row.at <= reading.at)];
        let weighted: f64 = window.iter().map(|row| weighted(provider, row)).sum();
        let cost = window
            .iter()
            .map(|row| row.cost_micros)
            .sum::<Option<i64>>()
            .map(|micros| micros as f64 / 1e6);
        let points = reading.used - from.used;
        if window.is_empty() {
            idle.push(reading.at);
        }
        segments.push(Segment {
            end: reading.at,
            points,
            weighted,
            cost: if window.is_empty() { None } else { cost },
        });
        start = Some(reading);
    }
    (segments, idle)
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// The baseline rate: total points over total weight on the lower half of days by ratio.
fn baseline(days: &[(f64, f64)]) -> Option<f64> {
    let mut active: Vec<(f64, f64)> = days
        .iter()
        .copied()
        .filter(|(points, weight)| *weight >= MIN_WEIGHTED && *points > 0.0)
        .collect();
    if active.len() < MIN_DAYS {
        return None;
    }
    active.sort_by(|a, b| (a.0 / a.1).total_cmp(&(b.0 / b.1)));
    let lower = &active[..active.len().div_ceil(2)];
    let points: f64 = lower.iter().map(|day| day.0).sum();
    let weight: f64 = lower.iter().map(|day| day.1).sum();
    Some(points / weight)
}

/// The day the points-per-token ratio shifted most (by mean log ratio on each side), when it
/// moved by at least half and both sides have three active days.
fn change_point(days: &[(String, f64)]) -> Option<Value> {
    if days.len() < 6 {
        return None;
    }
    let logs: Vec<f64> = days.iter().map(|(_, ratio)| ratio.ln()).collect();
    let mut best: Option<(usize, f64)> = None;
    for split in 3..=logs.len() - 3 {
        let shift = mean(&logs[split..]) - mean(&logs[..split]);
        if best.is_none_or(|(_, current)| shift.abs() > current.abs()) {
            best = Some((split, shift));
        }
    }
    let (split, shift) = best?;
    (shift.abs() >= 1.5_f64.ln()).then(|| json!({"date":days[split].0,"factor":shift.exp()}))
}

/// Whether a request counts toward a window. Claude's model-specific weekly windows count only
/// their model family; Cowork usage never appears in local logs, so nothing local explains it.
fn in_scope(window: &str, model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    if window.contains("opus") {
        model.contains("opus")
    } else if window.contains("sonnet") {
        model.contains("sonnet")
    } else {
        !window.contains("cowork")
    }
}

/// Analyses one account's allowance windows against its observed usage.
pub fn analyze(
    provider: Provider,
    labels: &BTreeMap<String, String>,
    readings: &[Reading],
    usage: &[Usage],
    server_days: &BTreeMap<String, i64>,
) -> Value {
    let mut sorted = usage.to_vec();
    sorted.sort_by_key(|row| row.at);
    let usage = &sorted[..];
    let mut by_window: BTreeMap<&str, Vec<&Reading>> = BTreeMap::new();
    for reading in readings {
        by_window.entry(&reading.window).or_default().push(reading);
    }
    let mut windows = Vec::new();
    for (id, mut rows) in by_window {
        rows.sort_by_key(|row| row.at);
        let scoped: Vec<Usage>;
        let usage = if id.contains("opus") || id.contains("sonnet") || id.contains("cowork") {
            scoped = usage
                .iter()
                .filter(|row| in_scope(id, &row.model))
                .cloned()
                .collect();
            &scoped[..]
        } else {
            usage
        };
        let (segments, idle) = segments(&rows, usage, provider);
        let mut days: BTreeMap<String, (f64, f64, f64, bool)> = BTreeMap::new();
        for segment in &segments {
            let day = days.entry(date(segment.end)).or_default();
            day.0 += segment.points;
            day.1 += segment.weighted;
            if let Some(cost) = segment.cost {
                day.2 += cost;
            } else if segment.weighted > 0.0 {
                day.3 = true;
            }
        }
        let pairs: Vec<(f64, f64)> = days.values().map(|day| (day.0, day.1)).collect();
        let rate = baseline(&pairs);
        // Points per dollar only when every active day's usage reported its cost.
        let cost_pairs: Vec<(f64, f64)> = days
            .values()
            .filter(|day| !day.3)
            .map(|day| (day.0, day.2 * 1e6))
            .collect();
        let cost_rate = (provider == Provider::Claude)
            .then(|| baseline(&cost_pairs))
            .flatten()
            .map(|rate| rate * 1e6);
        let ratios: Vec<(String, f64)> = days
            .iter()
            .filter(|(_, day)| day.1 >= MIN_WEIGHTED && day.0 > 0.0)
            .map(|(date, day)| (date.clone(), day.0 / day.1))
            .collect();
        let mut total = (0.0, 0.0, 0.0);
        let day_rows: Vec<Value> = days
            .iter()
            .map(|(date, day)| {
                let explained = rate.map_or(0.0, |rate| (rate * day.1).min(day.0));
                total.0 += day.0;
                total.1 += explained;
                total.2 += day.1;
                json!({"date":date,"points":day.0,"weighted":day.1.round(),
                    "explained":explained,"unattributed":day.0 - explained})
            })
            .collect();
        let idle_points: f64 = segments
            .iter()
            .filter(|segment| segment.weighted == 0.0)
            .map(|segment| segment.points)
            .sum();
        windows.push(json!({
            "id":id,"label":labels.get(id).cloned().unwrap_or_else(|| id.to_owned()),
            "enough":rate.is_some(),"activeDays":ratios.len(),
            "pointsPerMillion":rate.map(|rate| rate * 1e6),"pointsPerDollar":cost_rate,
            "points":total.0,"explained":total.1,"unattributed":total.0 - total.1,
            "idlePoints":idle_points,"idleSteps":idle.iter().rev().take(500).collect::<Vec<_>>(),
            "change":change_point(&ratios),"days":day_rows,
        }));
    }
    let mut groups: BTreeMap<(String, String, String), [f64; 6]> = BTreeMap::new();
    for row in usage {
        let group = groups
            .entry((
                row.surface.clone(),
                row.query_source.clone(),
                row.model.clone(),
            ))
            .or_default();
        group[0] += 1.0;
        group[1] += row.input as f64;
        group[2] += row.output as f64;
        group[3] += row.cache_read as f64;
        group[4] += row.cache_write as f64;
        group[5] += row.cost_micros.unwrap_or(0) as f64 / 1e6;
    }
    let read: f64 = usage.iter().map(|row| row.cache_read as f64).sum();
    let prompt: f64 = usage
        .iter()
        .map(|row| (row.input + row.cache_read + row.cache_write) as f64)
        .sum();
    let mut local_days: BTreeMap<String, i64> = BTreeMap::new();
    for row in usage {
        *local_days.entry(date(row.at)).or_default() +=
            row.input + row.output + row.cache_read + row.cache_write;
    }
    let server: Vec<Value> = server_days
        .iter()
        .filter(|(day, _)| local_days.keys().next().is_some_and(|first| *day >= first))
        .map(|(day, tokens)| json!({"date":day,"server":tokens,"local":local_days.get(day).copied().unwrap_or(0)}))
        .collect();
    json!({
        "windows":windows,
        "surfaces":groups.into_iter().map(|((surface,source,model),g)| json!({"surface":surface,"querySource":source,"model":model,
            "requests":g[0],"input":g[1],"output":g[2],"cacheRead":g[3],"cacheWrite":g[4],"costUsd":g[5]})).collect::<Vec<_>>(),
        "cacheReadShare":(prompt > 0.0).then(|| read / prompt),
        "serverVsLocal":server,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(window: &str, at: i64, used: f64) -> Reading {
        Reading {
            window: window.into(),
            at,
            used,
            resets_at: Some(1_000_000_000),
        }
    }

    fn input(at: i64, tokens: i64, cost: i64) -> Usage {
        Usage {
            at,
            surface: "cli".into(),
            model: "m".into(),
            query_source: "main".into(),
            input: tokens,
            cost_micros: Some(cost),
            ..Default::default()
        }
    }

    #[test]
    fn hidden_usage_is_unattributed_and_does_not_inflate_the_baseline() {
        // Four days at 1 point per 100k tokens; day five also burns 3 points with no local usage.
        let mut readings = Vec::new();
        let mut usage = Vec::new();
        let mut used = 0.0;
        for day in 0..5 {
            let base = day * DAY;
            readings.push(reading("seven_day", base, used));
            for step in 1..=2 {
                usage.push(input(base + step * 600 - 1, 100_000, 1_000_000));
                used += 1.0;
                readings.push(reading("seven_day", base + step * 600, used));
            }
            if day == 4 {
                used += 3.0;
                readings.push(reading("seven_day", base + 5000, used));
            }
        }
        let labels = BTreeMap::from([("seven_day".to_owned(), "7-day".to_owned())]);
        let result = analyze(
            Provider::Claude,
            &labels,
            &readings,
            &usage,
            &BTreeMap::new(),
        );
        let window = &result["windows"][0];
        assert_eq!(window["label"], "7-day");
        assert_eq!(window["enough"], true);
        assert!((window["pointsPerMillion"].as_f64().unwrap() - 10.0).abs() < 1e-9);
        assert!((window["pointsPerDollar"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(window["points"], 13.0);
        assert!((window["unattributed"].as_f64().unwrap() - 3.0).abs() < 1e-9);
        assert_eq!(window["idlePoints"], 3.0);
        assert_eq!(window["idleSteps"], json!([4 * DAY + 5000]));
        assert!(window["change"].is_null());
        assert_eq!(result["surfaces"][0]["requests"], 10.0);
    }

    #[test]
    fn model_windows_count_only_their_model_family() {
        let readings = vec![
            reading("seven_day_opus", 0, 0.0),
            reading("seven_day_opus", 600, 1.0),
            reading("seven_day", 0, 0.0),
            reading("seven_day", 600, 1.0),
        ];
        let usage = vec![Usage {
            model: "claude-sonnet-5-5".into(),
            ..input(300, 100_000, 0)
        }];
        let result = analyze(
            Provider::Claude,
            &BTreeMap::new(),
            &readings,
            &usage,
            &BTreeMap::new(),
        );
        let windows = result["windows"].as_array().unwrap();
        assert_eq!(windows[0]["id"], "seven_day");
        assert_eq!(windows[0]["idlePoints"], 0.0);
        assert_eq!(windows[1]["id"], "seven_day_opus");
        assert_eq!(windows[1]["idlePoints"], 1.0);
        assert!(!in_scope("seven_day_cowork", "claude-opus-5-5"));
    }

    #[test]
    fn resets_split_windows_and_a_sustained_rate_shift_is_dated() {
        let mut readings = Vec::new();
        let mut usage = Vec::new();
        for day in 0..8_i64 {
            let base = day * DAY;
            let reset = Some(base + DAY);
            // From day four, each point needs only half the tokens.
            let tokens = if day >= 4 { 50_000 } else { 100_000 };
            for (step, used) in [(0, 0.0), (1, 1.0), (2, 2.0)] {
                readings.push(Reading {
                    window: "five_hour".into(),
                    at: base + step * 600,
                    used,
                    resets_at: reset,
                });
                if step > 0 {
                    usage.push(input(base + step * 600 - 1, tokens, 0));
                }
            }
        }
        let result = analyze(
            Provider::Codex,
            &BTreeMap::new(),
            &readings,
            &usage,
            &BTreeMap::from([("1970-01-01".to_owned(), 900_000)]),
        );
        let window = &result["windows"][0];
        assert_eq!(window["points"], 16.0);
        assert!(window["pointsPerDollar"].is_null());
        assert_eq!(window["change"]["date"], "1970-01-05");
        assert!((window["change"]["factor"].as_f64().unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(
            result["serverVsLocal"][0],
            json!({"date":"1970-01-01","server":900_000,"local":200_000})
        );
    }
}
