use crate::{dashboard::Dashboard, settings::Provider};
use m_dash_sync::{hash, Pair};
use serde_json::{json, Value};
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_preserves_allowance_labels_and_safely_marks_invalid_or_disconnected_readings() {
        let mut pair = Pair::new("https://relay.example.workers.dev", 1000).unwrap();
        pair.sequence = 1;
        let mut d = Dashboard::default();
        d.settings.providers.claude = true;
        d.claude_enabled = true;
        d.claude_usage_error = Some("CLI unavailable".into());
        d.claude = Some(
            json!({"source":"Claude Code statusline","quotaObservedAt":1000,"windows":[{"id":"spend_limit","label":"Gateway spending allowance","durationMins":null,"remainingPercent":25}]}),
        );
        d.settings.providers.grok = true;
        d.grok = Some(
            json!({"observedAt":1002,"periodEnd":"1970-01-02T00:00:00Z","remainingPercent":50,"prepaidCents":1e30}),
        );
        d.settings.providers.openrouter = true;
        d.openrouter = Some(json!({"observedAt":1000,"balanceUsd":10}));
        let value = snapshot(&d, &pair, 1001);
        m_dash_sync::validate_snapshot(&value).unwrap();
        assert_eq!(value["providers"][0]["status"], "ok");
        assert_eq!(
            value["providers"][0]["windows"][0]["label"],
            "Gateway spending allowance"
        );
        assert!(value["providers"][1]["observedAt"].is_null());
        assert!(value["providers"][1]["balanceUsd"].is_null());
        assert_eq!(value["providers"][1]["windows"][0]["id"], "allowance");
        assert_eq!(value["providers"][2]["status"], "unavailable");
    }
    #[test]
    fn export_uses_only_enabled_readings_and_never_exports_account_identifiers_or_secrets() {
        let pair = Pair::new("https://relay.example.workers.dev", 1000).unwrap();
        let mut d = Dashboard::default();
        d.settings.providers.codex = true;
        d.snapshot = Some(
            json!({"accountId":"private-email@example.com","secret":"DO-NOT-EXPORT","observedAt":900,"windows":[{"id":"primary","durationMins":300,"remainingPercent":75,"resetsAt":1100}],"dailyUsage":[{"date":"2026-10-17","tokens":12}]}),
        );
        let value = snapshot(&d, &pair, 1001);
        assert_eq!(value["providers"][0]["windows"][0]["remainingPercent"], 75);
        assert_eq!(value["providers"][0]["dailyTokens"][0]["tokens"], 12);
        assert!(!value.to_string().contains("private-email"));
        assert!(!value.to_string().contains("DO-NOT-EXPORT"));
        d.settings.providers.codex = false;
        assert!(snapshot(&d, &pair, 1001)["providers"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}

fn number(v: &Value, max: f64) -> Value {
    v.as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= max)
        .map_or(Value::Null, |_| v.clone())
}
fn timestamp(v: &Value) -> Value {
    v.as_u64()
        .filter(|n| *n <= 9_007_199_254_740_991)
        .map_or(Value::Null, |n| json!(n))
}
pub fn snapshot(d: &Dashboard, pair: &Pair, now: u64) -> Value {
    let mut providers = Vec::new();
    for (provider, id, source, failed) in [
        (Provider::Claude, "claude", &d.claude, d.claude_failed()),
        (Provider::Codex, "codex", &d.snapshot, d.error.is_some()),
        (Provider::Grok, "grok", &d.grok, d.grok_error.is_some()),
        (
            Provider::Openrouter,
            "openrouter",
            &d.openrouter,
            d.openrouter_error.is_some(),
        ),
    ] {
        if !d.settings.providers.enabled(provider) {
            continue;
        }
        let null = Value::Null;
        let s = source.as_ref().unwrap_or(&null);
        let mut observed = if id == "claude" {
            timestamp(&s["quotaObservedAt"])
        } else {
            timestamp(&s["observedAt"])
        };
        if observed.as_u64().is_some_and(|at| at > now) {
            observed = Value::Null;
        }
        let mut windows:Vec<Value>=s["windows"].as_array().into_iter().flatten().filter_map(|w|{
   let duration=w["durationMins"].as_u64().filter(|n|*n>0&&*n<=9_007_199_254_740_991);
   let wid=w["id"].as_str().filter(|s|!s.is_empty()&&s.len()<=80&&!s.chars().any(char::is_control))?;
   let label=w["label"].as_str().filter(|s|!s.is_empty()&&s.len()<=80&&!s.chars().any(char::is_control)).unwrap_or(wid);
   Some(json!({"id":wid,"label":label,"durationMinutes":duration,"remainingPercent":number(&w["remainingPercent"],100.0),"resetsAt":timestamp(&w["resetsAt"])}))
  }).collect();
        let parse_time = |v: &Value| {
            v.as_str()
                .and_then(|s| {
                    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .and_then(|t| u64::try_from(t.unix_timestamp()).ok())
        };
        if id == "grok" {
            if let Some(end) = parse_time(&s["periodEnd"]) {
                let duration = parse_time(&s["periodStart"])
                    .filter(|start| end.saturating_sub(*start) >= 60)
                    .map(|start| (end - start) / 60);
                windows.push(json!({"id":"allowance","label":"Grok allowance","durationMinutes":duration,"remainingPercent":number(&s["remainingPercent"],100.0),"resetsAt":end}));
            }
        }
        windows.sort_by_key(|w| w["durationMinutes"].as_u64());
        let mut seen = std::collections::HashSet::new();
        windows.retain(|w| seen.insert(w["id"].as_str().unwrap().to_owned()));
        windows.truncate(8);
        let mut days:Vec<Value>=s["dailyUsage"].as_array().into_iter().flatten().filter_map(|day|{
   let date=day["date"].as_str()?;let format=time::format_description::parse_borrowed::<2>("[year]-[month]-[day]").ok()?;time::Date::parse(date,&format).ok()?;
   let tokens=day["tokens"].as_u64().filter(|n|*n<=9_007_199_254_740_991)?;
   Some(json!({"date":date,"tokens":tokens,"scope":if id=="claude"{"local"}else{"account"}}))
  }).collect();
        days.sort_by(|a, b| b["date"].as_str().cmp(&a["date"].as_str()));
        days.dedup_by(|a, b| a["date"] == b["date"]);
        days.truncate(30);
        let balance = if id == "grok" {
            s["prepaidCents"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0 && *n / 100.0 <= 9_007_199_254_740_991.0)
                .map(|n| json!(n / 100.0))
                .unwrap_or(Value::Null)
        } else if id == "openrouter" {
            number(&s["balanceUsd"], 9_007_199_254_740_991.0)
        } else {
            Value::Null
        };
        providers.push(json!({"id":id,"accountId":hash(&format!("{}:{}:{}",pair.offer.key,id,s["accountId"].as_str().unwrap_or("unavailable"))),"observedAt":observed,"status":if id=="openrouter" && !d.openrouter_configured {"unavailable"}else if failed{"error"}else if observed.is_null(){"unavailable"}else{"ok"},"windows":windows,"balanceUsd":balance,"dailyTokens":days}));
    }
    json!({"version":2,"sourceId":pair.offer.id,"sourceName":"m-dash desktop","sequence":pair.sequence,"publishedAt":now,"providers":providers})
}

use m_dash_sync::{Method, Result};
use serde::{Deserialize, Serialize};
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub pairs: Vec<Pair>,
}
fn prune_expired_codes(saved: &mut Saved, now: u64) -> bool {
    let before = saved.pairs.len();
    saved.pairs.retain(|p| {
        p.approved || p.revoked || p.approval_claim.is_some() || p.offer.expires_at > now
    });
    saved.pairs.len() != before
}
#[derive(Default)]
pub struct State(pub tokio::sync::Mutex<()>);
#[cfg(target_os = "macos")]
fn load() -> Result<Saved> {
    match security_framework::passwords::get_generic_password(
        "net.chuckdudley.mdash/mobile-sync",
        "state",
    ) {
        Ok(bytes) => {
            if bytes.len() > 32768 {
                return Err("Saved sync state is invalid.".into());
            }
            serde_json::from_slice(&bytes)
                .map_err(|_| "Saved sync state is invalid; sync is paused.".into())
        }
        Err(e) if e.code() == -25300 => Ok(Saved::default()),
        Err(_) => Err("Unlock macOS Keychain to use mobile sync.".into()),
    }
}
#[cfg(target_os = "macos")]
fn save(value: &Saved) -> Result<()> {
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(value).map_err(|_| "Could not save sync state.")?,
    );
    security_framework::passwords::set_generic_password(
        "net.chuckdudley.mdash/mobile-sync",
        "state",
        &bytes,
    )
    .map_err(|_| "Could not save sync state in Keychain. Sync is paused.".into())
}
#[cfg(windows)]
fn load_windows(target: &str) -> Result<Saved> {
    match crate::credentials::read_target(target)? {
        None => Ok(Saved::default()),
        Some(bytes) => serde_json::from_str(&bytes)
            .map_err(|_| "Saved sync state is invalid; sync is paused.".into()),
    }
}
#[cfg(windows)]
fn save_windows(target: &str, value: &Saved) -> Result<()> {
    // shortcut: one Windows credential is bounded to 2560 bytes; use DPAPI files if longer relay origins exceed that limit.
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_string(value).map_err(|_| "Could not save sync state.")?,
    );
    crate::credentials::write_target(target, &bytes)
}
#[cfg(windows)]
fn load() -> Result<Saved> {
    load_windows("net.chuckdudley.mdash/mobile-sync")
}
#[cfg(windows)]
fn save(value: &Saved) -> Result<()> {
    save_windows("net.chuckdudley.mdash/mobile-sync", value)
}
#[cfg(not(any(windows, target_os = "macos")))]
fn load() -> Result<Saved> {
    Err("Desktop mobile publishing requires Windows or macOS.".into())
}
#[cfg(not(any(windows, target_os = "macos")))]
fn save(_: &Saved) -> Result<()> {
    Err("Desktop mobile publishing requires Windows or macOS.".into())
}

