//! Collects Antigravity / Google AI quota snapshots via the signed-in `agy` CLI without
//! sending model prompts, extracting credentials, or modifying the user's `settings.json`.
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    process::Command,
    time::{timeout, Instant},
};

/// Maximum statusLine payload accepted from stdin (1 MiB).
const MAX_PAYLOAD: usize = 1024 * 1024;

pub fn executable() -> Result<PathBuf, String> {
    let mut paths = crate::collector::cli_directories();
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        paths.insert(0, PathBuf::from(local).join("agy/bin"));
    }
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let home = PathBuf::from(home);
        paths.push(home.join(".local/bin"));
        paths.push(home.join(".agy/bin"));
    }
    paths
        .into_iter()
        .map(|dir| dir.join(if cfg!(windows) { "agy.exe" } else { "agy" }))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "Antigravity CLI (agy) was not found. Install agy, sign in with Google, and restart m-dash."
                .into()
        })
}

/// Filters an `agy` statusLine payload down to strictly accounting fields so prompts, titles,
/// workspace paths and transcript locations never leave the capture helper.
pub fn filter_capture(payload: &Value) -> Result<Value, String> {
    let email = payload["email"]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or("Antigravity did not identify the signed-in account.")?;
    let quota = payload
        .get("quota")
        .filter(|value| value.is_object())
        .ok_or("Antigravity did not supply quota status.")?;
    Ok(json!({
        "email": email,
        "plan_tier": payload.get("plan_tier").and_then(Value::as_str),
        "version": payload.get("version").and_then(Value::as_str),
        "quota": quota,
    }))
}

/// Entrypoint for `m-dash --antigravity-capture <output-path>`, invoked transiently by
/// `agy --settings <temp-file>` during an on-demand quota refresh.
pub fn run_capture() {
    let _ = (|| -> Result<(), String> {
        let destination = std::env::args_os()
            .nth(2)
            .ok_or("Missing capture output path")?;
        let destination = PathBuf::from(destination);
        let mut input = Vec::new();
        std::io::stdin()
            .take(MAX_PAYLOAD as u64 + 1)
            .read_to_end(&mut input)
            .map_err(|_| "Cannot read Antigravity statusLine")?;
        if input.len() > MAX_PAYLOAD {
            return Err("Antigravity statusLine exceeded 1 MiB".into());
        }
        let payload: Value =
            serde_json::from_slice(&input).map_err(|_| "Invalid Antigravity statusLine JSON")?;
        let filtered = filter_capture(&payload)?;
        let bytes = serde_json::to_vec(&filtered).map_err(|_| "Cannot encode capture")?;
        let temp = destination.with_extension("tmp");
        let mut file = std::fs::File::create(&temp).map_err(|_| "Cannot write temp capture")?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| "Cannot flush temp capture")?;
        drop(file);
        std::fs::rename(&temp, &destination).map_err(|_| "Cannot commit capture")?;
        Ok(())
    })();
}

fn parse_reset(bucket: &Value, now: u64) -> Option<u64> {
    bucket["reset_time"]
        .as_str()
        .and_then(|text| {
            time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()
        })
        .and_then(|dt| u64::try_from(dt.unix_timestamp()).ok())
        .or_else(|| {
            bucket["reset_in_seconds"]
                .as_u64()
                .map(|seconds| now.saturating_add(seconds))
        })
}

fn bucket_meta(id: &str) -> (String, Option<u64>, usize) {
    match id {
        "gemini-5h" => ("Gemini".into(), Some(300), 0),
        "gemini-weekly" => ("Gemini".into(), Some(10080), 1),
        "3p-5h" => ("Claude & GPT".into(), Some(300), 2),
        "3p-weekly" => ("Claude & GPT".into(), Some(10080), 3),
        other => {
            let duration = if other.ends_with("-5h") || other.contains("five-hour") {
                Some(300)
            } else if other.ends_with("-weekly") || other.ends_with("-7d") {
                Some(10080)
            } else {
                None
            };
            let label = other
                .trim_end_matches("-5h")
                .trim_end_matches("-weekly")
                .trim_end_matches("-7d")
                .replace(['-', '_'], " ");
            let label = if label.is_empty() {
                other.to_owned()
            } else {
                let mut chars = label.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => other.to_owned(),
                }
            };
            (label, duration, 10)
        }
    }
}

