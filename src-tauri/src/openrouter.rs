use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub struct ReadError {
    pub message: String,
    pub retry_after: u64,
}
impl From<String> for ReadError {
    fn from(message: String) -> Self {
        Self {
            message,
            retry_after: 0,
        }
    }
}
impl From<&str> for ReadError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

pub async fn collect() -> Result<Value, ReadError> {
    let key = crate::credentials::read()?
        .ok_or("Connect an OpenRouter management key in Connections.")?;
    collect_with_key(&key).await
}

pub async fn collect_with_key(key: &str) -> Result<Value, ReadError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| ReadError::from("Cannot initialize the OpenRouter connection."))?;
    let credits = read_json(&client, "https://openrouter.ai/api/v1/credits", key).await?;
    let activity = read_json(&client, "https://openrouter.ai/api/v1/activity", key).await;
    let retry_after = activity.as_ref().err().map_or(0, |error| error.retry_after);
    // No stable account ID is supplied; a changed key starts separate history rather than mixing accounts.
    let account = format!("openrouter:{:x}", Sha256::digest(key.as_bytes()));
    let mut snapshot = normalize(
        credits,
        activity.map_err(|error| error.message),
        &account,
        crate::collector::now(),
    )?;
    snapshot["retryAfterSeconds"] = json!(retry_after);
    Ok(snapshot)
}

