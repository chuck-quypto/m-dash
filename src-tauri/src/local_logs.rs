//! Optional reader for the session logs that Codex and Claude Code keep on this computer.
//! It extracts only per-request accounting fields and Codex allowance snapshots; message,
//! tool and file content is never retained. Files are read incrementally from a saved byte
//! offset, so a pass costs only what was appended since the last one.
use crate::usage::Request;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Bytes read per pass across all files; a large backlog continues on later passes.
const BUDGET: u64 = 32 * 1024 * 1024;
/// Lines longer than this hold content, never accounting data, and are skipped unparsed.
const LINE_LIMIT: usize = 4 * 1024 * 1024;
/// Files untouched for longer than this are not back-filled.
const HORIZON: u64 = 90 * 86_400;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Codex,
    Claude,
}

/// Where a file was left and the running state needed to continue it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cursor {
    pub offset: u64,
    pub state: Value,
}

#[derive(Debug, Default)]
pub struct Scan {
    /// (dedupe key, time, request)
    pub requests: Vec<(String, i64, Request)>,
    /// (window id, time, used percent, reset time)
    pub readings: Vec<(String, i64, f64, Option<i64>)>,
    /// (path, cursor) for every file read in this pass.
    pub cursors: Vec<(String, Cursor)>,
}

/// The log directory for a provider, honouring the CLIs' own overrides.
pub fn root(kind: Kind) -> Option<PathBuf> {
    let home =
        || std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    match kind {
        Kind::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|home| home.join(".codex")))
            .map(|base| base.join("sessions")),
        Kind::Claude => std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| home().map(|home| home.join(".claude")))
            .map(|base| base.join("projects")),
    }
}

fn files(directory: &Path, depth: u8, now: u64, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() && depth > 0 {
            files(&path, depth - 1, now, found);
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
            let recent = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|at| at.as_secs() + HORIZON >= now);
            if recent {
                found.push(path);
            }
        }
    }
}

/// Reads what was appended to each log since its saved cursor, within the pass budget.
pub fn scan(
    kind: Kind,
    root: &Path,
    cursors: &std::collections::HashMap<String, Cursor>,
    now: u64,
) -> Scan {
    let mut paths = Vec::new();
    files(root, 6, now, &mut paths);
    paths.sort();
    let mut result = Scan::default();
    let mut budget = BUDGET;
    for path in paths {
        if budget == 0 {
            break;
        }
        let name = path.to_string_lossy().into_owned();
        let mut cursor = cursors.get(&name).cloned().unwrap_or_default();
        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        if length < cursor.offset {
            // Rewritten or truncated: start over; request keys keep replays from counting twice.
            cursor = Cursor::default();
        }
        if length == cursor.offset || file.seek(SeekFrom::Start(cursor.offset)).is_err() {
            continue;
        }
        let mut reader = std::io::BufReader::new((&mut file).take(budget));
        let mut line = Vec::new();
        loop {
            line.clear();
            let Ok(size) = reader.read_until(b'\n', &mut line) else {
                break;
            };
            // Stop at a partial last line; it is read whole on a later pass.
            if size == 0 || line.last() != Some(&b'\n') {
                break;
            }
            cursor.offset += size as u64;
            budget = budget.saturating_sub(size as u64);
            if line.len() <= LINE_LIMIT {
                match kind {
                    Kind::Codex => codex_line(&line, &mut cursor.state, &mut result),
                    Kind::Claude => claude_line(&line, &mut result),
                }
            }
        }
        result.cursors.push((name, cursor));
    }
    result
}

fn timestamp(value: &Value) -> Option<i64> {
    value
        .as_str()
        .and_then(|text| {
            time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()
        })
        .map(|at| at.unix_timestamp())
        .filter(|at| *at >= 0)
}

fn label(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 128)
        .map(str::to_owned)
}

fn amount(value: &Value) -> i64 {
    value
        .as_i64()
        .filter(|amount| *amount >= 0)
        .unwrap_or_default()
}

