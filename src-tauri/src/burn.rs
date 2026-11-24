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
use std::ops::RangeInclusive;

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
    Antigravity,
}

/// Relative price of each token type against fresh input, from the providers' published
/// price lists: Anthropic API prices for Claude, the Codex credit rate card for Codex
/// (which counts reasoning inside output), and Gemini API list ratios for Antigravity.
fn weighted(provider: Provider, usage: &Usage) -> f64 {
    let (output, read, write, model) = match provider {
        Provider::Claude => (5.0, 0.1, 1.25, claude_model_factor(&usage.model)),
        Provider::Codex => (6.0, 0.1, 1.0, 1.0),
        Provider::Antigravity => (5.0, 0.1, 1.0, antigravity_model_factor(&usage.model)),
    };
    model
        * (usage.input as f64
            + output * usage.output as f64
            + read * usage.cache_read as f64
            + write * usage.cache_write as f64)
}

fn antigravity_model_factor(model: &str) -> f64 {
    let m = model.to_ascii_lowercase();
    if m.contains("claude") {
        return claude_model_factor(&m);
    }
    if m.contains("flash") || m.contains("hydrogen") {
        return 0.25;
    }
    if m.contains("argon") {
        return 0.5;
    }
    1.0
}

/// The first two numbers after `family-` in a model ID, such as (4, 1) for
/// "claude-opus-4-1-20250805" or (5, 0) for "claude-sonnet-5". Date suffixes are not versions.
fn version(model: &str, family: &str) -> Option<(u32, u32)> {
    let rest = &model[model.find(family)? + family.len()..];
    let mut parts = rest
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u32>().ok().filter(|_| part.len() <= 2));
    let major = parts.next()??;
    Some((major, parts.next().flatten().unwrap_or(0)))
}

/// A Claude model's API input price relative to $5 per million tokens, the Opus 4.5 to Opus 5
/// price, so Opus usage keeps its familiar scale. Every listed model keeps the same output, cache
/// read and cache write ratios, so one factor per model is enough. Unknown models count as 1.
fn claude_model_factor(model: &str) -> f64 {
    let model = model.to_ascii_lowercase();
    if model.contains("fable") || model.contains("mythos") {
        return 2.0; // $10
    }
    if let Some(version) = version(&model, "opus-") {
        return match version {
            (4, 0..=1) | (0..=3, _) => 3.0, // $15
            (5, 5..) | (6.., _) => 0.8,     // $4
            _ => 1.0,                       // $5
        };
    }
    if let Some((major, _)) = version(&model, "sonnet-") {
        return if major >= 5 { 0.4 } else { 0.6 }; // $2 or $3
    }
    if let Some((major, _)) = version(&model, "haiku-") {
        return if major >= 5 { 0.02 } else { 0.2 }; // $0.10 or $1
    }
    1.0
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
    start: i64,
    end: i64,
    points: f64,
    weighted: f64,
    cost: Option<f64>,
}

fn starts_new_window(from: &Reading, reading: &Reading) -> bool {
    // A small drop can be mixed precision (a poll's 41 after a log's 41.3).
    reading.used < from.used - 1.0
        || matches!((from.resets_at, reading.resets_at), (Some(a), Some(b)) if (a - b).abs() > 600)
}

fn usage_between(usage: &[Usage], start: i64, end: i64) -> &[Usage] {
    &usage[usage.partition_point(|row| row.at <= start)..usage.partition_point(|row| row.at <= end)]
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
        if starts_new_window(from, reading) {
            start = Some(reading);
            continue;
        }
        if reading.used <= from.used {
            continue;
        }
        // `usage` is sorted by time, so each stretch is one contiguous slice.
        let window = usage_between(usage, from.at, reading.at);
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
            start: from.at,
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
    (shift.abs() >= 1.5_f64.ln())
        .then(|| json!({"date":days[split].0,"factor":shift.exp(),"daysAfter":days.len() - split}))
}

/// Whether a request counts toward a window. Claude's model-specific weekly windows count only
/// their model family; Cowork usage never appears in local logs, so nothing local explains it.
fn in_scope(window: &str, model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    if window.contains("opus") {
        model.contains("opus")
    } else if window.contains("sonnet") {
        model.contains("sonnet")
    } else if window.starts_with("gemini-") {
        model.contains("gemini")
    } else if window.starts_with("3p-") {
        !model.is_empty() && !model.contains("gemini")
    } else {
        !window.contains("cowork")
    }
}