async fn read_json(client: &reqwest::Client, url: &str, key: &str) -> Result<Value, ReadError> {
    let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
        .map_err(|_| ReadError::from("The management key is invalid."))?;
    authorization.set_sensitive(true);
    let mut response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, authorization)
        .send()
        .await
        .map_err(|_| {
            ReadError::from("Could not reach OpenRouter. Check the connection and refresh.")
        })?;
    let status = response.status();
    if !status.is_success() {
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .parse::<u64>()
                    .ok()
                    .or_else(|| {
                        time::OffsetDateTime::parse(
                            value,
                            &time::format_description::well_known::Rfc2822,
                        )
                        .ok()
                        .map(|date| {
                            (date.unix_timestamp() - crate::collector::now() as i64).max(0) as u64
                        })
                    })
                    .unwrap_or(60)
            })
            .unwrap_or(60);
        let message = match status.as_u16() {
            401 | 403 => "OpenRouter requires a valid management key. Reconnect it in Connections.",
            429 => "OpenRouter requested a pause. Automatic refresh will wait before retrying.",
            _ => "OpenRouter could not supply this report. Last saved values were retained.",
        };
        return Err(ReadError {
            message: message.into(),
            retry_after: if status.as_u16() == 429 {
                retry_after
            } else {
                0
            },
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ReadError::from("OpenRouter's response was interrupted."))?
    {
        if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err("OpenRouter's report was too large; saved history was retained.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "OpenRouter supplied an invalid report.".into())
}

pub fn normalize(
    credits: Value,
    activity: Result<Value, String>,
    account: &str,
    now: u64,
) -> Result<Value, String> {
    let money = |field: &str| {
        credits["data"][field]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or("OpenRouter supplied invalid credit totals.")
    };
    let purchased = money("total_credits")?;
    let used = money("total_usage")?;
    let (days, error) = match activity.and_then(|value| daily_usage(value, now)) {
        Ok(days) => (days, None),
        Err(message) => (Vec::new(), Some(message)),
    };
    Ok(
        json!({"provider":"openrouter","accountId":account,"observedAt":now,
        "totalCreditsUsd":purchased,"totalUsageUsd":used,"balanceUsd":purchased-used,
        "dailyUsage":days,"usageError":error}),
    )
}

fn daily_usage(value: Value, now: u64) -> Result<Vec<Value>, String> {
    let today = time::OffsetDateTime::from_unix_timestamp(now as i64)
        .map_err(|_| "Invalid observation date.")?
        .date();
    let start = today - time::Duration::days(30);
    let day_format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]").unwrap();
    let mut days = std::collections::BTreeMap::new();
    for row in value["data"]
        .as_array()
        .ok_or("OpenRouter activity response is missing rows.")?
    {
        let date = row["date"]
            .as_str()
            .ok_or("OpenRouter activity is missing its date.")?;
        let date = date.strip_suffix(" 00:00:00").unwrap_or(date);
        let parsed = time::Date::parse(date, &day_format).map_err(|_| {
            "OpenRouter activity contains an invalid date; history was not replaced."
        })?;
        if parsed >= start && parsed < today {
            let total = days.entry(date.to_owned()).or_insert(0_i64);
            for field in ["prompt_tokens", "completion_tokens"] {
                let count = row[field]
                    .as_i64()
                    .filter(|n| *n >= 0)
                    .ok_or("OpenRouter activity contains invalid token counts.")?;
                *total = total
                    .checked_add(count)
                    .ok_or("OpenRouter token total is too large.")?;
            }
        } else if parsed > today {
            return Err(
                "OpenRouter activity contains an unexpected date; history was not replaced.".into(),
            );
        }
    }
    Ok(days
        .into_iter()
        .map(|(date, tokens)| json!({"date":date,"tokens":tokens}))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn omitted_activity_days_are_not_fabricated_as_zero() {
        let days = daily_usage(
            json!({"data":[{"date":"2026-10-11","prompt_tokens":3,"completion_tokens":2}]}),
            1791806400,
        )
        .unwrap();
        assert_eq!(
            days,
            json!([{"date":"2026-10-11","tokens":5}])
                .as_array()
                .unwrap()
                .clone()
        );
    }

    #[test]
    fn live_midnight_timestamp_dates_share_the_documented_utc_day_bucket() {
        let activity = json!({"data":[
            {"date":"2026-10-07 00:00:00","prompt_tokens":25,"completion_tokens":10},
            {"date":"2026-10-07","prompt_tokens":15,"completion_tokens":5},
            {"date":"2026-09-11 00:00:00","prompt_tokens":999,"completion_tokens":1},
            {"date":"2026-10-12 00:00:00","prompt_tokens":999,"completion_tokens":1}
        ]});
        let credits = json!({"data":{"total_credits":10.,"total_usage":1.}});
        let snapshot = normalize(credits.clone(), Ok(activity), "fixture", 1791806400).unwrap();
        assert!(
            snapshot["usageError"].is_null(),
            "{}",
            snapshot["usageError"]
        );
        assert_eq!(
            snapshot["dailyUsage"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["date"] == "2026-10-07")
                .unwrap()["tokens"],
            55
        );
        assert_eq!(snapshot["dailyUsage"].as_array().unwrap().len(), 1);
        assert_eq!(
            snapshot["dailyUsage"]
                .as_array()
                .unwrap()
                .iter()
                .map(|day| day["tokens"].as_i64().unwrap())
                .sum::<i64>(),
            55
        );
        for date in ["2026-10-07 01:00:00", "2026-09-31", "2026-10-13"] {
            let invalid = normalize(
                credits.clone(),
                Ok(json!({"data":[{"date":date,"prompt_tokens":25,"completion_tokens":10}]})),
                "fixture",
                1791806400,
            )
            .unwrap();
            assert!(!invalid["usageError"].is_null());
        }
    }

    #[tokio::test]
    async fn http_errors_do_not_echo_secrets_and_honor_rate_limit_delay() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/credits", server.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = server.accept().unwrap();
            let mut buffer = [0; 2048];
            let _ = stream.read(&mut buffer).unwrap();
            stream.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3600\r\nContent-Length: 27\r\nConnection: close\r\n\r\nprivate-provider-error-body").unwrap();
        });
        let result = read_json(&reqwest::Client::new(), &url, "nonsecret-test-key").await;
        task.join().unwrap();
        let error = result.err().unwrap();
        assert_eq!(error.retry_after, 3600);
        assert!(!error.message.contains("private"));
        assert!(!error.message.contains("nonsecret-test-key"));
    }

    #[test]
    fn balance_and_completed_days_do_not_double_count_token_subsets() {
        let value = normalize(json!({"data":{"total_credits":100.0,"total_usage":25.5}}), Ok(json!({"data":[
            {"date":"2026-10-11","prompt_tokens":100,"completion_tokens":30,"cached_tokens":80,"reasoning_tokens":20},
            {"date":"2026-10-11","prompt_tokens":70,"completion_tokens":10},
            {"date":"2026-10-12","prompt_tokens":999,"completion_tokens":1}
        ]})), "connection-one", 1791806400).unwrap();
        assert_eq!(value["balanceUsd"], 74.5);
        assert_eq!(value["dailyUsage"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["dailyUsage"].as_array().unwrap().last().unwrap(),
            &json!({"date":"2026-10-11","tokens":210})
        );
        assert!(value["usageError"].is_null());
    }

    #[test]
    fn activity_failure_keeps_fresh_balance_and_invalid_money_is_rejected() {
        let value = normalize(
            json!({"data":{"total_credits":0,"total_usage":0}}),
            Err("Activity unavailable".into()),
            "one",
            1791806400,
        )
        .unwrap();
        assert_eq!(value["balanceUsd"], 0.0);
        assert_eq!(value["usageError"], "Activity unavailable");
        assert!(value["dailyUsage"].as_array().unwrap().is_empty());
        assert!(normalize(
            json!({"data":{"total_credits":-1,"total_usage":0}}),
            Ok(json!({"data":[]})),
            "one",
            1791806400
        )
        .is_err());
    }
}