/// Normalizes a filtered `agy` statusLine payload into m-dash's standard provider snapshot.
pub fn normalize(payload: Value, now: u64) -> Result<Value, String> {
    let email = payload["email"]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or("Antigravity did not identify the signed-in account.")?;
    let quota = payload["quota"]
        .as_object()
        .ok_or("Antigravity did not supply quota windows.")?;

    let mut windows_ranked: Vec<(usize, String, Value)> = Vec::new();
    let mut credits: Vec<Value> = Vec::new();

    for (id, bucket) in quota {
        if id.is_empty() || id.len() > 128 || !bucket.is_object() {
            continue;
        }
        let (label, duration_mins, rank) = bucket_meta(id);
        let disabled = bucket["disabled"].as_bool();
        if let Some(fraction) = bucket["remaining_fraction"].as_f64() {
            if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
                return Err(format!(
                    "Antigravity returned an invalid quota fraction for {id}."
                ));
            }
            let remaining = (fraction * 100.0).clamp(0.0, 100.0);
            let used = ((1.0 - fraction) * 100.0).clamp(0.0, 100.0);
            windows_ranked.push((
                rank,
                id.clone(),
                json!({
                    "id": id,
                    "label": label,
                    "remainingPercent": remaining,
                    "usedPercent": used,
                    "durationMins": duration_mins,
                    "resetsAt": parse_reset(bucket, now),
                    "blocked": disabled,
                }),
            ));
        } else if let Some(amount) = bucket["remaining_amount"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
        {
            credits.push(json!({
                "id": id,
                "label": label,
                "balance": format!("{amount}"),
                "unlimited": false,
            }));
        }
    }

    windows_ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let windows: Vec<Value> = windows_ranked.into_iter().map(|(_, _, w)| w).collect();
    credits.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));

    Ok(json!({
        "provider": "antigravity",
        "accountId": format!("antigravity:{email}"),
        "observedAt": now,
        "plan": payload["plan_tier"].as_str().filter(|s| !s.is_empty()),
        "windows": windows,
        "credits": credits,
        "dailyUsage": [],
        "observedTokens": Value::Null,
        "observedSince": Value::Null,
        "lastTokenAt": Value::Null,
        "source": "Antigravity CLI",
    }))
}

fn checked_path_text(path: &Path) -> Result<&str, String> {
    let text = path
        .to_str()
        .ok_or("Antigravity helper path is not valid UTF-8.")?;
    if text.chars().any(|c| c == '\r' || c == '\n') {
        return Err("Antigravity helper path contains unsupported characters.".into());
    }
    Ok(text)
}

#[cfg(windows)]
fn quote_ps(path: &Path) -> Result<String, String> {
    let text = checked_path_text(path)?;
    Ok(format!("'{}'", text.replace('\'', "''")))
}

