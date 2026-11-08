//! Optional usage channel on the owner's relay. Each computer sends its per-request accounting
//! rows and receives the others', and Claude Code cloud sessions export their telemetry to it
//! directly, so the burn ledger can account for usage this computer never sees. Rows carry
//! token counts, model and request identifiers only; the relay drops everything else.
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

type Result<T> = std::result::Result<T, String>;
#[cfg(any(windows, target_os = "macos"))]
const TARGET: &str = "net.chuckdudley.mdash/usage-relay";
const CODE_PREFIX: &str = "mdash-usage1:";
/// Account of the rows that announce a computer's name and setup instead of usage. No provider
/// account looks like this, so versions that predate announcements store them but never show them.
const DEVICE_ACCOUNT: &str = "m-dash:device";

/// What this computer tells the others on the channel about itself, once a day or on a change.
pub struct Device {
    pub name: String,
    /// Short setup summary, such as "m-dash 0.6.0;logs=1;feed=0".
    pub details: String,
}

/// Another computer's announcement: its channel identity, name, setup summary and time.
#[derive(Debug, PartialEq)]
pub struct Announced {
    pub origin: String,
    pub name: String,
    pub details: String,
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Channel {
    pub endpoint: String,
    pub id: String,
    /// Lets a holder add rows: cloud sessions and the owner's computers.
    pub ingest: String,
    /// Lets a holder read every row: the owner's computers only.
    pub read: String,
    /// Identifies this computer's rows so it skips them when reading.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
}

fn token(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Channel {
    fn validate(self) -> Result<Self> {
        m_dash_sync::endpoint(&self.endpoint)?;
        if !token(&self.id)
            || !token(&self.ingest)
            || !token(&self.read)
            || self.ingest == self.read
        {
            return Err("This usage channel code is invalid.".into());
        }
        Ok(self)
    }

    /// A code that joins another of the owner's computers to this channel.
    pub fn code(&self) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let shared = Channel {
            device: String::new(),
            ..self.clone()
        };
        format!(
            "{CODE_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&shared).unwrap_or_default())
        )
    }

    pub fn from_code(code: &str) -> Result<Self> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let invalid = || "This usage channel code is invalid.".to_string();
        let encoded = code
            .trim()
            .strip_prefix(CODE_PREFIX)
            .filter(|rest| rest.len() <= 2048)
            .ok_or_else(invalid)?;
        let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
        let mut channel: Channel = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        channel.device = m_dash_sync::random();
        channel.validate()
    }

    /// A setup script for a Claude Code cloud environment that makes its sessions export usage
    /// here. Cloud environments drop `OTEL_*` variables, so the script writes the exporter
    /// settings into Claude Code's user settings before the session starts instead.
    pub fn cloud_setup_script(&self) -> String {
        CLOUD_SETUP.replace(
            "@ENDPOINT@",
            &format!(
                "{}/v1/usage/{}/{}/logs",
                self.endpoint, self.id, self.ingest
            ),
        )
    }
}

const CLOUD_SETUP: &str = r#"# m-dash: send Claude Code usage from cloud sessions to your relay.
MDASH_ENDPOINT='@ENDPOINT@'
mkdir -p "$HOME/.claude"
MDASH_ENDPOINT="$MDASH_ENDPOINT" node -e '
const fs = require("fs"), path = require("path").join(process.env.HOME, ".claude", "settings.json");
let settings = {}; try { settings = JSON.parse(fs.readFileSync(path, "utf8")); } catch {}
settings.env = { ...settings.env, CLAUDE_CODE_ENABLE_TELEMETRY: "1", OTEL_LOGS_EXPORTER: "otlp",
  OTEL_EXPORTER_OTLP_LOGS_PROTOCOL: "http/json", OTEL_EXPORTER_OTLP_LOGS_ENDPOINT: process.env.MDASH_ENDPOINT };
fs.writeFileSync(path, JSON.stringify(settings, null, 2) + "\n");
'
"#;