fn away_source(row: &Usage) -> (&str, &str) {
    let purpose = match row.query_source.as_str() {
        "main" | "repl_main_thread" => "",
        purpose => purpose,
    };
    (&row.surface, purpose)
}

/// Reviews a chosen period using only readings inside it and rates fitted before it.
pub fn away_usage(
    provider: Provider,
    labels: &BTreeMap<String, String>,
    readings: &[Reading],
    usage: &[Usage],
    plan_changes: &[i64],
    span: RangeInclusive<i64>,
) -> Value {
    let mut usage = usage.to_vec();
    usage.sort_by_key(|row| row.at);
    let mut observed = BTreeMap::new();
    for row in usage.iter().filter(|row| span.contains(&row.at)) {
        let (requests, tokens) = observed.entry(away_source(row)).or_insert((0, 0));
        *requests += 1;
        *tokens += row.input + row.output + row.cache_read + row.cache_write;
    }
    let mut observed: Vec<_> = observed.into_iter().collect();
    observed.sort_by_key(|(_, (_, tokens))| std::cmp::Reverse(*tokens));

    let mut windows: BTreeMap<&str, Vec<&Reading>> =
        labels.keys().map(|id| (id.as_str(), Vec::new())).collect();
    for reading in readings.iter().filter(|row| row.at <= *span.end()) {
        windows.entry(&reading.window).or_default().push(reading);
    }
    let plan_at = |at| {
        plan_changes
            .iter()
            .copied()
            .filter(|mark| *mark <= at)
            .max()
    };
    let prior_plan = plan_at(*span.start());
    let mut summaries = Vec::new();
    for (id, mut readings) in windows {
        readings.sort_by_key(|row| row.at);
        let (earlier, selected) =
            readings.split_at(readings.partition_point(|row| row.at < *span.start()));
        if selected.is_empty() && !labels.contains_key(id) {
            continue;
        }
        let scoped: Vec<_> = usage
            .iter()
            .filter(|row| in_scope(id, &row.model))
            .cloned()
            .collect();
        let mut daily = BTreeMap::<i64, (f64, f64)>::new();
        let same_window = |from: &&Reading, to: &&Reading| {
            !starts_new_window(from, to)
                && from.resets_at.is_none_or(|reset| to.at < reset)
                && plan_at(from.at) == plan_at(to.at)
        };
        let prior =
            &earlier[earlier.partition_point(|row| prior_plan.is_some_and(|at| row.at < at))..];
        for chunk in prior.chunk_by(same_window) {
            for step in segments(chunk, &scoped, provider).0 {
                let day = daily.entry(step.end.div_euclid(DAY)).or_default();
                day.0 += step.points;
                day.1 += step.weighted;
            }
        }
        let rate = baseline(&daily.into_values().collect::<Vec<_>>());
        let mut points = 0.0;
        let mut first_reading = None;
        let mut last_reading = None;
        let mut by_source = BTreeMap::new();
        let mut covered_pairs = 0;
        for chunk in selected
            .chunk_by(same_window)
            .filter(|chunk| chunk.len() >= 2)
        {
            first_reading.get_or_insert(chunk[0].at);
            last_reading = chunk.last().map(|row| row.at);
            covered_pairs += chunk.len() - 1;
            for step in segments(chunk, &scoped, provider).0 {
                points += step.points;
                if let Some(rate) =
                    rate.filter(|_| plan_at(step.start) == prior_plan && step.weighted > 0.0)
                {
                    let points_per_weight = rate.min(step.points / step.weighted);
                    for row in usage_between(&scoped, step.start, step.end) {
                        *by_source.entry(away_source(row)).or_insert(0.0) +=
                            points_per_weight * weighted(provider, row);
                    }
                }
            }
        }
        let mut sources: Vec<_> = by_source
            .into_iter()
            .filter(|(_, value)| *value > 0.0)
            .collect();
        sources.sort_by(|a, b| b.1.total_cmp(&a.1));
        let attributed: f64 = sources.iter().map(|(_, points)| points).sum();
        let label = labels.get(id).cloned().unwrap_or_else(|| match id {
            "five_hour" => "5-hour allowance".into(),
            "seven_day" => "7-day allowance".into(),
            _ => id.replace('_', " "),
        });
        summaries.push(json!({
            "label":label, "points":first_reading.map(|_| points),
            "attributed":attributed, "unexplained":(points - attributed).max(0.0),
            "firstReading":first_reading, "lastReading":last_reading,
            "breaks":selected.len().saturating_sub(1) - covered_pairs, "hasRate":rate.is_some(),
            "sources": sources.into_iter().map(|((surface, purpose), points)| json!({
                "surface":surface, "purpose":purpose, "points":points
            })).collect::<Vec<_>>(),
        }));
    }
    json!({
        "windows":summaries,
        "observed":observed.into_iter().map(|((surface, purpose), (requests, tokens))| json!({
            "surface":surface, "purpose":purpose, "requests":requests, "tokens":tokens
        })).collect::<Vec<_>>(),
    })
}