#[cfg(windows)]
fn encode_ps(script: &str) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(not(windows))]
fn quote_posix(path: &Path) -> Result<String, String> {
    let text = checked_path_text(path)?;
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

fn statusline_command(helper: &Path, output_path: &Path) -> Result<String, String> {
    #[cfg(windows)]
    {
        let script = format!(
            "[Console]::In.ReadToEnd() | & {} --antigravity-capture {}",
            quote_ps(helper)?,
            quote_ps(output_path)?
        );
        Ok(format!(
            "powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}",
            encode_ps(&script)
        ))
    }
    #[cfg(not(windows))]
    {
        Ok(format!(
            "{} --antigravity-capture {}",
            quote_posix(helper)?,
            quote_posix(output_path)?
        ))
    }
}

fn spawn_agy(
    agy: &Path,
    settings_path: &Path,
    output_path: &Path,
) -> Result<tokio::process::Child, String> {
    #[cfg(windows)]
    {
        // `agy` only renders its interactive statusLine when attached to a console TTY.
        // Launch it in a hidden console via PowerShell and tear it down as soon as the
        // capture file is committed, so no window flashes and no `agy` process is orphaned.
        let script = format!(
            "$p = Start-Process -FilePath {agy} -ArgumentList @('--settings', {cfg}) -WindowStyle Hidden -PassThru; \
             $sw = [System.Diagnostics.Stopwatch]::StartNew(); \
             try {{ while ($sw.Elapsed.TotalSeconds -lt 30 -and -not (Test-Path -LiteralPath {out}) -and -not $p.HasExited) {{ Start-Sleep -Milliseconds 100 }} }} \
             finally {{ if ($p -and -not $p.HasExited) {{ try {{ $p.Kill(); $p.WaitForExit(2000) | Out-Null }} catch {{}} }} }}",
            agy = quote_ps(agy)?,
            cfg = quote_ps(settings_path)?,
            out = quote_ps(output_path)?,
        );
        let mut cmd = Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-EncodedCommand",
        ])
        .arg(encode_ps(&script))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .kill_on_drop(true);
        cmd.spawn().map_err(|_| {
            "Could not start Antigravity CLI (agy). Verify that agy runs normally.".into()
        })
    }
    #[cfg(not(windows))]
    {
        let _ = output_path;
        // Allocate a pseudo-terminal when `script` is available so `agy` renders its statusLine
        // even when launched from a GUI app without a controlling terminal.
        #[cfg(target_os = "macos")]
        let script_attempt = {
            let mut cmd = Command::new("/usr/bin/script");
            cmd.args(["-q", "/dev/null"])
                .arg(agy)
                .arg("--settings")
                .arg(settings_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            cmd.spawn()
        };
        #[cfg(not(target_os = "macos"))]
        let script_attempt = {
            let inner = format!(
                "{} --settings {}",
                quote_posix(agy)?,
                quote_posix(settings_path)?
            );
            let mut cmd = Command::new("/usr/bin/script");
            cmd.args(["-q", "-c", &inner, "/dev/null"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            cmd.spawn()
        };
        if let Ok(child) = script_attempt {
            return Ok(child);
        }
        let mut cmd = Command::new(agy);
        cmd.args(["--settings"])
            .arg(settings_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        cmd.spawn().map_err(|_| {
            "Could not start Antigravity CLI (agy). Verify that agy runs normally.".into()
        })
    }
}

struct CaptureDir(PathBuf);

impl CaptureDir {
    fn new() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("m-dash-agy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path)
            .map_err(|_| "Could not create a temporary directory for Antigravity refresh.")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for CaptureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub async fn collect() -> Result<Value, String> {
    let agy = executable()?;
    let helper = std::env::current_exe()
        .map_err(|_| "Could not locate m-dash executable for Antigravity capture.")?;
    let temp = CaptureDir::new()?;
    let output_path = temp.path().join("payload.json");
    let settings_path = temp.path().join("settings.json");

    let command = statusline_command(&helper, &output_path)?;
    let settings = json!({
        "statusLine": {
            "type": "command",
            "command": command,
            "stack_with_default": true
        }
    });
    std::fs::write(&settings_path, settings.to_string())
        .map_err(|_| "Could not write temporary Antigravity settings.")?;

    timeout(Duration::from_secs(45), async {
        let mut child = spawn_agy(&agy, &settings_path, &output_path)?;

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut captured_at: Option<Instant> = None;
        let captured: Result<Vec<u8>, String> = loop {
            let payload_bytes = std::fs::read(&output_path)
                .ok()
                .filter(|bytes| !bytes.is_empty());
            if let Ok(Some(_)) = child.try_wait() {
                if let Some(bytes) = payload_bytes {
                    break Ok(bytes);
                }
                break Err("Antigravity CLI exited before reporting quota. Run agy once to sign in, then refresh.".into());
            }
            if let Some(bytes) = payload_bytes {
                // On Windows, let the PowerShell runner finish its `finally` block (kills `agy`)
                // for up to 3 seconds before returning, so no hidden `agy` process is orphaned.
                #[cfg(windows)]
                {
                    let seen = *captured_at.get_or_insert_with(Instant::now);
                    if seen.elapsed() >= Duration::from_secs(3) {
                        break Ok(bytes);
                    }
                }
                #[cfg(not(windows))]
                {
                    let _ = &mut captured_at;
                    break Ok(bytes);
                }
            }
            if Instant::now() >= deadline {
                break Err("Antigravity CLI did not report quota within 30 seconds. Check agy sign-in and your connection.".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let _ = child.kill().await;
        let _ = child.wait().await;
        let bytes = captured?;
        let payload: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Antigravity returned an invalid quota payload.")?;
        normalize(payload, crate::collector::now())
    })
    .await
    .map_err(|_| "Antigravity quota refresh timed out. Your last successful values have been retained.".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_strips_conversation_context_and_keeps_only_accounting_fields() {
        let raw = json!({
            "email": "dev@example.com",
            "plan_tier": "Google AI Ultra",
            "version": "1.5.16",
            "cwd": "/private/repo",
            "conversation_title": "Secret design notes",
            "transcript_path": "/private/transcript.jsonl",
            "quota": {
                "gemini-5h": { "remaining_fraction": 0.875, "reset_time": "2026-11-25T01:00:00Z", "reset_in_seconds": 3600 }
            }
        });
        let filtered = filter_capture(&raw).unwrap();
        assert_eq!(filtered["email"], "dev@example.com");
        assert_eq!(filtered["plan_tier"], "Google AI Ultra");
        assert!(filtered.get("cwd").is_none());
        assert!(filtered.get("conversation_title").is_none());
        assert!(filtered.get("transcript_path").is_none());
        assert!(filtered["quota"]["gemini-5h"].is_object());
    }

    #[test]
    fn normalize_orders_ultra_quota_windows_and_separates_credit_amounts() {
        let payload = json!({
            "email": "fixture@example.com",
            "plan_tier": "Google AI Ultra",
            "quota": {
                "3p-weekly": { "remaining_fraction": 0.9, "reset_time": "2026-12-01T20:00:00Z", "reset_in_seconds": 604800 },
                "gemini-weekly": { "remaining_fraction": 0.95, "reset_time": "2026-12-01T19:00:00Z", "reset_in_seconds": 600000 },
                "gemini-5h": { "remaining_fraction": 0.8, "reset_in_seconds": 12000, "disabled": false },
                "3p-5h": { "remaining_fraction": 1.0, "reset_time": "2026-11-25T02:00:00Z", "disabled": true },
                "ai-credits": { "remaining_amount": 42 }
            }
        });
        let snapshot = normalize(payload, 1_000).unwrap();
        assert_eq!(snapshot["provider"], "antigravity");
        assert_eq!(snapshot["accountId"], "antigravity:fixture@example.com");
        assert_eq!(snapshot["plan"], "Google AI Ultra");
        let windows = snapshot["windows"].as_array().unwrap();
        let ids: Vec<&str> = windows.iter().map(|w| w["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            vec!["gemini-5h", "gemini-weekly", "3p-5h", "3p-weekly"]
        );
        assert_eq!(windows[0]["label"], "Gemini");
        assert_eq!(windows[0]["durationMins"], 300);
        assert!((windows[0]["remainingPercent"].as_f64().unwrap() - 80.0).abs() < 1e-6);
        assert!((windows[0]["usedPercent"].as_f64().unwrap() - 20.0).abs() < 1e-6);
        assert_eq!(windows[0]["resetsAt"], 13_000);
        assert_eq!(windows[2]["label"], "Claude & GPT");
        assert_eq!(windows[2]["blocked"], true);
        let credits = snapshot["credits"].as_array().unwrap();
        assert_eq!(credits.len(), 1);
        assert_eq!(credits[0]["id"], "ai-credits");
        assert_eq!(credits[0]["balance"], "42");
    }

    #[test]
    fn normalize_rejects_out_of_range_fractions_and_missing_identity() {
        assert!(normalize(json!({"quota": {}}), 100).is_err());
        assert!(normalize(
            json!({"email": "a@b.c", "quota": {"gemini-5h": {"remaining_fraction": 1.5}}}),
            100
        )
        .is_err());
    }

    #[test]
    fn statusline_command_encodes_safely_and_rejects_newlines() {
        let helper = PathBuf::from(if cfg!(windows) {
            r"C:\Program Files\m-dash's\m-dash.exe"
        } else {
            "/Applications/m-dash's/m-dash"
        });
        let output = PathBuf::from(if cfg!(windows) {
            r"C:\Temp\a'b\payload.json"
        } else {
            "/tmp/a'b/payload.json"
        });
        let cmd = statusline_command(&helper, &output).unwrap();
        #[cfg(windows)]
        assert!(cmd.starts_with(
            "powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand "
        ));
        #[cfg(not(windows))]
        assert!(cmd.contains("--antigravity-capture"));
        assert!(statusline_command(Path::new("bad\npath"), &output).is_err());
    }
}
