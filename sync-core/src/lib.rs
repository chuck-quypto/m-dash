use aes_gcm::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
pub type Result<T> = std::result::Result<T, String>;
pub const MAX_BODY: usize = 100_000;
const INVALID: &str = "Sync data could not be authenticated.";
pub fn random() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    B64.encode(bytes)
}
pub fn hash(value: &str) -> String {
    B64.encode(Sha256::digest(value.as_bytes()))
}
fn bytes(value: &str, len: usize) -> Result<Vec<u8>> {
    let b = B64.decode(value).map_err(|_| INVALID)?;
    if b.len() != len {
        return Err(INVALID.into());
    }
    Ok(b)
}
pub fn endpoint(value: &str) -> Result<String> {
    let u = reqwest::Url::parse(value).map_err(|_| "Enter an HTTPS relay origin.")?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/"
        || u.query().is_some()
        || u.fragment().is_some()
        || u.port().is_some()
    {
        return Err("Enter an HTTPS relay origin without a path or port.".into());
    }
    let host = u.host_str().unwrap();
    if host == "localhost"
        || host.parse::<std::net::IpAddr>().is_ok()
        || !host.ends_with(".workers.dev")
    {
        return Err("This development build accepts Cloudflare workers.dev relays only.".into());
    }
    Ok(u.origin().ascii_serialization())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Offer {
    pub version: u8,
    pub endpoint: String,
    pub id: String,
    pub key: String,
    pub read_token: String,
    pub expires_at: u64,
}
impl Offer {
    pub fn decode(code: &str, now: u64) -> Result<Self> {
        if code.len() > 4096 {
            return Err(INVALID.into());
        }
        let raw = B64
            .decode(
                code.strip_prefix("mdash2:")
                    .ok_or("Create a new pairing code with the updated desktop app.")?,
            )
            .map_err(|_| INVALID)?;
        let offer: Self = serde_json::from_slice(&raw).map_err(|_| INVALID)?;
        offer.validate()?;
        if offer.version != 2 {
            return Err(INVALID.into());
        }
        if offer.expires_at.saturating_add(300) <= now || offer.expires_at > now.saturating_add(900)
        {
            return Err("Pairing code expired. Create a new one on desktop.".into());
        }
        Ok(offer)
    }
    pub fn validate(&self) -> Result<()> {
        if ![1, 2].contains(&self.version) || endpoint(&self.endpoint)? != self.endpoint {
            return Err(INVALID.into());
        }
        bytes(&self.id, 32)?;
        bytes(&self.key, 32)?;
        bytes(&self.read_token, 32)?;
        Ok(())
    }
    pub fn verification(&self, claim: &str) -> Result<String> {
        bytes(claim, 32)?;
        Ok(hash(&format!("{}:{}", self.key, claim))[..12].to_owned())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pair {
    pub offer: Offer,
    pub write_token: String,
    pub sequence: u64,
    pub approved: bool,
    #[serde(default)]
    pub approval_claim: Option<String>,
    pub revoked: bool,
}
impl Pair {
    pub fn new(url: &str, now: u64) -> Result<Self> {
        Ok(Self {
            offer: Offer {
                version: 2,
                endpoint: endpoint(url)?,
                id: random(),
                key: random(),
                read_token: random(),
                expires_at: now + 600,
            },
            write_token: random(),
            sequence: 0,
            approved: false,
            approval_claim: None,
            revoked: false,
        })
    }
    pub fn code(&self) -> Result<String> {
        Ok(format!(
            "mdash2:{}",
            B64.encode(serde_json::to_vec(&self.offer).map_err(|_| INVALID)?)
        ))
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Envelope {
    pub version: u8,
    pub id: String,
    pub sequence: u64,
    pub published_at: u64,
    pub nonce: String,
    pub ciphertext: String,
}
fn aad(e: &Envelope) -> Vec<u8> {
    format!("m-dash:1:{}:{}:{}", e.id, e.sequence, e.published_at).into_bytes()
}
pub fn seal(offer: &Offer, snapshot: &Value) -> Result<Envelope> {
    offer.validate()?;
    validate_snapshot(snapshot)?;
    let mut e = Envelope {
        version: 1,
        id: offer.id.clone(),
        sequence: snapshot["sequence"].as_u64().ok_or(INVALID)?,
        published_at: snapshot["publishedAt"].as_u64().ok_or(INVALID)?,
        nonce: String::new(),
        ciphertext: String::new(),
    };
    if snapshot["sourceId"] != offer.id || e.sequence == 0 || e.sequence > 9_007_199_254_740_991 {
        return Err(INVALID.into());
    }
    let plaintext = serde_json::to_vec(snapshot).map_err(|_| INVALID)?;
    if plaintext.len() > 65536 {
        return Err(INVALID.into());
    }
    let key = bytes(&offer.key, 32)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| INVALID)?;
    let mut nonce = [0; 12];
    OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &aad(&e),
            },
        )
        .map_err(|_| INVALID)?;
    e.nonce = B64.encode(nonce);
    e.ciphertext = B64.encode(encrypted);
    Ok(e)
}
pub fn open(offer: &Offer, e: &Envelope, previous: Option<&Envelope>, now: u64) -> Result<Value> {
    if now.saturating_sub(e.published_at) >= 604800 {
        return Err(INVALID.into());
    }
    authenticate(offer, e, previous, now)
}
fn authenticate(
    offer: &Offer,
    e: &Envelope,
    previous: Option<&Envelope>,
    now: u64,
) -> Result<Value> {
    offer.validate()?;
    if e.version != 1
        || e.id != offer.id
        || e.sequence == 0
        || e.sequence > 9_007_199_254_740_991
        || e.published_at > now + 300
        || e.ciphertext.len() > 87404
    {
        return Err(INVALID.into());
    }
    if previous.is_some_and(|p| {
        e.sequence < p.sequence
            || e.published_at < p.published_at
            || (e.sequence == p.sequence && e != p)
    }) {
        return Err("Older or conflicting snapshot rejected.".into());
    }
    let nonce = bytes(&e.nonce, 12)?;
    let key = bytes(&offer.key, 32)?;
    let ciphertext = B64.decode(&e.ciphertext).map_err(|_| INVALID)?;
    let plain = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| INVALID)?
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad(e),
            },
        )
        .map_err(|_| INVALID)?;
    if plain.len() > 65536 {
        return Err(INVALID.into());
    }
    let snapshot: Value = serde_json::from_slice(&plain).map_err(|_| INVALID)?;
    validate_snapshot(&snapshot)?;
    if snapshot["sourceId"] != e.id
        || snapshot["sequence"] != e.sequence
        || snapshot["publishedAt"] != e.published_at
    {
        return Err(INVALID.into());
    }
    Ok(snapshot)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_pairings_use_fresh_phone_read_authority_and_tolerate_small_clock_skew() {
        let p = Pair::new("https://RELAY.example.workers.dev/", 1000).unwrap();
        assert_eq!(p.offer.endpoint, "https://relay.example.workers.dev");
        let phone = Phone::pair(&p.code().unwrap(), 940).unwrap();
        assert_ne!(
            phone.read_token.as_deref(),
            Some(phone.offer.read_token.as_str())
        );
        let reopened: Phone = serde_json::from_slice(&serde_json::to_vec(&phone).unwrap()).unwrap();
        assert_eq!(reopened.read_token, phone.read_token);
        assert_eq!(reopened.claim, phone.claim);
        assert!(Offer::decode(&p.code().unwrap(), 699).is_err());
        assert!(Offer::decode(&p.code().unwrap(), 1900).is_err());
    }
    #[test]
    fn only_the_paired_phone_can_open_an_unmodified_snapshot() {
        let pair = Pair::new("https://relay.example.workers.dev", 1000).unwrap();
        let offer = Offer::decode(&pair.code().unwrap(), 1001).unwrap();
        let snapshot = serde_json::json!({"version":1,"sourceName":"Desktop","sourceId":offer.id,"sequence":1,"publishedAt":1001,"providers":[]});
        let envelope = seal(&offer, &snapshot).unwrap();
        assert_eq!(open(&offer, &envelope, None, 1002).unwrap(), snapshot);
        let other = Pair::new("https://relay.example.workers.dev", 1000).unwrap();
        assert!(open(&other.offer, &envelope, None, 1002).is_err());
        let mut changed = envelope.clone();
        changed.sequence = 2;
        assert!(open(&offer, &changed, None, 1002).is_err());
        assert!(Offer::decode(&pair.code().unwrap(), 1900).is_err());
    }
}

