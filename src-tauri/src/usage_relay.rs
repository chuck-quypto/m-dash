//! Optional usage channel on the owner's relay. Each computer sends its per-request accounting
//! rows and receives the others'. Claude Code cloud sessions send telemetry through a loopback
//! forwarder so the burn ledger can include requests made elsewhere. The relay keeps only
//! accounting fields.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::Future;

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
    fn validate(mut self) -> Result<Self> {
        self.endpoint = m_dash_sync::endpoint(&self.endpoint)?;
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

    /// Installs a loopback forwarder so cloud sessions can share usage without sending their
    /// exporter credentials to this channel.
    pub fn cloud_setup_script(&self) -> String {
        let destination = format!(
            "{}/v1/usage/{}/{}/logs",
            self.endpoint, self.id, self.ingest
        );
        CLOUD_SETUP
            .replace("@ENDPOINT@", &URL_SAFE_NO_PAD.encode(destination))
            .replace(
                "@FORWARDER@",
                &serde_json::to_string(include_str!("cloud_usage_forwarder.cjs")).unwrap(),
            )
    }
}

const CLOUD_SETUP: &str = r#"# m-dash: send Claude Code usage from cloud sessions to your relay.
mkdir -p "$HOME/.claude"
MDASH_ENDPOINT='@ENDPOINT@' node <<'MDASH_SETUP'
const fs = require("fs"), join = require("path").join, crypto = require("crypto");
const directory = join(process.env.HOME, ".claude"), path = join(directory, "settings.json");
let settings = {};
try {
  settings = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(fs.readFileSync(path)));
} catch (error) {
  if (error.code !== "ENOENT") throw new Error("Cannot read Claude settings. Cloud setup stopped.");
}
const isObject = value => value !== null && typeof value === "object" && !Array.isArray(value);
if (!isObject(settings) || ("env" in settings && !isObject(settings.env))) {
  throw new Error("Invalid Claude settings. Cloud setup stopped.");
}
const hasHeaders = env => Object.keys(env).some(key => key.startsWith("OTEL_EXPORTER_OTLP_") && key.endsWith("_HEADERS"));
if ("otelHeadersHelper" in settings || hasHeaders(settings.env ?? {}) || hasHeaders(process.env)) {
  throw new Error("Claude already has OTLP exporter headers. Its settings were preserved.");
}
if (("hooks" in settings && !isObject(settings.hooks)) ||
    (settings.hooks && "SessionStart" in settings.hooks && !Array.isArray(settings.hooks.SessionStart))) {
  throw new Error("Invalid Claude hooks. Cloud setup stopped.");
}
const command = 'node "$HOME/.claude/m-dash-usage-relay.cjs" </dev/null >/dev/null 2>&1 &';
const starts = settings.hooks?.SessionStart ?? [];
if (!starts.some(group => !group?.matcher && Array.isArray(group?.hooks) &&
    group.hooks.some(h => h?.type === "command" && h.command === command))) {
  settings.hooks = { ...settings.hooks, SessionStart: [...starts, { hooks: [{ type: "command", command }] }] };
}
const exporter = { OTEL_EXPORTER_OTLP_LOGS_ENDPOINT: Buffer.from(process.env.MDASH_ENDPOINT, "base64url").toString("utf8") };
function write(file, data) {
  const temporary = file + "." + crypto.randomUUID();
  fs.writeFileSync(temporary, data, { flag: "wx", mode: 0o600 });
  try { fs.renameSync(temporary, file); }
  finally { fs.rmSync(temporary, { force: true }); }
}
const socket = require("net").createServer();
socket.listen(0, "127.0.0.1", () => {
  const listen = "http://127.0.0.1:" + socket.address().port + "/" + crypto.randomUUID() + "/logs";
  socket.close();
  write(join(directory, "m-dash-usage-relay.cjs"), @FORWARDER@);
  write(join(directory, "m-dash-usage-relay.json"), JSON.stringify({ listen, env: exporter }));
  settings.env = { ...settings.env, CLAUDE_CODE_ENABLE_TELEMETRY: "1", OTEL_LOGS_EXPORTER: "otlp",
    OTEL_EXPORTER_OTLP_LOGS_PROTOCOL: "http/json", OTEL_EXPORTER_OTLP_LOGS_ENDPOINT: listen };
  write(path, JSON.stringify(settings, null, 2) + "\n");
});
MDASH_SETUP
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
            .filter(|value| !value.is_empty() && value.encode_utf16().count() <= 300)
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
/// Returns whether a channel is set up and any warning from the exchange.
pub async fn sync(
    with_store: impl Fn(&mut dyn FnMut(&mut crate::store::Store) -> Result<()>) -> Result<()>,
    me: Device,
) -> Result<(bool, Option<String>)> {
    let Some(ref channel) = load()? else {
        return Ok((false, None));
    };
    sync_channel(
        channel,
        announcement(&me, &channel.device, crate::collector::now() as i64),
        with_store,
        |rows| m_dash_sync::post_usage_rows(&channel.endpoint, &channel.id, &channel.ingest, rows),
        |after| async move {
            m_dash_sync::usage_request(
                &channel.endpoint,
                &format!("/v1/usage/{}/rows/{after}", channel.id),
                Method::GET,
                &channel.read,
                None,
            )
            .await
        },
    )
    .await
}