#[cfg(target_os = "macos")]
fn operator() -> Result<Option<Value>> {
    match security_framework::passwords::get_generic_password(
        "net.chuckdudley.mdash/mobile-relay",
        "operator",
    ) {
        Ok(bytes) => {
            let bytes = zeroize::Zeroizing::new(bytes);
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| "Relay setup is invalid.".into())
        }
        Err(e) if e.code() == -25300 => Ok(None),
        Err(_) => Err("Unlock Keychain to access relay setup.".into()),
    }
}
#[cfg(windows)]
fn operator() -> Result<Option<Value>> {
    crate::credentials::read_target("net.chuckdudley.mdash/mobile-relay")?
        .map(|bytes| serde_json::from_str(&bytes).map_err(|_| "Relay setup is invalid.".into()))
        .transpose()
}
#[cfg(not(any(windows, target_os = "macos")))]
fn operator() -> Result<Option<Value>> {
    Ok(None)
}

/// Operator setup accepts bounded private input; only the endpoint reaches the renderer.
pub fn configure_relay(input: &str) -> Result<()> {
    if input.len() > 4096 {
        return Err("Relay setup input is too large.".into());
    }
    let value: Value = serde_json::from_str(input).map_err(|_| "Relay setup is invalid.")?;
    let endpoint = value["endpoint"]
        .as_str()
        .ok_or("Missing relay endpoint.")?;
    let endpoint = m_dash_sync::endpoint(endpoint)?;
    let secret = value["enrollment"]
        .as_str()
        .ok_or("Missing enrollment access.")?;
    if secret.len() != 43
        || !secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid enrollment access.".into());
    }
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_string(&json!({"endpoint":endpoint,"enrollment":secret}))
            .map_err(|_| "Could not encode relay setup.")?,
    );
    #[cfg(windows)]
    return crate::credentials::write_target("net.chuckdudley.mdash/mobile-relay", &bytes);
    #[cfg(target_os = "macos")]
    return security_framework::passwords::set_generic_password(
        "net.chuckdudley.mdash/mobile-relay",
        "operator",
        bytes.as_bytes(),
    )
    .map_err(|_| "Keychain could not save relay access.".into());
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        drop(bytes);
        Err("Relay setup requires Windows or macOS secure storage.".into())
    }
}
#[tauri::command]
pub fn sync_relay() -> Result<Option<String>> {
    Ok(operator()?.and_then(|v| v["endpoint"].as_str().map(str::to_owned)))
}
#[tauri::command]
pub async fn sync_status(state: tauri::State<'_, State>) -> Result<Value> {
    let _guard = state.0.lock().await;
    let mut saved = load()?;
    if prune_expired_codes(&mut saved, crate::collector::now()) {
        save(&saved)?;
    }
    let mut rows = Vec::new();
    for p in saved.pairs {
        let mut row = json!({"id":p.offer.id,"approved":p.approved,"revoked":p.revoked,"needsRepair":p.offer.version!=2,"expiresAt":p.offer.expires_at,"sequence":p.sequence,"verification":null});
        if !p.approved && !p.revoked && p.offer.expires_at > crate::collector::now() {
            if let Ok(value) =
                m_dash_sync::request(&p.offer, "", Method::GET, &p.write_token, None).await
            {
                if let Some(claim) = value["claim"].as_str() {
                    row["verification"] = json!(p.offer.verification(claim)?);
                }
            }
        }
        rows.push(row);
    }
    Ok(json!(rows))
}
#[tauri::command]
pub async fn sync_pair(
    state: tauri::State<'_, State>,
    endpoint: String,
    enrollment: String,
    consent: bool,
) -> Result<String> {
    let endpoint = m_dash_sync::endpoint(&endpoint)?;
    let enrollment = zeroize::Zeroizing::new(if enrollment.is_empty() {
        let configured = operator()?
            .ok_or("Enter the relay enrollment secret or finish this computer's relay setup.")?;
        if configured["endpoint"]
            .as_str()
            .map(m_dash_sync::endpoint)
            .transpose()?
            .as_deref()
            != Some(endpoint.as_str())
        {
            return Err("Saved relay access is for a different endpoint.".into());
        }
        configured["enrollment"]
            .as_str()
            .ok_or("Relay setup is invalid.")?
            .to_owned()
    } else {
        enrollment
    });
    if !consent {
        return Err("Review and accept the managed-sync disclosure first.".into());
    }
    let _guard = state.0.lock().await;
    let mut saved = load()?;
    if prune_expired_codes(&mut saved, crate::collector::now()) {
        save(&saved)?;
    }
    if saved.pairs.len() >= 4 {
        return Err(
            "Remove an existing pairing before adding another phone (maximum four).".into(),
        );
    }
    let pair = Pair::new(&endpoint, crate::collector::now())?;
    // Persist first so a successful remote create never leaves an unmanageable channel.
    saved.pairs.push(pair.clone());
    save(&saved)?;
    m_dash_sync::enroll(&pair, &enrollment).await?;
    pair.code()
}
#[tauri::command]
pub async fn sync_approve(
    state: tauri::State<'_, State>,
    id: String,
    verification: String,
) -> Result<()> {
    let _guard = state.0.lock().await;
    let mut saved = load()?;
    let p = saved
        .pairs
        .iter_mut()
        .find(|p| p.offer.id == id && !p.revoked && !p.approved)
        .ok_or("Pairing unavailable.")?;
    let result = m_dash_sync::request(&p.offer, "", Method::GET, &p.write_token, None).await?;
    let claim = result["claim"]
        .as_str()
        .ok_or("Open the code on your phone first.")?;
    if p.offer.verification(claim)? != verification {
        return Err("Verification changed. Compare both devices again.".into());
    }
    let claim = claim.to_owned();
    p.approval_claim = Some(claim.clone());
    save(&saved)?;
    let p = saved
        .pairs
        .iter_mut()
        .find(|p| p.offer.id == id)
        .ok_or("Pairing unavailable.")?;
    m_dash_sync::request(
        &p.offer,
        "approve",
        Method::POST,
        &p.write_token,
        Some(json!({"claim":claim})),
    )
    .await?;
    p.approved = true;
    save(&saved)
}
#[tauri::command]
pub async fn sync_revoke(state: tauri::State<'_, State>, id: String) -> Result<()> {
    let _guard = state.0.lock().await;
    let mut saved = load()?;
    let p = saved
        .pairs
        .iter_mut()
        .find(|p| p.offer.id == id)
        .ok_or("Pairing unavailable.")?;
    p.revoked = true;
    save(&saved)?;
    flush_revocations(&mut saved).await
}
async fn flush_revocations(saved: &mut Saved) -> Result<()> {
    revoke_saved(saved, &save, &relay_request).await
}
async fn revoke_saved(
    saved: &mut Saved,
    persist: &impl Fn(&Saved) -> Result<()>,
    relay: &impl AsyncFn(&Pair, &str, Method, Option<Value>) -> Result<Value>,
) -> Result<()> {
    let mut failed = false;
    for p in saved.pairs.clone().into_iter().filter(|p| p.revoked) {
        match relay(&p, "", Method::DELETE, None).await {
            Ok(_) => saved.pairs.retain(|v| v.offer.id != p.offer.id),
            Err(e) if e == "Pairing revoked or relay authorization rejected." => {
                saved.pairs.retain(|v| v.offer.id != p.offer.id)
            }
            Err(_) => failed = true,
        }
    }
    persist(saved)?;
    if failed {
        Err("Uploads stopped. Remote removal pending; keep m-dash open to retry.".into())
    } else {
        Ok(())
    }
}
async fn relay_request(
    p: &Pair,
    action: &str,
    method: Method,
    body: Option<Value>,
) -> Result<Value> {
    m_dash_sync::request(&p.offer, action, method, &p.write_token, body).await
}
pub async fn publish(state: &State, d: &Dashboard) -> Result<()> {
    let _guard = state.0.lock().await;
    let mut saved = load()?;
    publish_to_relays(&mut saved, d, save, relay_request).await
}
/// Outbound publication boundary with disk and network supplied by the native host.
pub async fn publish_to_relays(
    saved: &mut Saved,
    d: &Dashboard,
    persist: impl Fn(&Saved) -> Result<()>,
    relay: impl AsyncFn(&Pair, &str, Method, Option<Value>) -> Result<Value>,
) -> Result<()> {
    let mut errors = Vec::new();
    if saved.pairs.iter().any(|p| p.revoked) {
        if let Err(error) = revoke_saved(saved, &persist, &relay).await {
            errors.push(error);
        }
    }
    let now = crate::collector::now();
    if prune_expired_codes(saved, now) {
        persist(saved)?;
    }
    for i in 0..saved.pairs.len() {
        if saved.pairs[i].revoked {
            continue;
        }
        if saved.pairs[i].offer.version != 2 {
            errors.push("Legacy phone pairing paused. Upgrade the private relay and Android app, then revoke this pairing and create a new code.".into());
            continue;
        }
        if !saved.pairs[i].approved {
            if let Some(claim) = saved.pairs[i].approval_claim.clone() {
                let p = &saved.pairs[i];
                match relay(p, "approve", Method::POST, Some(json!({"claim":claim}))).await {
                    Ok(_) => {
                        saved.pairs[i].approved = true;
                        persist(saved)?;
                    }
                    Err(error) => {
                        if error == "Pairing or snapshot expired." {
                            saved.pairs[i].revoked = true;
                            persist(saved)?;
                        }
                        errors.push(error);
                        continue;
                    }
                }
            } else {
                continue;
            }
        }
        saved.pairs[i].sequence = saved.pairs[i]
            .sequence
            .checked_add(1)
            .ok_or("Sync sequence exhausted.")?;
        persist(saved)?;
        let p = &saved.pairs[i];
        let value = snapshot(d, p, now);
        let envelope = match m_dash_sync::seal(&p.offer, &value) {
            Ok(envelope) => envelope,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        if let Err(error) = relay(p, "snapshot", Method::PUT, Some(json!(envelope))).await {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} phone operation(s) need attention: {}",
            errors.len(),
            errors[0]
        ))
    }
}