#[cfg(windows)]
pub fn load() -> Result<Option<Channel>> {
    crate::credentials::read_target(TARGET)?
        .map(|text| {
            serde_json::from_str::<Channel>(&text)
                .map_err(|_| "Saved usage channel is invalid.".to_string())
                .and_then(Channel::validate)
        })
        .transpose()
}
#[cfg(windows)]
fn save(channel: Option<&Channel>) -> Result<()> {
    match channel {
        Some(channel) => crate::credentials::write_target(
            TARGET,
            &zeroize::Zeroizing::new(
                serde_json::to_string(channel).map_err(|_| "Cannot encode usage channel.")?,
            ),
        ),
        None => crate::credentials::delete_target(TARGET),
    }
}
#[cfg(target_os = "macos")]
pub fn load() -> Result<Option<Channel>> {
    match security_framework::passwords::get_generic_password(TARGET, "channel") {
        Ok(bytes) => {
            let bytes = zeroize::Zeroizing::new(bytes);
            serde_json::from_slice::<Channel>(&bytes)
                .map_err(|_| "Saved usage channel is invalid.".to_string())
                .and_then(Channel::validate)
                .map(Some)
        }
        Err(e) if e.code() == -25300 => Ok(None),
        Err(_) => Err("Unlock Keychain to access the usage channel.".into()),
    }
}
#[cfg(target_os = "macos")]
fn save(channel: Option<&Channel>) -> Result<()> {
    match channel {
        Some(channel) => security_framework::passwords::set_generic_password(
            TARGET,
            "channel",
            zeroize::Zeroizing::new(
                serde_json::to_vec(channel).map_err(|_| "Cannot encode usage channel.")?,
            )
            .as_slice(),
        )
        .map_err(|_| "Keychain could not save the usage channel.".into()),
        None => match security_framework::passwords::delete_generic_password(TARGET, "channel") {
            Err(e) if e.code() != -25300 => {
                Err("Keychain could not remove the usage channel.".into())
            }
            _ => Ok(()),
        },
    }
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn load() -> Result<Option<Channel>> {
    Ok(None)
}
#[cfg(not(any(windows, target_os = "macos")))]
fn save(_: Option<&Channel>) -> Result<()> {
    Err("The usage channel requires Windows or macOS secure storage.".into())
}

fn status(channel: Option<Channel>) -> Value {
    match channel {
        Some(channel) => {
            json!({"endpoint":channel.endpoint,"cloudSetup":channel.cloud_setup_script(),"code":channel.code()})
        }
        None => Value::Null,
    }
}

#[tauri::command]
pub fn usage_relay_status() -> Result<Value> {
    Ok(status(load()?))
}

/// Creates a channel on the relay with the operator's enrollment secret.
#[tauri::command]
pub async fn usage_relay_create(endpoint: String, enrollment: String) -> Result<Value> {
    let endpoint = m_dash_sync::endpoint(&endpoint)?;
    let enrollment = zeroize::Zeroizing::new(if enrollment.is_empty() {
        let operator = crate::mobile_sync::operator()?
            .filter(|value| value["endpoint"].as_str() == Some(endpoint.as_str()))
            .ok_or("Enter the relay enrollment secret.")?;
        operator["enrollment"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    } else {
        enrollment
    });
    let channel = Channel {
        endpoint,
        id: m_dash_sync::random(),
        ingest: m_dash_sync::random(),
        read: m_dash_sync::random(),
        device: m_dash_sync::random(),
    };
    m_dash_sync::usage_request(&channel.endpoint, "/v1/usage", Method::POST, &enrollment,
        Some(json!({"id":channel.id,"ingestHash":m_dash_sync::hash(&channel.ingest),"readHash":m_dash_sync::hash(&channel.read)}))).await?;
    save(Some(&channel))?;
    Ok(status(Some(channel)))
}

#[tauri::command]
pub fn usage_relay_join(code: String) -> Result<Value> {
    let channel = Channel::from_code(&code)?;
    save(Some(&channel))?;
    Ok(status(Some(channel)))
}

/// Forgets the channel on this computer; `delete` also removes it and its rows from the relay.
#[tauri::command]
pub async fn usage_relay_leave(delete: bool) -> Result<Value> {
    if let Some(channel) = load()? {
        if delete {
            m_dash_sync::usage_request(
                &channel.endpoint,
                &format!("/v1/usage/{}", channel.id),
                Method::DELETE,
                &channel.read,
                None,
            )
            .await?;
        }
    }
    save(None)?;
    Ok(Value::Null)
}

/// One local row in the relay's wire format.
pub fn wire(
    account: &str,
    key: &str,
    at: i64,
    request: &crate::usage::Request,
    device: &str,
) -> Value {
    json!({"provider":if account.starts_with("claude:") {"claude"} else {"codex"},"account":account,"key":key,"at":at,
        "surface":request.surface,"model":request.model,"effort":request.effort,"speed":request.speed,"querySource":request.query_source,
        "agent":request.agent,"input":request.input,"output":request.output,"cacheRead":request.cache_read,"cacheWrite":request.cache_write,
        "reasoning":request.reasoning,"costMicros":request.cost_micros,"origin":device})
}

/// The row announcing this computer. Its key changes daily and with its content, so the relay,
/// which keeps rows for 14 days and ignores repeated keys, always holds a recent one.
fn announcement(device: &Device, origin: &str, now: i64) -> Value {
    let name: String = device.name.chars().take(64).collect();
    let details: String = device.details.chars().take(128).collect();
    let digest = m_dash_sync::hash(&format!("{name}\n{details}"));
    json!({"provider":"codex","account":DEVICE_ACCOUNT,"key":format!("device:{}:{}", now / 86_400, &digest[..12]),"at":now,
        "surface":"device","model":name,"effort":null,"speed":null,"querySource":details,"agent":null,
        "input":0,"output":0,"cacheRead":0,"cacheWrite":0,"reasoning":null,"costMicros":null,"origin":origin})
}

/// Parses another computer's announcement.
pub fn announced(row: &Value, device: &str) -> Option<Announced> {
    let origin = row["origin"]
        .as_str()
        .filter(|origin| *origin != device && *origin != "cloud")?;
    (row["account"] == DEVICE_ACCOUNT).then_some(())?;
    let text = |name: &str, max: usize| {
        row[name]
            .as_str()
            .filter(|value| !value.is_empty() && value.chars().count() <= max)
            .map(str::to_owned)
    };
    Some(Announced {
        origin: origin.to_owned(),
        name: text("model", 64)?,
        details: text("querySource", 128).unwrap_or_default(),
        at: row["at"].as_i64()?,
    })
}

/// Parses one row read from the relay, skipping this computer's own rows.
pub fn unwire(row: &Value, device: &str) -> Option<(i64, crate::usage::Keyed)> {
    let text = |name: &str| {
        row[name]
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= 300)
            .map(str::to_owned)
    };
    let amount = |name: &str| row[name].as_i64().filter(|value| *value >= 0);
    let origin = row["origin"].as_str()?;
    if origin == device || row["account"] == DEVICE_ACCOUNT {
        return None;
    }
    let surface = if origin == "cloud" {
        "cloud".to_owned()
    } else {
        text("surface")?
    };
    Some((
        row["seq"].as_i64()?,
        (
            text("account")?,
            text("key")?,
            amount("at")?,
            crate::usage::Request {
                surface,
                model: text("model"),
                effort: text("effort"),
                speed: text("speed"),
                query_source: text("querySource"),
                agent: text("agent"),
                input: amount("input")?,
                output: amount("output")?,
                cache_read: amount("cacheRead")?,
                cache_write: amount("cacheWrite")?,
                reasoning: amount("reasoning"),
                cost_micros: amount("costMicros"),
            },
        ),
    ))
}

/// Sends local rows added since the last pass and stores rows from other sources.
/// Network calls run without the collector lock; `with_store` takes it briefly.
/// Returns whether a channel is set up.
pub async fn sync(
    with_store: impl Fn(&mut dyn FnMut(&mut crate::store::Store) -> Result<()>) -> Result<()>,
    me: Device,
) -> Result<bool> {
    let Some(channel) = load()? else {
        return Ok(false);
    };
    let row = announcement(&me, &channel.device, crate::collector::now() as i64);
    let key = row["key"].as_str().unwrap_or_default().to_owned();
    let mut sent = String::new();
    with_store(&mut |store| {
        sent = store
            .setting(&format!("usage-relay-announced:{}", channel.id))?
            .unwrap_or_default();
        Ok(())
    })?;
    if sent != key {
        m_dash_sync::usage_request(
            &channel.endpoint,
            &format!("/v1/usage/{}/rows", channel.id),
            Method::POST,
            &channel.ingest,
            Some(json!({ "rows": [row] })),
        )
        .await?;
        with_store(&mut |store| {
            store.save_setting(&format!("usage-relay-announced:{}", channel.id), &key)
        })?;
    }
    for _ in 0..5 {
        let mut batch = (0_i64, Vec::new());
        with_store(&mut |store| {
            batch = store.unshared_usage(&channel.id, 200)?;
            Ok(())
        })?;
        if batch.1.is_empty() {
            break;
        }
        let rows: Vec<Value> = batch
            .1
            .iter()
            .map(|(account, key, at, request)| wire(account, key, *at, request, &channel.device))
            .collect();
        let full = rows.len() == 200;
        m_dash_sync::usage_request(
            &channel.endpoint,
            &format!("/v1/usage/{}/rows", channel.id),
            Method::POST,
            &channel.ingest,
            Some(json!({ "rows": rows })),
        )
        .await?;
        with_store(&mut |store| store.mark_shared(&channel.id, batch.0))?;
        if !full {
            break;
        }
    }
    for _ in 0..5 {
        let mut after = 0;
        with_store(&mut |store| {
            after = store.relay_cursor(&channel.id)?;
            Ok(())
        })?;
        let response = m_dash_sync::usage_request(
            &channel.endpoint,
            &format!("/v1/usage/{}/rows/{after}", channel.id),
            Method::GET,
            &channel.read,
            None,
        )
        .await?;
        let rows = response["rows"].as_array().cloned().unwrap_or_default();
        let last = rows
            .iter()
            .filter_map(|row| row["seq"].as_i64())
            .max()
            .unwrap_or(after);
        let parsed: Vec<_> = rows
            .iter()
            .filter_map(|row| unwire(row, &channel.device))
            .collect();
        let origins: Vec<(String, String, String)> = rows
            .iter()
            .filter(|row| unwire(row, &channel.device).is_some())
            .filter_map(|row| {
                Some((
                    row["account"].as_str()?.to_owned(),
                    row["key"].as_str()?.to_owned(),
                    row["origin"].as_str()?.to_owned(),
                ))
            })
            .collect();
        let devices: Vec<Announced> = rows
            .iter()
            .filter_map(|row| announced(row, &channel.device))
            .collect();
        with_store(&mut |store| {
            store.ingest_relay(&channel.id, &parsed, last)?;
            store.relay_details(&origins, &devices)
        })?;
        if rows.len() < 200 {
            break;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> Channel {
        Channel {
            endpoint: "https://relay.example.workers.dev".into(),
            id: "i".repeat(43),
            ingest: "w".repeat(43),
            read: "r".repeat(43),
            device: "d".repeat(43),
        }
    }

    #[test]
    fn join_codes_round_trip_without_the_creating_device() {
        let code = channel().code();
        assert!(!code.contains(&"d".repeat(43)));
        let joined = Channel::from_code(&code).unwrap();
        assert_eq!((&joined.id, &joined.read), (&channel().id, &channel().read));
        assert_ne!(joined.device, channel().device);
        assert!(Channel::from_code("mdash-usage1:bad").is_err());
        let mut same = channel();
        same.read = same.ingest.clone();
        assert!(Channel::from_code(&same.code()).is_err());
        let script = channel().cloud_setup_script();
        assert!(script.contains(&format!(
            "MDASH_ENDPOINT='{}/v1/usage/{}/{}/logs'",
            channel().endpoint,
            "i".repeat(43),
            "w".repeat(43)
        )));
        assert!(!script.contains('@'));
        assert!(script.contains("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT: process.env.MDASH_ENDPOINT"));
    }

    #[test]
    fn rows_round_trip_and_this_computers_rows_are_skipped() {
        let request = crate::usage::Request {
            surface: "cli".into(),
            model: Some("m".into()),
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cost_micros: Some(5),
            ..Default::default()
        };
        let mut row = wire("claude:[\"a\",\"o\"]", "claude:r1", 10, &request, "other");
        assert_eq!(row["provider"], "claude");
        row["seq"] = json!(7);
        let (seq, (account, key, at, parsed)) = unwire(&row, "mine").unwrap();
        assert_eq!(
            (seq, account.as_str(), key.as_str(), at),
            (7, "claude:[\"a\",\"o\"]", "claude:r1", 10)
        );
        assert_eq!(parsed, request);
        assert!(unwire(&row, "other").is_none());
        row["origin"] = json!("cloud");
        assert_eq!(unwire(&row, "mine").unwrap().1 .3.surface, "cloud");
        row["input"] = json!(-1);
        assert!(unwire(&row, "mine").is_none());
    }

    #[test]
    fn announcements_name_other_computers_and_are_not_usage() {
        let me = Device {
            name: "Studio Mac".into(),
            details: "m-dash 0.6.0;logs=1;feed=0".into(),
        };
        let row = announcement(&me, "other", 86_400 * 3 + 5);
        assert!(row["key"].as_str().unwrap().starts_with("device:3:"));
        assert!(unwire(&row, "mine").is_none());
        assert!(announced(&row, "other").is_none());
        assert_eq!(
            announced(&row, "mine"),
            Some(Announced {
                origin: "other".into(),
                name: "Studio Mac".into(),
                details: "m-dash 0.6.0;logs=1;feed=0".into(),
                at: 86_400 * 3 + 5
            })
        );
        let renamed = Device {
            name: "Office".into(),
            ..me
        };
        assert_ne!(
            announcement(&renamed, "other", 86_400 * 3 + 5)["key"],
            row["key"]
        );
    }
}
