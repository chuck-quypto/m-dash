use crate::dashboard::Dashboard;
use crate::settings::CompactWindow;
use serde::Serialize;
use serde_json::Value;

pub fn show_strip(visible: bool, minimized: bool) -> bool {
    (cfg!(target_os = "macos") || visible) && minimized
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Allowance {
    pub label: &'static str,
    pub period: &'static str,
    pub remaining_percent: Option<f64>,
    pub resets_at: Option<u64>,
    pub observed_at: Option<u64>,
    pub status: &'static str,
}

pub fn allowances(dashboard: &Dashboard, now: u64) -> Vec<Allowance> {
    let codex = dashboard.snapshot.as_ref().unwrap_or(&Value::Null);
    let claude = dashboard.claude.as_ref().unwrap_or(&Value::Null);
    let grok = dashboard.grok.as_ref().unwrap_or(&Value::Null);
    let codex_week = window(codex, &["codex/primary", "codex/secondary"], 10080);
    let codex_short = window(codex, &["codex/primary", "codex/secondary"], 300);
    let claude_week = window(claude, &["seven_day"], 10080);
    let claude_short = window(claude, &["five_hour"], 300);
    let weekly_grok = grok["periodType"] == "USAGE_PERIOD_TYPE_WEEKLY";
    let grok_reset = grok["periodEnd"].as_str().and_then(|text| {
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
            .ok()
            .and_then(|at| u64::try_from(at.unix_timestamp()).ok())
    });
    let claude_failed = dashboard.claude_failed();
    let automatic = [
        CompactWindow::CodexFiveHour,
        CompactWindow::ClaudeFiveHour,
        CompactWindow::GrokAllowance,
        CompactWindow::CodexWeekly,
        CompactWindow::ClaudeWeekly,
    ];
    let selected = dashboard
        .settings
        .compact_windows
        .as_deref()
        .unwrap_or(&automatic);
    let mut rows = Vec::new();
    for selection in selected {
        if !dashboard.settings.providers.enabled(selection.provider()) {
            continue;
        }
        let (mut row, failed, blocked, present) = match selection {
            CompactWindow::CodexFiveHour => (
                row("Codex", "5h", codex_short, codex["observedAt"].as_u64()),
                dashboard.error.is_some(),
                codex["ordinaryUsageAllowed"] == false || codex_short["blocked"] == true,
                !codex_short.is_null() || codex_week.is_null(),
            ),
            CompactWindow::CodexWeekly => (
                row("Codex", "wk", codex_week, codex["observedAt"].as_u64()),
                dashboard.error.is_some(),
                codex["ordinaryUsageAllowed"] == false || codex_week["blocked"] == true,
                !codex_week.is_null(),
            ),
            CompactWindow::ClaudeFiveHour => (
                row(
                    "Claude",
                    "5h",
                    claude_short,
                    claude["quotaObservedAt"].as_u64(),
                ),
                claude_failed,
                false,
                !claude_short.is_null() || claude_week.is_null(),
            ),
            CompactWindow::ClaudeWeekly => (
                row(
                    "Claude",
                    "wk",
                    claude_week,
                    claude["quotaObservedAt"].as_u64(),
                ),
                claude_failed,
                false,
                !claude_week.is_null(),
            ),
            CompactWindow::GrokAllowance => (
                Allowance {
                    label: "Grok",
                    period: if weekly_grok {
                        "wk"
                    } else if grok["periodType"] == "USAGE_PERIOD_TYPE_MONTHLY" {
                        "mo"
                    } else {
                        ""
                    },
                    remaining_percent: percentage(&grok["remainingPercent"]),
                    resets_at: grok_reset,
                    observed_at: grok["observedAt"].as_u64(),
                    status: "unavailable",
                },
                dashboard.grok_error.is_some(),
                false,
                true,
            ),
        };
        if dashboard.settings.compact_windows.is_none() && !present {
            continue;
        }
        row.status = if row.remaining_percent.is_none() {
            "unavailable"
        } else if row.resets_at.is_some_and(|at| at <= now) {
            "resetPending"
        } else if failed
            || row
                .observed_at
                .is_none_or(|at| at == 0 || at > now || now.saturating_sub(at) > 360)
        {
            "stale"
        } else {
            "fresh"
        };
        if row.status == "fresh" && blocked {
            row.status = "restricted";
        }
        rows.push(row);
        if rows.len() == 4 {
            break;
        }
    }
    rows.sort_by_key(|row| (row.label, row.period != "5h"));
    rows
}

fn percentage(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
}

fn window<'a>(snapshot: &'a Value, ids: &[&str], minutes: u64) -> &'a Value {
    let mut matches = snapshot["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|window| {
            window["id"].as_str().is_some_and(|id| ids.contains(&id))
                && window["durationMins"] == minutes
        });
    let first = matches.next().unwrap_or(&Value::Null);
    if matches.next().is_some() {
        &Value::Null
    } else {
        first
    }
}