fn contains(line: &[u8], needle: &[u8]) -> bool {
    line.windows(needle.len()).any(|window| window == needle)
}

fn codex_line(line: &[u8], state: &mut Value, result: &mut Scan) {
    if !(contains(line, b"token_count")
        || contains(line, b"turn_context")
        || contains(line, b"session_meta"))
    {
        return;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let payload = &value["payload"];
    match value["type"].as_str() {
        Some("session_meta") => {
            if let Some(id) = label(&payload["id"]) {
                state["session"] = json!(id);
            }
        }
        Some("turn_context") => {
            state["model"] = payload["model"].clone();
            state["effort"] = payload["effort"].clone();
        }
        Some("event_msg") if payload["type"] == "token_count" => {
            let Some(at) = timestamp(&value["timestamp"]) else {
                return;
            };
            let total = &payload["info"]["total_token_usage"];
            let fields = [
                "input_tokens",
                "cached_input_tokens",
                "output_tokens",
                "reasoning_output_tokens",
            ];
            if total.is_object() {
                let now: Vec<i64> = fields.iter().map(|field| amount(&total[field])).collect();
                let before: Vec<i64> = fields
                    .iter()
                    .map(|field| amount(&state["total"][field]))
                    .collect();
                let delta: Vec<i64> = now
                    .iter()
                    .zip(&before)
                    .map(|(now, before)| (now - before).max(0))
                    .collect();
                if delta.iter().any(|amount| *amount > 0) {
                    // Codex counts cached input inside input, and reasoning inside output.
                    let cached = delta[1].min(delta[0]);
                    let session = state["session"].as_str().unwrap_or("unknown");
                    let key = format!("codex:{session}:{}", now.iter().sum::<i64>());
                    result.requests.push((
                        key,
                        at,
                        Request {
                            surface: "codex".into(),
                            model: label(&state["model"]),
                            effort: label(&state["effort"]),
                            input: delta[0] - cached,
                            cache_read: cached,
                            output: delta[2],
                            reasoning: Some(delta[3]),
                            ..Default::default()
                        },
                    ));
                }
                state["total"] = total.clone();
            }
            let limits = &payload["rate_limits"];
            let id = label(&limits["limit_id"]).unwrap_or_else(|| "codex".into());
            for kind in ["primary", "secondary"] {
                let window = &limits[kind];
                let Some(used) = window["used_percent"]
                    .as_f64()
                    .filter(|used| used.is_finite() && *used >= 0.0)
                else {
                    continue;
                };
                let reset = window["resets_at"].as_i64().or_else(|| {
                    window["resets_in_seconds"]
                        .as_i64()
                        .map(|seconds| at + seconds)
                });
                result
                    .readings
                    .push((format!("{id}/{kind}"), at, used, reset));
            }
        }
        _ => {}
    }
}

fn claude_line(line: &[u8], result: &mut Scan) {
    if !contains(line, b"\"usage\"") || !contains(line, b"\"assistant\"") {
        return;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let message = &value["message"];
    let usage = &message["usage"];
    if value["type"] != "assistant" || !usage.is_object() {
        return;
    }
    let (Some(at), Some(request)) = (
        timestamp(&value["timestamp"]),
        label(&value["requestId"]).or_else(|| label(&message["id"])),
    ) else {
        return;
    };
    let model = label(&message["model"]).filter(|model| !model.starts_with('<'));
    if model.is_none() {
        return;
    }
    let sidechain = value["isSidechain"] == true;
    result.requests.push((
        format!("claude:{request}"),
        at,
        Request {
            surface: "cli".into(),
            model,
            query_source: Some(if sidechain { "subagent" } else { "main" }.into()),
            input: amount(&usage["input_tokens"]),
            output: amount(&usage["output_tokens"]),
            cache_read: amount(&usage["cache_read_input_tokens"]),
            cache_write: amount(&usage["cache_creation_input_tokens"]),
            ..Default::default()
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn write(path: &Path, lines: &[Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn codex_rollouts_yield_turn_deltas_and_allowance_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("2026/10/30/rollout-a.jsonl");
        let usage = |input: i64, cached: i64, output: i64| {
            json!({"timestamp":"2026-10-30T00:00:10Z","type":"event_msg","payload":{"type":"token_count",
                "info":{"total_token_usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output,"reasoning_output_tokens":5}},
                "rate_limits":{"primary":{"used_percent":12.0,"window_minutes":300,"resets_in_seconds":600},
                    "secondary":{"used_percent":40.0,"window_minutes":10080,"resets_at":1800000000}}}})
        };
        write(
            &file,
            &[
                json!({"type":"session_meta","payload":{"id":"s1","instructions":"private"}}),
                json!({"type":"turn_context","payload":{"model":"gpt-5.6-sol","effort":"high"}}),
                json!({"type":"response_item","payload":{"content":"private token_count"}}),
                usage(1000, 800, 50),
                usage(1000, 800, 50),
                usage(1500, 1200, 80),
            ],
        );
        let now = crate::collector::now();
        let first = scan(Kind::Codex, directory.path(), &HashMap::new(), now);
        assert_eq!(first.requests.len(), 2);
        let (key, at, request) = &first.requests[1];
        assert_eq!(key, "codex:s1:2785");
        assert_eq!(*at, 1_793_318_410);
        assert_eq!(
            (request.input, request.cache_read, request.output),
            (100, 400, 30)
        );
        assert_eq!(request.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(
            first.readings[0],
            (
                "codex/primary".into(),
                1_793_318_410,
                12.0,
                Some(1_793_319_010)
            )
        );
        assert_eq!(first.readings.len(), 6);
        // Resuming from the saved cursor reads only appended lines.
        let cursors: HashMap<_, _> = first.cursors.into_iter().collect();
        let again = scan(Kind::Codex, directory.path(), &cursors, now);
        assert!(again.requests.is_empty() && again.readings.is_empty());
        let mut text = std::fs::read_to_string(&file).unwrap();
        text.push_str(&format!("{}\n{{\"partial", usage(1600, 1200, 90)));
        std::fs::write(&file, text).unwrap();
        let more = scan(Kind::Codex, directory.path(), &cursors, now);
        assert_eq!(more.requests.len(), 1);
        assert_eq!(more.requests[0].2.input, 100);
        assert!(!format!("{:?}", more.requests).contains("private"));
    }

    #[test]
    fn claude_transcripts_yield_one_request_per_api_call_without_content() {
        let directory = tempfile::tempdir().unwrap();
        let usage = json!({"input_tokens":3,"output_tokens":7,"cache_read_input_tokens":900,"cache_creation_input_tokens":60});
        let assistant = |request: &str, sidechain: bool, model: &str| {
            json!({"type":"assistant","timestamp":"2026-10-30T00:00:10.5Z","requestId":request,"isSidechain":sidechain,
                "message":{"model":model,"usage":usage,"content":[{"type":"text","text":"private answer"}]}})
        };
        write(
            &directory.path().join("project/session.jsonl"),
            &[
                json!({"type":"user","message":{"content":"private assistant usage"}}),
                assistant("r1", false, "claude-opus-5-5"),
                assistant("r1", false, "claude-opus-5-5"),
                assistant("r2", true, "claude-haiku-5-5"),
                assistant("r3", false, "<synthetic>"),
            ],
        );
        let found = scan(
            Kind::Claude,
            directory.path(),
            &HashMap::new(),
            crate::collector::now(),
        );
        let keys: Vec<&str> = found.requests.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(keys, ["claude:r1", "claude:r1", "claude:r2"]);
        let request = &found.requests[2].2;
        assert_eq!(request.query_source.as_deref(), Some("subagent"));
        assert_eq!(
            (
                request.input,
                request.output,
                request.cache_read,
                request.cache_write
            ),
            (3, 7, 900, 60)
        );
        assert!(!format!("{:?}", found.requests).contains("private"));
    }
}