/// The baseline rate for each day: from the days in its own plan period (between breaks) when
/// that period has enough active days, otherwise from the whole range. A plan change resizes the
/// allowance, so mixing both sides would explain too much on one and too little on the other.
fn period_rates(dates: &[&String], pairs: &[(f64, f64)], breaks: &[String]) -> Vec<Option<f64>> {
    let overall = baseline(pairs);
    let period = |date: &String| breaks.iter().filter(|day| *day <= date).count();
    let mut rates = BTreeMap::new();
    for index in dates.iter().map(|date| period(date)) {
        rates.entry(index).or_insert_with(|| {
            let own: Vec<(f64, f64)> = dates
                .iter()
                .zip(pairs)
                .filter(|(date, _)| period(date) == index)
                .map(|(_, pair)| *pair)
                .collect();
            baseline(&own).or(overall)
        });
    }
    dates.iter().map(|date| rates[&period(date)]).collect()
}

/// How sure the current rate is: the active days in the latest plan period and the spread of
/// their points-per-weight ratios over the efficient half the baseline is fitted on, from the
/// most efficient day to the median day. Few days or a wide spread mean a rough estimate.
fn spread(
    dates: &[&String],
    pairs: &[(f64, f64)],
    breaks: &[String],
) -> (usize, Option<(f64, f64)>) {
    let latest = breaks
        .iter()
        .filter(|day| dates.iter().any(|date| date >= day))
        .max();
    let mut ratios: Vec<f64> = dates
        .iter()
        .zip(pairs)
        .filter(|(date, _)| latest.is_none_or(|start| **date >= start))
        .filter(|(_, (points, weight))| *weight >= MIN_WEIGHTED && *points > 0.0)
        .map(|(_, (points, weight))| points / weight)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let range = (ratios.len() >= MIN_DAYS).then(|| (ratios[0], ratios[(ratios.len() - 1) / 2]));
    (ratios.len(), range)
}