fn row(
    label: &'static str,
    period: &'static str,
    window: &Value,
    observed_at: Option<u64>,
) -> Allowance {
    let remaining_percent = percentage(&window["remainingPercent"]);
    Allowance {
        label,
        period,
        remaining_percent,
        observed_at,
        resets_at: window["resetsAt"].as_u64(),
        status: if remaining_percent.is_some() {
            "fresh"
        } else {
            "unavailable"
        },
    }
}

pub fn tray_image(rows: &[Allowance]) -> tauri::image::Image<'static> {
    let mut pixels = vec![0; 32 * 32 * 4];
    if rows.is_empty() {
        for x in [7, 15, 23] {
            for y in 14..18 {
                for dx in 0..3 {
                    let offset = (y * 32 + x + dx) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&[174, 184, 198, 255]);
                }
            }
        }
    }
    let columns = rows.len().clamp(1, 4);
    for (index, row) in rows.iter().take(4).enumerate() {
        let color = match (row.status, row.remaining_percent) {
            ("fresh", Some(0.)) | ("restricted", _) => [247, 132, 140, 255],
            ("fresh", Some(value)) if value < 20. => [245, 181, 68, 255],
            ("fresh", Some(_)) => [34, 211, 238, 255],
            _ => [174, 184, 198, 255],
        };
        let height = row
            .remaining_percent
            .map(|value| (value * 28. / 100.).round().clamp(2., 28.) as usize);
        for y in 2..30 {
            let filled = height.map_or((14..16).contains(&y), |height| y >= 30 - height);
            for x in (1 + index * 32 / columns)..((index + 1) * 32 / columns - 1) {
                let offset = (y * 32 + x) * 4;
                pixels[offset..offset + 4].copy_from_slice(if filled {
                    &color
                } else {
                    &[46, 56, 70, 255]
                });
            }
        }
    }
    tauri::image::Image::new_owned(pixels, 32, 32)
}