/// Fixed relay operations; no arbitrary URL/headers are exposed to renderers.
pub async fn request(
    offer: &Offer,
    action: &str,
    method: reqwest::Method,
    token: &str,
    body: Option<Value>,
) -> Result<Value> {
    endpoint(&offer.endpoint)?;
    let suffix = match action {
        "" => "",
        "claim" => "/claim",
        "approve" => "/approve",
        "snapshot" => "/snapshot",
        _ => return Err(INVALID.into()),
    };
    call(
        &format!("{}/v1/devices/{}{}", offer.endpoint, offer.id, suffix),
        method,
        token,
        body,
    )
    .await
}
pub async fn enroll(pair: &Pair, enrollment: &str) -> Result<()> {
    call(&format!("{}/v1/devices",endpoint(&pair.offer.endpoint)?),reqwest::Method::POST,enrollment,Some(serde_json::json!({"id":pair.offer.id,"readHash":hash(&pair.offer.read_token),"writeHash":hash(&pair.write_token)}))).await?;
    Ok(())
}
async fn call(
    url: &str,
    method: reqwest::Method,
    token: &str,
    body: Option<Value>,
) -> Result<Value> {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = if let Some(client) = CLIENT.get() {
        client.clone()
    } else {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|_| "Relay unavailable.".to_owned())?;
        let _ = CLIENT.set(client.clone());
        client
    };
    let mut req = client.request(method, url).bearer_auth(token);
    if let Some(body) = body {
        req = req.json(&body)
    }
    let mut response = req
        .send()
        .await
        .map_err(|_| "Relay unavailable. Last saved readings retained.")?;
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            400 => "Relay rejected sync data. Check app/relay compatibility and the system clock; saved readings were retained.",
            401 => "Pairing revoked or relay authorization rejected.",
            403 => "Waiting for desktop approval.",
            404 => "Waiting for the first desktop upload.",
            409 => "Pairing or snapshot conflict; refresh and retry.",
            410 => "Pairing or snapshot expired.",
            429 => "Relay busy. Wait a minute before retrying.",
            _ => "Relay unavailable. Last saved readings retained.",
        }
        .into());
    }
    let mut result = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Relay response interrupted.")?
    {
        if result.len() + chunk.len() > MAX_BODY {
            return Err(INVALID.into());
        }
        result.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&result).map_err(|_| INVALID.into())
}
/// A usage-channel request to a validated relay origin; `path` must be a `/v1/usage` route.
pub async fn usage_request(
    origin: &str,
    path: &str,
    method: reqwest::Method,
    token: &str,
    body: Option<Value>,
) -> Result<Value> {
    if !path.starts_with("/v1/usage") {
        return Err(INVALID.into());
    }
    call(&format!("{}{path}", endpoint(origin)?), method, token, body).await
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Phone {
    pub offer: Offer,
    pub claim: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_token: Option<String>,
    pub last: Option<Envelope>,
    pub received_at: Option<u64>,
}
impl Phone {
    pub fn pair(code: &str, now: u64) -> Result<Self> {
        Ok(Self {
            offer: Offer::decode(code, now)?,
            claim: random(),
            read_token: Some(random()),
            last: None,
            received_at: None,
        })
    }
    pub async fn claim(&self) -> Result<()> {
        request(
            &self.offer,
            "claim",
            reqwest::Method::POST,
            &self.offer.read_token,
            Some(serde_json::json!({"claim":self.claim,"readHash":hash(self.read_token.as_deref().ok_or("This legacy pairing needs to be replaced with a new desktop code.")?)})),
        )
        .await?;
        Ok(())
    }
    pub async fn pull(&mut self, now: u64) -> Result<Value> {
        let envelope: Envelope = serde_json::from_value(
            request(
                &self.offer,
                "snapshot",
                reqwest::Method::GET,
                self.read_token
                    .as_deref()
                    .ok_or("This legacy pairing needs to be replaced with a new desktop code.")?,
                None,
            )
            .await?,
        )
        .map_err(|_| INVALID)?;
        let snapshot = open(&self.offer, &envelope, self.last.as_ref(), now)?;
        self.last = Some(envelope);
        self.received_at = Some(now);
        Ok(snapshot)
    }
    pub fn cached(&self, now: u64) -> Result<Option<Value>> {
        self.last
            .as_ref()
            .map(|e| authenticate(&self.offer, e, None, now))
            .transpose()
    }
}
pub use reqwest::Method;

#[cfg(test)]
mod admission_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn authenticated_but_invalid_or_secret_bearing_snapshots_are_not_admitted() {
        let p = Pair::new("https://relay.example.workers.dev", 1000).unwrap();
        let bad = json!({"sourceId":p.offer.id,"sequence":1,"publishedAt":1001,"providers":[],"secret":"no"});
        assert!(seal(&p.offer, &bad).is_err());
    }
}