/// Analyses one account's allowance windows against its observed usage. `breaks` are the times
/// the plan changed; each plan period gets its own baseline rate.
pub fn analyze(
    provider: Provider,
    labels: &BTreeMap<String, String>,
    readings: &[Reading],
    usage: &[Usage],
    server_days: &BTreeMap<String, i64>,
    breaks: &[i64],
) -> Value {
    let breaks: Vec<String> = breaks.iter().map(|at| date(*at)).collect();
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
        let dates: Vec<&String> = days.keys().collect();
        let pairs: Vec<(f64, f64)> = days.values().map(|day| (day.0, day.1)).collect();
        let rate = baseline(&pairs);
        let rates = period_rates(&dates, &pairs, &breaks);
        let (rate_days, rate_range) = spread(&dates, &pairs, &breaks);
        // Points per dollar only when every active day's usage reported its cost.
        let priced: Vec<(&String, (f64, f64))> = days
            .iter()
            .filter(|(_, day)| !day.3)
            .map(|(date, day)| (date, (day.0, day.2 * 1e6)))
            .collect();
        let cost_rate = (provider == Provider::Claude)
            .then(|| {
                let (dates, pairs): (Vec<&String>, Vec<(f64, f64)>) = priced.into_iter().unzip();
                period_rates(&dates, &pairs, &breaks)
                    .last()
                    .copied()
                    .flatten()
            })
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
            .zip(&rates)
            .map(|((date, day), rate)| {
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
            "pointsPerMillion":rate.and(rates.last().copied().flatten()).map(|rate| rate * 1e6),
            "rateDays":rate_days,"rateRange":rate_range.map(|(low, high)| [low * 1e6, high * 1e6]),"pointsPerDollar":cost_rate,
            "points":total.0,"explained":total.1,"unattributed":total.0 - total.1,
            "idlePoints":idle_points,"idleSteps":idle.iter().rev().take(500).collect::<Vec<_>>(),
            "change":change_point(&ratios),"days":day_rows,
        }));
    }
    let mut groups: BTreeMap<(String, String, String), [f64; 7]> = BTreeMap::new();
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
        if let Some(cost) = row.cost_micros {
            group[5] += cost as f64 / 1e6;
            group[6] += 1.0;
        }
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
            "requests":g[0],"input":g[1],"output":g[2],"cacheRead":g[3],"cacheWrite":g[4],"costUsd":g[5],"pricedRequests":g[6]})).collect::<Vec<_>>(),
        "cacheReadShare":(prompt > 0.0).then(|| read / prompt),
        "serverVsLocal":server,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn away_review_explains_part_of_an_allowance_step_with_local_and_cloud_usage() {
        let reading = |at, used| Reading {
            window: "seven_day".into(),
            at,
            used,
            resets_at: None,
        };
        let request = |at, surface: &str, input| Usage {
            at,
            surface: surface.into(),
            input,
            ..Default::default()
        };
        let start = 4 * DAY;
        let readings = [
            reading(DAY, 0.0),
            reading(DAY + 300, 2.0),
            reading(2 * DAY + 300, 4.0),
            reading(3 * DAY + 300, 6.0),
            reading(start - 60, 8.0),
            reading(start + 60, 10.0),
            reading(start + 360, 19.0),
        ];
        let usage = [
            request(DAY + 100, "cli", 100_000),
            request(2 * DAY + 100, "cli", 100_000),
            request(3 * DAY + 100, "cli", 100_000),
            request(start + 30, "cli", 500_000),
            request(start + 120, "cli", 100_000),
            request(start + 180, "cloud", 200_000),
        ];
        let result = away_usage(
            Provider::Claude,
            &BTreeMap::new(),
            &readings,
            &usage,
            &[],
            start..=start + 600,
        );
        let window = &result["windows"][0];
        assert_eq!(window["points"].as_f64(), Some(9.0));
        assert_eq!(window["unexplained"].as_f64(), Some(3.0));
        assert_eq!(window["firstReading"], start + 60);
        assert_eq!(window["lastReading"], start + 360);
        assert_eq!(window["sources"][0]["surface"], "cloud");
        assert_eq!(window["sources"][0]["points"].as_f64(), Some(4.0));
        assert_eq!(window["sources"][1]["surface"], "cli");
        assert_eq!(window["sources"][1]["points"].as_f64(), Some(2.0));
        assert_eq!(result["observed"][0]["surface"], "cli");
        assert_eq!(result["observed"][0]["tokens"], 600_000);
    }

    #[test]
    fn away_review_skips_reset_boundaries_and_keeps_usage_without_a_baseline() {
        let reading = |window: &str, at, used, reset| Reading {
            window: window.into(),
            at,
            used,
            resets_at: Some(reset),
        };
        let readings = [
            reading("five_hour", 100, 80.0, 500),
            reading("five_hour", 200, 87.0, 500),
            reading("five_hour", 510, 4.0, 18_510),
            reading("five_hour", 600, 7.0, 18_510),
            reading("seven_day", 500, 15.0, 604_800),
        ];
        let usage = [Usage {
            at: 550,
            surface: "other:cli@Studio Mac".into(),
            query_source: "subagent".into(),
            input: 100_000,
            ..Default::default()
        }];
        let result = away_usage(
            Provider::Claude,
            &BTreeMap::new(),
            &readings,
            &usage,
            &[],
            50..=650,
        );
        let first = &result["windows"][0];
        assert_eq!(first["points"].as_f64(), Some(10.0));
        assert_eq!(first["unexplained"].as_f64(), Some(10.0));
        assert_eq!(first["breaks"], 1);
        assert_eq!(first["hasRate"], false);
        assert_eq!(result["windows"][1]["points"], Value::Null);
        assert_eq!(result["observed"][0]["surface"], "other:cli@Studio Mac");
        assert_eq!(result["observed"][0]["purpose"], "subagent");
    }

    #[test]
    fn claude_tokens_are_scaled_by_model_price() {
        for (model, factor) in [
            ("claude-opus-5", 1.0),
            ("claude-opus-4-8[1m]", 1.0),
            ("claude-opus-5-5", 0.8),
            ("claude-opus-4-1-20250805", 3.0),
            ("claude-opus-4-20250514", 3.0),
            ("claude-sonnet-5-5", 0.4),
            ("claude-sonnet-4-6", 0.6),
            ("claude-haiku-4-5-20251001", 0.2),
            ("claude-haiku-5-5", 0.02),
            ("claude-fable-5-1", 2.0),
            ("", 1.0),
        ] {
            assert_eq!(claude_model_factor(model), factor, "{model}");
        }
        let usage = |model: &str| Usage {
            model: model.into(),
            input: 100,
            output: 10,
            ..Default::default()
        };
        assert_eq!(weighted(Provider::Claude, &usage("claude-opus-5")), 150.0);
        assert_eq!(weighted(Provider::Claude, &usage("claude-sonnet-5")), 60.0);
        assert_eq!(weighted(Provider::Codex, &usage("claude-sonnet-5")), 160.0);
    }

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
            &[],
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
        assert_eq!(result["surfaces"][0]["pricedRequests"], 10.0);
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
            &[],
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
            &[],
        );
        let window = &result["windows"][0];
        assert_eq!(window["points"], 16.0);
        assert!(window["pointsPerDollar"].is_null());
        assert_eq!(window["change"]["date"], "1970-01-05");
        assert!((window["change"]["factor"].as_f64().unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(window["change"]["daysAfter"], 4);
        assert_eq!(
            result["serverVsLocal"][0],
            json!({"date":"1970-01-01","server":900_000,"local":200_000})
        );
    }

    #[test]
    fn a_plan_change_gives_each_period_its_own_baseline() {
        let mut readings = Vec::new();
        let mut usage = Vec::new();
        for day in 0..8_i64 {
            let base = day * DAY;
            // A smaller plan from day four: each point needs only half the tokens.
            let tokens = if day >= 4 { 50_000 } else { 100_000 };
            for (step, used) in [(0, 0.0), (1, 1.0), (2, 2.0)] {
                readings.push(Reading {
                    window: "weekly".into(),
                    at: base + step * 600,
                    used,
                    resets_at: Some(base + DAY),
                });
                if step > 0 {
                    usage.push(input(base + step * 600 - 1, tokens, 0));
                }
            }
        }
        let analyze = |breaks: &[i64]| {
            analyze(
                Provider::Codex,
                &BTreeMap::new(),
                &readings,
                &usage,
                &BTreeMap::new(),
                breaks,
            )
        };
        let mixed = &analyze(&[])["windows"][0];
        assert!((mixed["unattributed"].as_f64().unwrap() - 4.0).abs() < 1e-9);
        let split = &analyze(&[4 * DAY + 3600])["windows"][0];
        assert!(split["unattributed"].as_f64().unwrap().abs() < 1e-9);
        assert!((split["pointsPerMillion"].as_f64().unwrap() - 20.0).abs() < 1e-9);
        assert_eq!(split["rateDays"], 4);
        assert_eq!(split["rateRange"], json!([20.0, 20.0]));
        assert_eq!(mixed["rateDays"], 8);
        assert_eq!(mixed["rateRange"], json!([10.0, 10.0]));
    }

    #[test]
    fn antigravity_windows_scope_gemini_and_third_party_models_separately() {
        assert!(in_scope("gemini-5h", "gemini-4-neon-high"));
        assert!(!in_scope("gemini-5h", "claude-opus-5-5-high"));
        assert!(in_scope("3p-weekly", "claude-opus-5-5-high"));
        assert!(in_scope("3p-5h", "gpt-oss-120b-medium"));
        assert!(!in_scope("3p-weekly", "gemini-4-hydrogen-high"));
        assert!(!in_scope("3p-weekly", ""));
    }
}