pub fn tray_tooltip(rows: &[Allowance]) -> String {
    if rows.is_empty() {
        return "m-dash · Open Settings to choose allowance windows".into();
    }
    let mut tooltip = String::from("m-dash · % left (* saved, ! blocked)");
    for row in rows {
        let value = row
            .remaining_percent
            .map_or("—".into(), |value| format!("{value:.1}%"));
        let marker = match row.status {
            "stale" | "resetPending" => "*",
            "restricted" => "!",
            _ => "",
        };
        tooltip.push_str(&format!("\n{} {}: {value}{marker}", row.label, row.period));
    }
    tooltip
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(target_os = "macos")]
    #[test]
    fn a_miniaturized_mac_window_shows_the_strip_even_when_appkit_marks_it_invisible() {
        assert!(show_strip(false, true));
        assert!(!show_strip(false, false), "a closed dashboard stays hidden");
        assert!(
            !show_strip(true, false),
            "a restored dashboard hides the strip"
        );
    }

    fn legacy_dashboard() -> Dashboard {
        Dashboard {
            settings: crate::settings::Settings::migrated(crate::settings::Providers {
                codex: true,
                claude: true,
                grok: true,
                openrouter: true,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_codex_only_plus_account_shows_reported_five_hour_and_weekly_windows() {
        use crate::settings::{Providers, Settings};
        let snapshot = crate::codex::finish_snapshot(
            json!({
                "accountId":"plus-fixture", "rateLimits":{
                    "primary":{"usedPercent":80,"windowDurationMins":300,"resetsAt":5000},
                    "secondary":{"usedPercent":35,"windowDurationMins":10080,"resetsAt":9000}
                }
            }),
            Err("Token history not supplied".into()),
            Some("plus"),
            1000,
        )
        .unwrap();
        let mut dashboard = Dashboard {
            snapshot: Some(snapshot),
            settings: Settings {
                providers: Providers {
                    codex: true,
                    ..Default::default()
                },
                compact_windows: None,
            },
            ..Default::default()
        };
        let rows = allowances(&dashboard, 1100);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].label, rows[0].period, rows[0].remaining_percent),
            ("Codex", "5h", Some(20.))
        );
        assert_eq!(
            (rows[1].period, rows[1].remaining_percent),
            ("wk", Some(65.))
        );
        dashboard.settings.providers.codex = false;
        assert!(
            allowances(&dashboard, 1100).is_empty(),
            "disabled accounts leave the compact display"
        );
        assert_eq!(
            dashboard.snapshot.as_ref().unwrap()["plan"],
            "plus",
            "hiding does not discard history"
        );
    }

    #[test]
    fn bar_order_is_provider_alphabetic_then_short_window_regardless_of_selection_order() {
        use crate::settings::CompactWindow::*;
        let mut dashboard = legacy_dashboard();
        for selection in [
            vec![CodexWeekly, ClaudeWeekly, CodexFiveHour, ClaudeFiveHour],
            vec![ClaudeFiveHour, CodexFiveHour, ClaudeWeekly, CodexWeekly],
            vec![CodexFiveHour, ClaudeFiveHour, CodexWeekly, ClaudeWeekly],
        ] {
            dashboard.settings.compact_windows = Some(selection.clone());
            let rows = allowances(&dashboard, 1000);
            assert_eq!(
                rows.iter()
                    .map(|row| (row.label, row.period))
                    .collect::<Vec<_>>(),
                [
                    ("Claude", "5h"),
                    ("Claude", "wk"),
                    ("Codex", "5h"),
                    ("Codex", "wk")
                ]
            );
            assert_eq!(
                dashboard.settings.compact_windows.as_ref(),
                Some(&selection)
            );
        }
    }

    #[test]
    fn custom_windows_hide_disabled_providers() {
        use crate::settings::CompactWindow::*;
        let mut dashboard = legacy_dashboard();
        dashboard.settings.compact_windows = Some(vec![ClaudeFiveHour, CodexFiveHour]);
        dashboard.settings.providers.claude = false;
        let rows = allowances(&dashboard, 1000);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].label, rows[0].period, rows[0].status),
            ("Codex", "5h", "unavailable")
        );
        dashboard.settings.providers.claude = true;
        assert_eq!(allowances(&dashboard, 1000)[0].label, "Claude");
        let empty = allowances(&Dashboard::default(), 1000);
        assert!(
            empty.is_empty(),
            "a fresh installation does not assume four accounts"
        );
        assert!(
            tray_image(&empty).rgba().iter().any(|byte| *byte != 0),
            "the tray remains visible with no bars"
        );
        assert!(tray_tooltip(&empty).contains("Settings"));
    }

    #[test]
    fn compact_allowances_select_the_four_account_windows_in_display_order() {
        let mut dashboard = Dashboard {
            snapshot: Some(json!({"observedAt":1000,"windows":[
                {"id":"other/secondary","durationMins":10080,"remainingPercent":99,"resetsAt":8000},
                {"id":"codex/primary","durationMins":300,"remainingPercent":90,"resetsAt":4000},
                {"id":"codex/secondary","durationMins":10080,"remainingPercent":42,"resetsAt":9000}
            ]})),
            claude: Some(
                json!({"source":"Claude Code account usage","quotaObservedAt":1000,"windows":[
                    {"id":"seven_day_opus","durationMins":10080,"remainingPercent":95,"resetsAt":8000},
                    {"id":"five_hour","durationMins":300,"remainingPercent":17,"resetsAt":3000},
                    {"id":"seven_day","durationMins":10080,"remainingPercent":68,"resetsAt":7000}
                ]}),
            ),
            grok: Some(
                json!({"observedAt":1000,"periodType":"USAGE_PERIOD_TYPE_WEEKLY", "remainingPercent":81,"periodEnd":"1970-01-01T02:00:00Z"}),
            ),
            ..legacy_dashboard()
        };
        let rows = allowances(&dashboard, 1100);
        assert_eq!(
            rows.iter()
                .map(|row| row.remaining_percent)
                .collect::<Vec<_>>(),
            [Some(17.), Some(68.), Some(42.), Some(81.)]
        );
        assert_eq!(
            rows.iter().map(|row| row.resets_at).collect::<Vec<_>>(),
            [Some(3000), Some(7000), Some(9000), Some(7200)]
        );
        assert!(rows.iter().all(|row| row.status == "fresh"));
        dashboard.settings.compact_windows = None;
        let automatic = allowances(&dashboard, 1100);
        assert_eq!(
            automatic
                .iter()
                .map(|row| (row.label, row.period))
                .collect::<Vec<_>>(),
            [
                ("Claude", "5h"),
                ("Codex", "5h"),
                ("Codex", "wk"),
                ("Grok", "wk")
            ],
            "automatic selection keeps its provider coverage and sorts the chosen four"
        );
    }

    #[test]
    fn saved_readings_expire_and_recover_without_inventing_allowance_at_reset() {
        let mut dashboard = Dashboard {
            snapshot: Some(json!({"observedAt":1000,"windows":[
                {"id":"codex/secondary","durationMins":10080,"remainingPercent":0,"resetsAt":2000}
            ]})),
            claude: Some(
                json!({"source":"Claude Code account usage","observedAt":1400,"quotaObservedAt":1000,"windows":[
                    {"id":"seven_day","durationMins":10080,"remainingPercent":68,"resetsAt":7000}
                ]}),
            ),
            ..legacy_dashboard()
        };
        let fresh = allowances(&dashboard, 1100);
        assert_eq!(
            (fresh[2].remaining_percent, fresh[2].status),
            (Some(0.), "fresh")
        );
        assert_eq!(
            (fresh[3].remaining_percent, fresh[3].status),
            (None, "unavailable")
        );
        let aged = allowances(&dashboard, 1361);
        assert_eq!(
            (aged[2].remaining_percent, aged[2].status),
            (Some(0.), "stale")
        );
        assert_eq!(
            aged[1].status, "stale",
            "new token events must not refresh old Claude quotas"
        );
        assert_eq!(allowances(&dashboard, 2000)[2].status, "resetPending");
        dashboard.apply_result(Err("offline".into()));
        assert_eq!(allowances(&dashboard, 1100)[2].status, "stale");
        dashboard.apply_result(Ok(json!({"observedAt":2010,"windows":[
            {"id":"codex/primary","durationMins":10080,"remainingPercent":100,"resetsAt":9000}
        ]})));
        let recovered = allowances(&dashboard, 2011);
        assert_eq!(
            (
                recovered[2].remaining_percent,
                recovered[2].resets_at,
                recovered[2].status
            ),
            (Some(100.), Some(9000), "fresh")
        );
    }

    #[test]
    fn restricted_pools_and_groks_actual_period_are_labeled_correctly() {
        let mut dashboard = Dashboard {
            snapshot: Some(
                json!({"observedAt":1000,"ordinaryUsageAllowed":false,"windows":[
                    {"id":"codex/secondary","durationMins":10080,"remainingPercent":42,"resetsAt":9000}
                ]}),
            ),
            claude: Some(
                json!({"source":"Claude Code account usage","quotaObservedAt":1000,"windows":[
                    {"id":"seven_day_opus","durationMins":10080,"remainingPercent":68,"resetsAt":7000}
                ]}),
            ),
            grok: Some(
                json!({"observedAt":1000,"periodType":"USAGE_PERIOD_TYPE_MONTHLY","remainingPercent":81,"periodEnd":"1970-01-01T02:00:00Z"}),
            ),
            ..legacy_dashboard()
        };
        let rows = allowances(&dashboard, 1100);
        assert_eq!(
            (rows[2].remaining_percent, rows[2].status),
            (Some(42.), "restricted")
        );
        assert_eq!(rows[1].remaining_percent, None);
        assert_eq!(
            (rows[3].period, rows[3].remaining_percent, rows[3].resets_at),
            ("mo", Some(81.), Some(7200))
        );
        dashboard.snapshot.as_mut().unwrap()["ordinaryUsageAllowed"] = json!(true);
        dashboard.snapshot.as_mut().unwrap()["windows"][0]["blocked"] = json!(true);
        assert_eq!(allowances(&dashboard, 1100)[2].status, "restricted");
    }

    #[test]
    fn tray_pixels_follow_each_allowance_and_distinguish_zero_unknown_and_stale() {
        let mut rows = allowances(&legacy_dashboard(), 1000);
        for (row, value) in rows.iter_mut().zip([100., 50., 25., 0.]) {
            row.remaining_percent = Some(value);
            row.status = "fresh";
        }
        let first = tray_image(&rows);
        let pixel = |image: &tauri::image::Image<'_>, x: usize, y: usize| {
            image.rgba()[(y * 32 + x) * 4..(y * 32 + x + 1) * 4].to_vec()
        };
        assert_eq!((first.width(), first.height()), (32, 32));
        for (x, height) in [(1, 28), (9, 14), (17, 7)] {
            assert_eq!(
                (0..32)
                    .filter(|y| pixel(&first, x, *y) == [34, 211, 238, 255])
                    .count(),
                height
            );
        }
        assert_eq!(
            pixel(&first, 25, 29),
            [247, 132, 140, 255],
            "zero has a red baseline"
        );
        rows[1].remaining_percent = Some(100.);
        rows[2].remaining_percent = None;
        rows[2].status = "unavailable";
        rows[3].remaining_percent = Some(75.);
        rows[3].status = "stale";
        let updated = tray_image(&rows);
        assert_eq!(
            pixel(&updated, 9, 2),
            [34, 211, 238, 255],
            "a new reading redraws the bar"
        );
        assert_eq!(
            pixel(&updated, 17, 14),
            [174, 184, 198, 255],
            "unknown has a grey middle dash"
        );
        assert_ne!(
            pixel(&updated, 17, 29),
            [247, 132, 140, 255],
            "unknown does not become zero"
        );
        assert_eq!(
            pixel(&updated, 25, 29),
            [174, 184, 198, 255],
            "saved values are grey"
        );
    }
}
