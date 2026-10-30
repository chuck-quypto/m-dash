use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

pub async fn read_usage_response<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Value, String> {
    loop {
        let mut bytes = Vec::new();
        let size = (&mut *reader)
            .take(2 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|_| "Could not read Claude account usage.")?;
        if size == 0 || size > 2 * 1024 * 1024 {
            return Err("Claude returned no bounded account usage report.".into());
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Claude returned an invalid usage report.")?;
        if value["type"] != "control_response" || value["response"]["request_id"] != "m-dash-usage"
        {
            continue;
        }
        let response = &value["response"];
        if response["subtype"] != "success" || response["response"]["rate_limits_available"] != true
        {
            return Err("Claude could not read subscription allowances. Check claude auth status and retry later.".into());
        }
        return Ok(response["response"].clone());
    }
}

pub fn read_usage_cache() -> Result<Value, String> {
    let path = if let Some(directory) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        std::path::PathBuf::from(directory).join(".claude.json")
    } else {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(std::path::PathBuf::from)
            .ok_or("Cannot find your Claude configuration.")?
            .join(".claude.json")
    };
    let file = std::fs::File::open(path).map_err(|_| {
        "Claude has no saved account usage. Sign in with claude login, then refresh."
    })?;
    read_usage_config(std::io::BufReader::new(file), crate::collector::now())
}

fn read_usage_config(reader: impl std::io::Read, now: u64) -> Result<Value, String> {
    // Deserialize only accounting metadata; subscription credential files are never opened.
    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Identity {
        account_uuid: String,
        organization_uuid: String,
    }
    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Accounting {
        oauth_account: Identity,
        cached_usage_utilization: Value,
    }
    let accounting: Accounting =
        serde_json::from_reader(reader.take(32 * 1024 * 1024)).map_err(|_| {
            "Claude's saved account usage is invalid, being updated, or exceeds 32 MiB."
        })?;
    normalize_usage_cache(
        serde_json::to_value(accounting).map_err(|_| "Cannot decode Claude account usage.")?,
        now,
    )
}

pub fn executable() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    let mut paths = crate::collector::cli_directories();
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let native = PathBuf::from(home).join(".local/bin");
        if cfg!(target_os = "macos") {
            // Legacy npm wrappers may need a Node runtime absent from Finder's PATH.
            paths.insert(0, native);
        } else {
            paths.push(native);
        }
    }
    paths.into_iter().map(|path| path.join(if cfg!(windows) { "claude.exe" } else { "claude" }))
        .find(|path| path.is_file())
        .ok_or_else(|| "Claude CLI was not found. Install the native CLI and sign in with your Claude subscription.".into())
}