async fn upload_pending<P>(
    channel: &Channel,
    row: Value,
    mut with_store: impl FnMut(&mut dyn FnMut(&mut crate::store::Store) -> Result<()>) -> Result<()>,
    post: impl Fn(Vec<Value>) -> P,
) -> Result<bool>
where
    P: Future<Output = Result<bool>>,
{
    let key = row["key"].as_str().unwrap_or_default().to_owned();
    let announced_key = format!("usage-relay-announced:{}", channel.id);
    let mut sent = String::new();
    with_store(&mut |store| {
        sent = store.setting(&announced_key)?.unwrap_or_default();
        Ok(())
    })?;
    if sent != key {
        if !post(vec![row]).await? {
            return Ok(false);
        }
        with_store(&mut |store| store.save_setting(&announced_key, &key))?;
    }
    for _ in 0..5 {
        let mut page = (0_i64, Vec::new());
        with_store(&mut |store| {
            page = store.unshared_usage(&channel.id, &channel.device, 200)?;
            Ok(())
        })?;
        let (through, rows) = page;
        if rows.is_empty() {
            break;
        }
        if !post(rows).await? {
            return Ok(false);
        }
        with_store(&mut |store| store.mark_shared(&channel.id, through))?;
    }
    Ok(true)
}

async fn sync_channel<P, G>(
    channel: &Channel,
    announcement: Value,
    mut with_store: impl FnMut(&mut dyn FnMut(&mut crate::store::Store) -> Result<()>) -> Result<()>,
    post: impl Fn(Vec<Value>) -> P,
    get: impl Fn(i64) -> G,
) -> Result<(bool, Option<String>)>
where
    P: Future<Output = Result<bool>>,
    G: Future<Output = Result<Value>>,
{
    let uploads_accepted = upload_pending(channel, announcement, &mut with_store, post).await?;
    for _ in 0..5 {
        let mut after = 0;
        with_store(&mut |store| {
            after = store.relay_cursor(&channel.id)?;
            Ok(())
        })?;
        let response = get(after).await?;
        let rows = response["rows"]
            .as_array()
            .ok_or("Invalid usage relay page.")?;
        let Some(last) = rows.last() else {
            break;
        };
        let through = last["seq"]
            .as_i64()
            .filter(|seq| *seq > after)
            .ok_or("Invalid usage relay cursor.")?;
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
            store.ingest_relay(&channel.id, &parsed, &origins, &devices, through)
        })?;
    }
    let warning = (!uploads_accepted)
        .then(|| "The usage channel is full. Pending uploads will retry on the next sync.".into());
    Ok((true, warning))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Run the Node program from the generated here-document on Windows as well as Unix.
    fn run_setup(
        script: &str,
        home: &std::path::Path,
        header: Option<&str>,
    ) -> std::process::Output {
        let (_, command) = script.split_once("MDASH_ENDPOINT='").unwrap();
        let (encoded, program) = command.split_once("' node <<'MDASH_SETUP'\n").unwrap();
        let program = program.strip_suffix("\nMDASH_SETUP\n").unwrap();
        let mut node = std::process::Command::new("node");
        node.env_clear()
            .env("HOME", home)
            .env("MDASH_ENDPOINT", encoded);
        #[cfg(windows)]
        node.env("SystemRoot", std::env::var_os("SystemRoot").unwrap());
        if let Some(key) = header {
            node.env(key, "synthetic-header");
        }
        node.args(["-e", program]).output().unwrap()
    }

    fn channel() -> Channel {
        Channel {
            endpoint: "https://relay.example.workers.dev".into(),
            id: "i".repeat(43),
            ingest: "w".repeat(43),
            read: "r".repeat(43),
            device: "d".repeat(43),
        }
    }

    #[tokio::test]
    async fn full_channel_downloads_retained_usage_and_retries_only_pending_writes() {
        use crate::store::Store;
        use std::cell::Cell;

        let channel = channel();
        let now = crate::collector::now();
        let announcement = announcement(
            &Device {
                name: "Desk".into(),
                details: "test".into(),
            },
            &channel.device,
            now as i64,
        );
        let usage = crate::usage::Request {
            surface: "cli".into(),
            input: 7,
            ..Default::default()
        };
        let mut store = Store::open(std::path::Path::new(":memory:")).unwrap();
        store
            .ingest_local_logs(
                "codex",
                "codex:here",
                crate::local_logs::Scan {
                    requests: (1..=201)
                        .map(|i| (format!("local-{i}"), now as i64, usage.clone()))
                        .collect(),
                    ..Default::default()
                },
            )
            .unwrap();
        let pending = store
            .unshared_usage(&channel.id, &channel.device, 200)
            .unwrap();
        assert_eq!(pending.1[0]["key"], "local-1");
        assert_eq!(pending.0, 200);
        let announced_key = format!("usage-relay-announced:{}", channel.id);
        let progress_key = format!("usage-relay:{}", channel.id);
        let free_rows = Cell::new(0);
        let post = async |rows: Vec<Value>| {
            if rows.len() > free_rows.get() {
                return Ok(false);
            }
            free_rows.set(free_rows.get() - rows.len());
            Ok(true)
        };
        // Generate retained rows by sequence so the fixture needn't allocate the whole channel.
        let get = async |after: i64| {
            let rows = (after + 1..=(after + 200).min(200_000))
                .map(|seq| {
                    let mut row = wire(
                        "codex:other",
                        &format!("remote-{seq}"),
                        now as i64,
                        &usage,
                        "cloud",
                    );
                    row["seq"] = json!(seq);
                    row
                })
                .collect::<Vec<_>>();
            Ok(json!({ "rows": rows }))
        };

        // Free space for the announcement, then one batch. Stay full for a pass before the last row.
        let passes = [(0, 0), (1, 0), (200, 200), (0, 200), (1, 201)];
        for (pass, (slots, pushed)) in passes.into_iter().enumerate() {
            free_rows.set(slots);
            let (connected, warning) = sync_channel(
                &channel,
                announcement.clone(),
                |apply| apply(&mut store),
                &post,
                &get,
            )
            .await
            .unwrap();
            assert!(connected);
            let through = (pass + 1) * 1000;
            assert_eq!(store.relay_cursor(&channel.id).unwrap(), through as i64);
            let list = store
                .usage_list("codex:other", &Default::default(), now, false)
                .unwrap();
            assert_eq!(list["total"], through);
            assert_eq!(list["rows"][0]["input"], 7);
            assert_eq!(
                store.setting(&announced_key).unwrap().as_deref(),
                (pass > 0).then(|| announcement["key"].as_str().unwrap())
            );
            let progress: Value =
                serde_json::from_str(&store.setting(&progress_key).unwrap().unwrap()).unwrap();
            let next = store
                .unshared_usage(&channel.id, &channel.device, 200)
                .unwrap();
            assert_eq!(progress["pushed"], pushed);
            if pushed < 201 {
                assert!(warning.unwrap().contains("usage channel is full"));
                if pushed == 0 {
                    assert_eq!(next, pending);
                } else {
                    assert_eq!(next.0, 201);
                    assert_eq!(
                        next.1,
                        [wire(
                            "codex:here",
                            "local-201",
                            now as i64,
                            &usage,
                            &channel.device
                        )]
                    );
                }
            } else {
                assert!(warning.is_none());
                assert!(next.1.is_empty());
            }
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
            "MDASH_ENDPOINT='{}'",
            URL_SAFE_NO_PAD.encode(format!(
                "{}/v1/usage/{}/{}/logs",
                channel().endpoint,
                "i".repeat(43),
                "w".repeat(43)
            ))
        )));
        assert!(!script.contains('@'));
        assert!(script.contains(r#"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT: Buffer.from(process.env.MDASH_ENDPOINT, "base64url").toString("utf8")"#));
    }

    #[test]
    fn a_joined_origin_is_canonical_and_shell_punctuation_is_only_data() {
        let mut source = channel();
        source.endpoint = "HTTPS://Relay&Archive.Example:443/'notes'/../".into();
        let joined = Channel::from_code(&source.code()).unwrap();
        assert_eq!(joined.endpoint, "https://relay&archive.example");
        assert_eq!(
            Channel::from_code(&joined.code()).unwrap().endpoint,
            joined.endpoint
        );
        let script = joined.cloud_setup_script();
        assert!(!script.contains(&joined.endpoint));
        assert!(!script.contains("notes"));
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".claude");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("settings.json");
        let existing_hook =
            json!({"hooks":[{"type":"command","command":"synthetic-existing-hook"}]});
        std::fs::write(
            &path,
            json!({"env":{"KEEP":"original"},"hooks":{"SessionStart":[existing_hook]}}).to_string(),
        )
        .unwrap();
        assert!(run_setup(&script, home.path(), None).status.success());
        let output = run_setup(&script, home.path(), None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let settings: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(settings["env"]["KEEP"], "original");
        let hooks = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0], existing_hook);
        assert!(!settings.to_string().contains(&joined.ingest));
        let exporter: Value = serde_json::from_slice(
            &std::fs::read(directory.join("m-dash-usage-relay.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"],
            exporter["listen"]
        );
        assert!(exporter["listen"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:"));
        assert_eq!(
            exporter["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"],
            format!(
                "https://relay&archive.example/v1/usage/{}/{}/logs",
                joined.id, joined.ingest
            )
        );
    }

    #[test]
    fn cloud_session_headers_never_reach_the_relay() {
        use axum::{http::HeaderMap, routing::post, Router};
        use tokio::{net::TcpListener, sync::mpsc};

        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".claude")).unwrap();
        tauri::async_runtime::block_on(async {
            let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut channel = channel();
            channel.endpoint = format!("http://{}", socket.local_addr().unwrap());
            let route = format!("/v1/usage/{}/{}/logs", channel.id, channel.ingest);
            let (tx, mut received) = mpsc::unbounded_channel();
            let relay = Router::new().route(
                &route,
                post(move |headers: HeaderMap, body: String| {
                    tx.send((headers, body)).unwrap();
                    async { "{}" }
                }),
            );
            let server = tokio::spawn(async move { axum::serve(socket, relay).await.unwrap() });
            let setup = run_setup(&channel.cloud_setup_script(), home.path(), None);
            assert!(
                setup.status.success(),
                "{}",
                String::from_utf8_lossy(&setup.stderr)
            );

            for (generic, logs) in [(false, false), (true, false), (false, true), (true, true)] {
                let mut session = tokio::process::Command::new("node");
                session.env_clear().env("HOME", home.path());
                #[cfg(windows)]
                session.env("SystemRoot", std::env::var_os("SystemRoot").unwrap());
                if generic {
                    session.env(
                        "OTEL_EXPORTER_OTLP_HEADERS",
                        "Authorization=synthetic-generic",
                    );
                }
                if logs {
                    session.env(
                        "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
                        "X-Collector-Key=synthetic-logs",
                    );
                }
                let output = session
                    .arg(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/cloud-relay-session.cjs"
                    ))
                    .output()
                    .await
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let (headers, body) = received.try_recv().unwrap();
                assert_eq!(body, r#"{"resourceLogs":[]}"#);
                assert_eq!(headers["content-type"], "application/json");
                assert!(!headers.contains_key("authorization"));
                assert!(!headers.contains_key("x-collector-key"));
            }
            server.abort();
        });
    }

    #[test]
    fn cloud_setup_preserves_authenticated_exporters() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".claude");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("settings.json");
        let script = channel().cloud_setup_script();
        for key in [
            "OTEL_EXPORTER_OTLP_HEADERS",
            "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
        ] {
            for inherited in [false, true] {
                let mut settings = json!({"env":{"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":"https://existing.example/logs"}});
                if !inherited {
                    settings["env"][key] = json!("synthetic-header");
                }
                let original = settings.to_string();
                std::fs::write(&path, &original).unwrap();
                let output = run_setup(&script, home.path(), inherited.then_some(key));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
                assert!(!output.status.success());
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    stderr.contains("Claude already has OTLP exporter headers"),
                    "{stderr}"
                );
                assert!(!stderr.contains("synthetic-header"));
            }
        }
    }

    #[test]
    fn cloud_setup_preserves_helper_and_unreadable_settings() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".claude");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("settings.json");
        for (original, reason) in [
            (
                r#"{"otelHeadersHelper":"synthetic-helper"}"#,
                "Claude already has OTLP exporter headers",
            ),
            (r#"{"env": "#, "Cannot read Claude settings"),
            (r#"{"env": []}"#, "Invalid Claude settings"),
            (r#"{"hooks":{"SessionStart":{}}}"#, "Invalid Claude hooks"),
        ] {
            std::fs::write(&path, original).unwrap();
            let output = run_setup(&channel().cloud_setup_script(), home.path(), None);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains(reason));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        let invalid_utf8 = b"{\"name\": \"\xff\"}\r\n";
        std::fs::write(&path, invalid_utf8).unwrap();
        let output = run_setup(&channel().cloud_setup_script(), home.path(), None);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Cannot read Claude settings"));
        assert_eq!(std::fs::read(&path).unwrap(), invalid_utf8);
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
    fn download_accepts_unicode_account_and_key_at_relay_limits() {
        let account = format!("claude:{}", "測".repeat(293));
        let key = "🦀".repeat(150);
        let request = crate::usage::Request {
            surface: "cli".into(),
            ..Default::default()
        };
        let mut row = wire(&account, &key, 10, &request, "other");
        row["seq"] = json!(17);
        assert_eq!(
            unwire(&row, "mine"),
            Some((17, (account, key.clone(), 10, request)))
        );
        row["key"] = json!(key + "a");
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