fn fields(v: &Value, names: &[&str]) -> Result<()> {
    let map = v.as_object().ok_or(INVALID)?;
    if map.len() != names.len() || !map.keys().all(|k| names.contains(&k.as_str())) {
        return Err(INVALID.into());
    }
    Ok(())
}
fn valid_text(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= 80 && !s.chars().any(char::is_control))
}
fn safe_integer(v: &Value) -> bool {
    v.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991)
}
fn nullable_number(v: &Value, max: f64) -> bool {
    v.is_null()
        || v.as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0 && n <= max)
}
pub fn validate_snapshot(s: &Value) -> Result<()> {
    fields(
        s,
        &[
            "version",
            "sourceId",
            "sourceName",
            "sequence",
            "publishedAt",
            "providers",
        ],
    )?;
    if (s["version"] != 1 && s["version"] != 2)
        || !valid_text(&s["sourceId"])
        || !valid_text(&s["sourceName"])
        || !safe_integer(&s["sequence"])
        || s["sequence"] == 0
        || !safe_integer(&s["publishedAt"])
    {
        return Err(INVALID.into());
    }
    let providers = s["providers"].as_array().ok_or(INVALID)?;
    if providers.len() > 4 {
        return Err(INVALID.into());
    }
    let mut ids = std::collections::HashSet::new();
    for p in providers {
        fields(
            p,
            &[
                "id",
                "accountId",
                "observedAt",
                "status",
                "windows",
                "balanceUsd",
                "dailyTokens",
            ],
        )?;
        if !["claude", "codex", "grok", "openrouter"].contains(&p["id"].as_str().unwrap_or(""))
            || !ids.insert(p["id"].as_str())
            || !valid_text(&p["accountId"])
            || !["ok", "error", "unavailable"].contains(&p["status"].as_str().unwrap_or(""))
            || !nullable_number(&p["balanceUsd"], 9_007_199_254_740_991.0)
        {
            return Err(INVALID.into());
        }
        if !p["observedAt"].is_null()
            && (!safe_integer(&p["observedAt"])
                || p["observedAt"].as_u64() > s["publishedAt"].as_u64())
        {
            return Err(INVALID.into());
        }
        let windows = p["windows"].as_array().ok_or(INVALID)?;
        if windows.len() > 8 {
            return Err(INVALID.into());
        }
        let mut wids = std::collections::HashSet::new();
        for w in windows {
            if s["version"] == 2 {
                fields(
                    w,
                    &[
                        "id",
                        "label",
                        "durationMinutes",
                        "remainingPercent",
                        "resetsAt",
                    ],
                )?;
                if !valid_text(&w["label"]) {
                    return Err(INVALID.into());
                }
            } else {
                fields(
                    w,
                    &["id", "durationMinutes", "remainingPercent", "resetsAt"],
                )?;
            }
            if !valid_text(&w["id"])
                || !wids.insert(w["id"].as_str())
                || (!(s["version"] == 2 && w["durationMinutes"].is_null())
                    && (!safe_integer(&w["durationMinutes"]) || w["durationMinutes"] == 0))
                || !nullable_number(&w["remainingPercent"], 100.0)
                || (!w["resetsAt"].is_null() && !safe_integer(&w["resetsAt"]))
            {
                return Err(INVALID.into());
            }
        }
        let days = p["dailyTokens"].as_array().ok_or(INVALID)?;
        if days.len() > 30 {
            return Err(INVALID.into());
        }
        let mut dates = std::collections::HashSet::new();
        for d in days {
            fields(d, &["date", "tokens", "scope"])?;
            let date = d["date"].as_str().ok_or(INVALID)?;
            if !valid_date(date)
                || !dates.insert(date)
                || !safe_integer(&d["tokens"])
                || !["local", "account"].contains(&d["scope"].as_str().unwrap_or(""))
            {
                return Err(INVALID.into());
            }
        }
    }
    Ok(())
}
fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return false;
    }
    let Ok(y) = s[..4].parse::<u32>() else {
        return false;
    };
    let Ok(m) = s[5..7].parse::<usize>() else {
        return false;
    };
    let Ok(d) = s[8..].parse::<u32>() else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    m > 0 && m <= 12 && d > 0 && d <= days[m - 1]
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn authenticated_local_cache_remains_available_after_relay_expiry() {
        let p = Pair::new("https://relay.example.workers.dev", 1000).unwrap();
        let value = json!({"version":1,"sourceName":"Desktop","sourceId":p.offer.id,"sequence":1,"publishedAt":1001,"providers":[]});
        let last = Some(seal(&p.offer, &value).unwrap());
        let phone = Phone {
            offer: p.offer,
            claim: random(),
            read_token: None,
            last,
            received_at: Some(1002),
        };
        assert_eq!(phone.cached(1001 + 604800).unwrap(), Some(value));
    }
    #[test]
    fn replay_conflicts_expiry_and_unsafe_endpoints_are_rejected() {
        let p = Pair::new("https://relay.example.workers.dev", 1000).unwrap();
        let value = json!({"version":1,"sourceName":"Desktop","sourceId":p.offer.id,"sequence":2,"publishedAt":1001,"providers":[]});
        let e = seal(&p.offer, &value).unwrap();
        assert!(open(&p.offer, &e, Some(&e), 1002).is_ok());
        let mut old = value.clone();
        old["sequence"] = json!(1);
        let old = seal(&p.offer, &old).unwrap();
        assert!(open(&p.offer, &old, Some(&e), 1002).is_err());
        let different = seal(&p.offer, &value).unwrap();
        assert!(open(&p.offer, &different, Some(&e), 1002).is_err());
        assert!(open(&p.offer, &e, None, 1001 + 604800).is_err());
        for url in [
            "http://relay.example.workers.dev",
            "https://127.0.0.1",
            "https://relay.example.workers.dev/path",
            "https://user:pass@relay.example.workers.dev",
            "https://evil.example",
        ] {
            assert!(Pair::new(url, 1000).is_err());
        }
    }
}