#[cfg(test)]
mod publisher_tests {
    use super::*;
    use std::sync::Mutex;
    #[tokio::test]
    async fn expired_codes_and_rejected_approvals_do_not_fill_slots_or_remove_approved_pairings() {
        let code = Pair::new("https://relay.example.workers.dev", 1).unwrap();
        let mut intent = code.clone();
        intent.offer.id = m_dash_sync::random();
        intent.approval_claim = Some(m_dash_sync::random());
        let mut ready =
            Pair::new("https://relay.example.workers.dev", crate::collector::now()).unwrap();
        ready.approved = true;
        let mut saved = Saved {
            pairs: vec![code, intent, ready],
        };
        assert!(publish_to_relays(
            &mut saved,
            &Dashboard::default(),
            |_| Ok(()),
            async |_: &Pair, action: &str, _: Method, _: Option<Value>| {
                if action == "approve" || action == "snapshot" {
                    Err("Pairing or snapshot expired.".into())
                } else {
                    Ok(json!({}))
                }
            }
        )
        .await
        .is_err());
        assert_eq!(saved.pairs.len(), 2);
        assert!(saved.pairs[0].revoked);
        assert!(saved.pairs[1].approved && !saved.pairs[1].revoked);
    }

    #[tokio::test]
    async fn invalid_pair_cannot_starve_a_valid_pair() {
        let mut bad =
            Pair::new("https://relay.example.workers.dev", crate::collector::now()).unwrap();
        bad.approved = true;
        bad.offer.key = "invalid".into();
        let mut good =
            Pair::new("https://relay.example.workers.dev", crate::collector::now()).unwrap();
        good.approved = true;
        let id = good.offer.id.clone();
        let sent = Mutex::new(Vec::new());
        let mut saved = Saved {
            pairs: vec![bad, good],
        };
        assert!(publish_to_relays(
            &mut saved,
            &Dashboard::default(),
            |_| Ok(()),
            async |p: &Pair, _: &str, _: Method, _: Option<Value>| {
                sent.lock().unwrap().push(p.offer.id.clone());
                Ok(json!({}))
            }
        )
        .await
        .is_err());
        assert_eq!(*sent.lock().unwrap(), [id]);
    }
    #[tokio::test]
    async fn rejected_phone_does_not_starve_healthy_phone_and_disk_failure_stops_uploads() {
        let mut first =
            Pair::new("https://relay.example.workers.dev", crate::collector::now()).unwrap();
        first.approved = true;
        let mut second =
            Pair::new("https://relay.example.workers.dev", crate::collector::now()).unwrap();
        second.approved = true;
        let denied = first.offer.id.clone();
        let good = second.offer.id.clone();
        let sent = Mutex::new(Vec::new());
        let mut saved = Saved {
            pairs: vec![first, second],
        };
        let result = publish_to_relays(
            &mut saved,
            &Dashboard::default(),
            |_| Ok(()),
            async |p: &Pair, _: &str, _: Method, _: Option<Value>| {
                sent.lock().unwrap().push(p.offer.id.clone());
                if p.offer.id == denied {
                    Err("Relay unavailable.".into())
                } else {
                    Ok(json!({}))
                }
            },
        )
        .await;
        assert!(result.is_err());
        assert!(sent.lock().unwrap().contains(&good));
        sent.lock().unwrap().clear();
        assert!(publish_to_relays(
            &mut saved,
            &Dashboard::default(),
            |_| Err("Disk unavailable".into()),
            async |p: &Pair, _: &str, _: Method, _: Option<Value>| {
                sent.lock().unwrap().push(p.offer.id.clone());
                Ok(json!({}))
            }
        )
        .await
        .is_err());
        assert!(sent.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn saved_owner_approval_intent_is_reconciled_before_publication() {
        let mut pair = Pair::new("https://relay.example.workers.dev", 1).unwrap();
        pair.approval_claim = Some(m_dash_sync::random());
        let mut saved = Saved { pairs: vec![pair] };
        let actions = Mutex::new(Vec::new());
        publish_to_relays(
            &mut saved,
            &Dashboard::default(),
            |_| Ok(()),
            async |_: &Pair, action: &str, _: Method, _: Option<Value>| {
                actions.lock().unwrap().push(action.to_owned());
                Ok(json!({}))
            },
        )
        .await
        .unwrap();
        assert!(saved.pairs[0].approved);
        assert_eq!(*actions.lock().unwrap(), ["approve", "snapshot"]);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn windows_protected_state_retains_four_phones_and_sequence_after_failed_replacement() {
        let target = format!(
            "net.chuckdudley.mdash/mobile-sync/test/{}",
            uuid::Uuid::new_v4()
        );
        let mut saved = Saved::default();
        for _ in 0..4 {
            let mut pair = Pair::new("https://relay.example.workers.dev", 1000).unwrap();
            pair.approved = true;
            pair.approval_claim = Some(m_dash_sync::random());
            pair.sequence = u64::MAX;
            saved.pairs.push(pair);
        }
        assert!(load_windows(&target).unwrap().pairs.is_empty());
        save_windows(&target, &saved).unwrap();
        let reopened = load_windows(&target).unwrap();
        assert_eq!(reopened.pairs.len(), 4);
        assert_eq!(reopened.pairs[3].sequence, u64::MAX);
        assert_eq!(reopened.pairs[3].offer.key, saved.pairs[3].offer.key);

        saved.pairs[0].offer.endpoint = "x".repeat(3000);
        assert!(save_windows(&target, &saved).is_err());
        let retained = load_windows(&target).unwrap();
        assert_eq!(retained.pairs[0].sequence, u64::MAX);
        assert_eq!(
            retained.pairs[0].offer.endpoint,
            "https://relay.example.workers.dev"
        );
        crate::credentials::write_target(&target, "not JSON").unwrap();
        assert!(load_windows(&target).is_err());
        crate::credentials::delete_target(&target).unwrap();
        assert!(load_windows(&target).unwrap().pairs.is_empty());
    }

    #[test]
    fn relay_setup_rejects_invalid_private_input_before_accessing_secure_storage() {
        assert_eq!(
            configure_relay(&"x".repeat(4097)).unwrap_err(),
            "Relay setup input is too large."
        );
        assert_eq!(
            configure_relay("not JSON").unwrap_err(),
            "Relay setup is invalid."
        );
        assert!(configure_relay(
            r#"{"endpoint":"http://relay.example.workers.dev","enrollment":"secret"}"#
        )
        .unwrap_err()
        .contains("HTTPS"));
        assert_eq!(
            configure_relay(
                r#"{"endpoint":"https://relay.example.workers.dev","enrollment":"do-not-echo"}"#
            )
            .unwrap_err(),
            "Invalid enrollment access."
        );
    }
}