pub async fn collect() -> Result<Value, String> {
    use std::{process::Stdio, time::Duration};
    use tokio::{
        io::{AsyncWriteExt, BufReader},
        process::Command,
        time::{timeout, Instant},
    };
    let started_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "The system clock is invalid.")?
        .as_millis();
    let mut command = Command::new(executable()?);
    command
        .args([
            "--safe-mode",
            "--no-session-persistence",
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|_| "Could not start Claude's account usage reader.")?;
    let result=timeout(Duration::from_secs(45),async {
        let mut input=child.stdin.take().ok_or("Cannot open Claude usage control input.")?;
        let output=child.stdout.take().ok_or("Cannot open Claude usage output.")?;
        input.write_all(b"{\"type\":\"control_request\",\"request_id\":\"m-dash-usage\",\"request\":{\"subtype\":\"get_usage\",\"skip_behaviors\":true}}\n").await
            .map_err(|_|"Could not request Claude account usage.")?;
        let response=read_usage_response(&mut BufReader::new(output)).await?;
        // Native success can contain a fallback; its timestamped cache determines freshness.
        let deadline=Instant::now()+Duration::from_secs(4);
        loop {
            let cache=read_usage_cache();
            if let Ok(mut snapshot)=cache.clone() {
                let now=crate::collector::now();
                if snapshot["quotaObservedAt"].as_u64().is_some_and(|at|now.saturating_sub(at)<=60) {
                    snapshot["plan"]=response["subscription_type"].clone();
                    snapshot["pingFresh"]=json!(snapshot["quotaFetchedAtMs"].as_u64().is_some_and(|at| u128::from(at)>=started_ms));
                    return Ok(snapshot);
                }
            }
            if Instant::now()>=deadline {return Err(cache.err().unwrap_or_else(||"Claude did not refresh its saved allowance. Showing the original observation; automatic retry waits at least five minutes.".into()))}
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.unwrap_or_else(|_|Err("Claude's usage read timed out. Saved allowances were retained.".into()));
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

pub fn normalize_usage_cache(value: Value, now: u64) -> Result<Value, String> {
    let cache = &value["cachedUsageUtilization"];
    let account = identifier(&value["oauthAccount"]["accountUuid"])?;
    let org = identifier(&value["oauthAccount"]["organizationUuid"])?;
    if cache["accountUuid"].as_str() != Some(account.as_str()) {
        return Err(
            "Claude's saved usage belongs to a different sign-in. Refresh Claude usage.".into(),
        );
    }
    let observed = cache["fetchedAtMs"]
        .as_u64()
        .map(|at| at / 1000)
        .filter(|at| *at > 0 && *at <= now)
        .ok_or("Claude's saved usage has no valid fetch time.")?;
    let mut windows = Vec::new();
    let mut five_hour_state = if cache["utilization"]
        .get("five_hour")
        .is_some_and(Value::is_null)
    {
        "idle"
    } else {
        "unknown"
    };
    let mut five_hour_reset = None;
    for (id, label, minutes) in [
        ("five_hour", "5-hour allowance", 300),
        ("seven_day", "7-day allowance", 10080),
        ("seven_day_opus", "7-day Opus allowance", 10080),
        ("seven_day_sonnet", "7-day Sonnet allowance", 10080),
        ("seven_day_cowork", "7-day Cowork allowance", 10080),
    ] {
        let Some(window) = cache["utilization"]
            .get(id)
            .filter(|window| !window.is_null())
        else {
            continue;
        };
        let used = window["utilization"]
            .as_f64()
            .filter(|used| used.is_finite() && (0.0..=100.0).contains(used))
            .ok_or("Claude supplied an invalid allowance percentage.")?;
        if window.get("resets_at").is_some_and(Value::is_null) && used == 0.0 {
            if id == "five_hour" {
                five_hour_state = "idle";
            }
            continue;
        }
        let reset = window["resets_at"]
            .as_str()
            .and_then(|text| {
                time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .and_then(|at| u64::try_from(at.unix_timestamp()).ok())
            .ok_or("Claude supplied an invalid allowance reset time.")?;
        if id == "five_hour" {
            five_hour_state = if reset > now { "active" } else { "expired" };
            five_hour_reset = Some(reset);
        }
        if reset > now {
            windows.push(
                json!({"id":id,"label":label,"usedPercent":used,"remainingPercent":100.0-used,
                "durationMins":minutes,"resetsAt":reset}),
            );
        }
    }
    if cache["utilization"].get("five_hour").is_none()
        && cache["utilization"].get("seven_day").is_none()
    {
        return Err("Claude did not supply account allowances.".into());
    }
    Ok(
        json!({"provider":"claude","accountId":format!("claude:{}",json!([account,org])),
        "observedAt":observed,"quotaObservedAt":observed,"quotaFetchedAtMs":cache["fetchedAtMs"],
        "fiveHourState":five_hour_state,"fiveHourReset":five_hour_reset,
        "windows":windows,"source":"Claude Code account usage"}),
    )
}

#[derive(Debug)]
pub struct StatusUpdate {
    pub session: String,
    pub api_duration_ms: i64,
    pub windows: Value,
}

pub fn normalize_status(value: Value, now: u64) -> Result<StatusUpdate, String> {
    let session = identifier(&value["session_id"])?;
    let api_duration_ms = value["cost"]["total_api_duration_ms"]
        .as_i64()
        .filter(|value| *value >= 0)
        .ok_or("Claude did not supply the session API duration.")?;
    let mut windows = Vec::new();
    for (id, label, minutes) in [
        ("five_hour", "5-hour allowance", Some(300)),
        ("seven_day", "7-day allowance", Some(10080)),
        ("spend_limit", "Gateway spending allowance", None),
    ] {
        let Some(window) = value["rate_limits"].get(id).filter(|row| !row.is_null()) else {
            continue;
        };
        let used = window["used_percentage"]
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or("Claude supplied an invalid quota percentage.")?;
        let reset = window["resets_at"]
            .as_u64()
            .ok_or("Claude supplied an invalid reset time.")?;
        if reset > now {
            windows.push(json!({"id":id,"label":label,"usedPercent":used,"remainingPercent":(100.0-used).max(0.0),
                "durationMins":minutes,"resetsAt":reset}));
        }
    }
    Ok(StatusUpdate {
        session,
        api_duration_ms,
        windows: json!(windows),
    })
}

fn identifier(value: &Value) -> Result<String, String> {
    value
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .map(str::to_owned)
        .ok_or_else(|| "Claude supplied no usable session or account identity.".into())
}

pub struct TelemetryEvent {
    pub account: String,
    pub session: String,
    pub request: Option<String>,
    pub occurred_at: i64,
    pub tokens: Option<i64>,
    /// Accounting detail of an API request; never prompt or tool content.
    pub detail: Option<crate::usage::Request>,
    /// An API error reporting HTTP 429, which marks a reached allowance.
    pub rate_limited: bool,
}

pub fn normalize_telemetry(value: Value, now: u64) -> Result<Vec<TelemetryEvent>, String> {
    let resources = value["resourceLogs"]
        .as_array()
        .ok_or("Claude telemetry is not OTLP JSON.")?;
    let mut events = Vec::new();
    for resource in resources {
        let service = attribute(&resource["resource"]["attributes"], "service.name");
        if !matches!(
            service.as_str(),
            Some("claude-code" | "claude-code-desktop")
        ) {
            continue;
        }
        for scope in resource["scopeLogs"].as_array().into_iter().flatten() {
            for record in scope["logRecords"].as_array().into_iter().flatten() {
                let attrs = &record["attributes"];
                let (Ok(account), Ok(org), Ok(session)) = (
                    identifier(&attribute(attrs, "user.account_uuid")),
                    identifier(&attribute(attrs, "organization.id")),
                    identifier(&attribute(attrs, "session.id")),
                ) else {
                    continue;
                };
                let named = |name: &str| {
                    attribute(attrs, "event.name").as_str() == Some(name)
                        || record["body"]["stringValue"].as_str()
                            == Some(format!("claude_code.{name}").as_str())
                };
                let is_request = named("api_request");
                let is_limit =
                    named("api_error") && count(&attribute(attrs, "status_code")) == Some(429);
                let timestamp = || -> Result<i64, String> {
                    let stamp = attribute(attrs, "event.timestamp");
                    let stamp = stamp
                        .as_str()
                        .ok_or("Claude token event has no timestamp.")?;
                    let at = time::OffsetDateTime::parse(
                        stamp,
                        &time::format_description::well_known::Rfc3339,
                    )
                    .map_err(|_| "Claude token event has an invalid timestamp.")?
                    .unix_timestamp();
                    if at < 0 || at > now as i64 + 300 {
                        return Err("Claude token event has an out-of-range timestamp.".into());
                    }
                    Ok(at)
                };
                let parsed = (|| -> Result<_, String> {
                    Ok(if is_request {
                        let request = identifier(&attribute(attrs, "request_id"))?;
                        let at = timestamp()?;
                        let mut amounts = [0_i64; 4];
                        for (slot, name) in amounts.iter_mut().zip([
                            "input_tokens",
                            "output_tokens",
                            "cache_read_tokens",
                            "cache_creation_tokens",
                        ]) {
                            *slot = count(&attribute(attrs, name))
                                .ok_or("Claude token event has missing or invalid token counts.")?;
                        }
                        let total = amounts
                            .iter()
                            .try_fold(0_i64, |sum, amount| sum.checked_add(*amount))
                            .ok_or("Claude token count is too large.")?;
                        let [input, output, cache_read, cache_write] = amounts;
                        let label = |name: &str| {
                            attribute(attrs, name)
                                .as_str()
                                .filter(|value| !value.is_empty() && value.len() <= 128)
                                .map(str::to_owned)
                        };
                        let cost = count(&attribute(attrs, "cost_usd_micros")).or_else(|| {
                            let usd = attribute(attrs, "cost_usd");
                            usd.as_f64()
                                .or_else(|| usd.as_str().and_then(|text| text.parse().ok()))
                                .filter(|usd| usd.is_finite() && *usd >= 0.0 && *usd < 1e9)
                                .map(|usd| (usd * 1e6).round() as i64)
                        });
                        let resource = &resource["resource"]["attributes"];
                        let cloud = !attribute(attrs, "ccr.session.id").is_null()
                            || !attribute(resource, "ccr.session.id").is_null();
                        let surface = if cloud {
                            "cloud".to_owned()
                        } else if service.as_str() == Some("claude-code-desktop") {
                            "desktop".to_owned()
                        } else {
                            label("app.entrypoint").unwrap_or_else(|| "cli".into())
                        };
                        let detail = crate::usage::Request {
                            surface,
                            model: label("model"),
                            effort: label("effort"),
                            speed: label("speed"),
                            query_source: label("query_source"),
                            agent: label("agent_id"),
                            input,
                            output,
                            cache_read,
                            cache_write,
                            reasoning: None,
                            cost_micros: cost,
                        };
                        (Some(request), at, Some(total), Some(detail))
                    } else if is_limit {
                        (None, timestamp()?, None, None)
                    } else {
                        (None, now as i64, None, None)
                    })
                })();
                let Ok((request, occurred_at, tokens, detail)) = parsed else {
                    continue;
                };
                events.push(TelemetryEvent {
                    account: format!("claude:{}", json!([account, org])),
                    session,
                    request,
                    occurred_at,
                    tokens,
                    detail,
                    rate_limited: is_limit,
                });
            }
        }
    }
    Ok(events)
}

fn count(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .filter(|amount| *amount >= 0)
}

fn attribute(attrs: &Value, name: &str) -> Value {
    attrs
        .as_array()
        .into_iter()
        .flatten()
        .find(|attr| attr["key"].as_str() == Some(name))
        .and_then(|attr| attr["value"].as_object())
        .and_then(|value| {
            value
                .get("stringValue")
                .or_else(|| value.get("intValue"))
                .or_else(|| value.get("doubleValue"))
        })
        .cloned()
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_record_cannot_discard_valid_events_in_the_same_batch() {
        let good = json!({"body":{"stringValue":"claude_code.api_request"},"attributes":[
            {"key":"user.account_uuid","value":{"stringValue":"one"}}, {"key":"organization.id","value":{"stringValue":"org"}},
            {"key":"session.id","value":{"stringValue":"s"}},{"key":"request_id","value":{"stringValue":"r"}},
            {"key":"event.timestamp","value":{"stringValue":"1970-01-01T00:16:40Z"}},
            {"key":"input_tokens","value":{"intValue":"4"}},{"key":"output_tokens","value":{"intValue":"3"}},
            {"key":"cache_read_tokens","value":{"intValue":"0"}},{"key":"cache_creation_tokens","value":{"intValue":"0"}}]});
        let mut bad = good.clone();
        bad["attributes"][4]["value"]["stringValue"] = json!("bad timestamp");
        let events=normalize_telemetry(json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code"}}]},"scopeLogs":[{"logRecords":[bad,good]}]}]}),1000).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tokens, Some(7));
    }
    #[test]
    fn large_config_streams_only_accounting_fields() {
        let value = json!({"oauthAccount":{"accountUuid":"one","organizationUuid":"org"},
            "cachedUsageUtilization":{"accountUuid":"one","fetchedAtMs":1000000,"utilization":{"five_hour":null,"seven_day":null}},
            "projects":"x".repeat(5*1024*1024)});
        let snapshot = read_usage_config(value.to_string().as_bytes(), 1000).unwrap();
        assert!(snapshot.get("projects").is_none());
        assert_eq!(snapshot["accountId"], "claude:[\"one\",\"org\"]");
    }
    #[test]
    fn idle_optional_allowances_do_not_invalidate_the_account_read() {
        let mut value = json!({"oauthAccount":{"accountUuid":"one","organizationUuid":"org"},
            "cachedUsageUtilization":{"accountUuid":"one","fetchedAtMs":1000000,
            "utilization":{"five_hour":null,"seven_day":{"utilization":0,"resets_at":null},
            "seven_day_opus":{"utilization":0,"resets_at":null},"seven_day_sonnet":{"utilization":0,"resets_at":null}}}});
        assert_eq!(
            normalize_usage_cache(value.clone(), 1000).unwrap()["windows"],
            json!([])
        );
        value["cachedUsageUtilization"]["utilization"]["seven_day_opus"]["utilization"] = json!(1);
        assert!(normalize_usage_cache(value, 1000).is_err());
    }
    use serde_json::json;

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_discovery_prefers_the_native_install_and_keeps_path_fallback() {
        if let Some(expected) = std::env::var_os("MDASH_TEST_CLAUDE_EXECUTABLE") {
            assert_eq!(executable().unwrap(), std::path::PathBuf::from(expected));
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let native = directory.path().join(".local/bin/claude");
        let legacy = directory.path().join("legacy/claude");
        for path in [&native, &legacy] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "synthetic CLI; never executed").unwrap();
        }
        for expected in [&native, &legacy] {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "claude::tests::mac_discovery_prefers_the_native_install_and_keeps_path_fallback",
                ])
                .env("HOME", directory.path())
                .env("PATH", legacy.parent().unwrap())
                .env("MDASH_TEST_CLAUDE_EXECUTABLE", expected)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            if expected == &native {
                std::fs::remove_file(&native).unwrap();
            }
        }
    }

    #[test]
    fn native_usage_control_reply_ignores_other_events_and_rejects_unavailable_reports() {
        tauri::async_runtime::block_on(async {
            let input=b"{\"type\":\"system\",\"account\":\"private metadata\"}\n{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"m-dash-usage\",\"response\":{\"rate_limits_available\":true,\"subscription_type\":\"pro\"}}}\n";
            let result = read_usage_response(&mut tokio::io::BufReader::new(&input[..]))
                .await
                .unwrap();
            assert_eq!(result["subscription_type"], "pro");
            assert!(!result.to_string().contains("private metadata"));
            let error=b"{\"type\":\"control_response\",\"response\":{\"subtype\":\"error\",\"request_id\":\"m-dash-usage\",\"error\":\"private provider text\"}}\n";
            let result = read_usage_response(&mut tokio::io::BufReader::new(&error[..]))
                .await
                .unwrap_err();
            assert!(!result.contains("private provider text"));
        });
    }

    #[test]
    fn account_usage_preserves_fetch_time_and_matches_the_signed_in_identity() {
        let mut value = json!({"oauthAccount":{"accountUuid":"one","organizationUuid":"org"},
            "cachedUsageUtilization":{"accountUuid":"one","fetchedAtMs":1791899260882_u64,
                "utilization":{"five_hour":{"utilization":66,"resets_at":"2026-10-13T11:40:00.86778-04:00"},
                    "seven_day":{"utilization":97,"resets_at":"2026-10-16T03:00:00.8678-04:00"}}}});
        let snapshot = normalize_usage_cache(value.clone(), 1791899300).unwrap();
        assert_eq!(snapshot["accountId"], "claude:[\"one\",\"org\"]");
        assert_eq!(snapshot["quotaObservedAt"], 1791899260);
        assert_eq!(snapshot["windows"][0]["remainingPercent"], 34.0);
        assert_eq!(snapshot["windows"][0]["resetsAt"], 1791906000_u64);
        assert_eq!(snapshot["windows"][1]["remainingPercent"], 3.0);
        assert_eq!(
            normalize_usage_cache(value.clone(), 1791899600).unwrap()["quotaObservedAt"],
            1791899260
        );
        value["cachedUsageUtilization"]["accountUuid"] = json!("other");
        assert!(normalize_usage_cache(value, 1791899300).is_err());
    }

    #[test]
    fn usage_distinguishes_idle_active_expired_and_unknown_five_hour_windows() {
        let mut value = json!({"oauthAccount":{"accountUuid":"one","organizationUuid":"org"},
            "cachedUsageUtilization":{"accountUuid":"one","fetchedAtMs":1000000,
            "utilization":{"five_hour":null,"seven_day":{"utilization":10,"resets_at":"1970-01-02T00:00:00Z"}}}});
        assert_eq!(
            normalize_usage_cache(value.clone(), 1000).unwrap()["fiveHourState"],
            "idle"
        );
        value["cachedUsageUtilization"]["utilization"]["five_hour"] =
            json!({"utilization":0,"resets_at":null});
        assert_eq!(
            normalize_usage_cache(value.clone(), 1000).unwrap()["fiveHourState"],
            "idle"
        );
        value["cachedUsageUtilization"]["utilization"]["five_hour"] =
            json!({"utilization":20,"resets_at":"1970-01-01T00:20:00Z"});
        let active = normalize_usage_cache(value.clone(), 1000).unwrap();
        assert_eq!(active["fiveHourState"], "active");
        assert_eq!(active["fiveHourReset"], 1200);
        assert_eq!(
            normalize_usage_cache(value.clone(), 1300).unwrap()["fiveHourState"],
            "expired"
        );
        value["cachedUsageUtilization"]["utilization"]
            .as_object_mut()
            .unwrap()
            .remove("five_hour");
        assert_eq!(
            normalize_usage_cache(value.clone(), 1000).unwrap()["fiveHourState"],
            "unknown"
        );
        value["cachedUsageUtilization"]["utilization"]["five_hour"] =
            json!({"utilization":50,"resets_at":null});
        assert!(normalize_usage_cache(value, 1000).is_err());
    }

    #[test]
    fn statusline_keeps_live_quota_windows_and_does_not_count_context_tokens_as_usage() {
        let status = normalize_status(
            json!({"session_id":"session-one", "cost":{"total_api_duration_ms":1200},
            "context_window":{"total_input_tokens":99999},
            "rate_limits":{"five_hour":{"used_percentage":25,"resets_at":2000},
                "seven_day":{"used_percentage":80,"resets_at":900}}}),
            1000,
        )
        .unwrap();
        assert_eq!(status.session, "session-one");
        assert_eq!(status.api_duration_ms, 1200);
        assert_eq!(
            status.windows,
            json!([{"id":"five_hour","label":"5-hour allowance","usedPercent":25.0,
            "remainingPercent":75.0,"durationMins":300,"resetsAt":2000}])
        );
    }
}
