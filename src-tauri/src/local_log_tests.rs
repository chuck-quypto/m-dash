use super::*;
use local_logs::Kind;
use serde_json::{json, Value};
use std::io::Write;

#[test]
fn failed_refresh_defers_local_rows_and_cursors_until_the_new_account_is_verified() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    let old_codex = json!({"accountId":"codex-old"});
    let old_claude = json!({"accountId":"claude:old"});
    let state = AppState(Mutex::new(Inner {
        dashboard: Dashboard {
            snapshot: Some(old_codex.clone()),
            claude: Some(old_claude.clone()),
            settings: Settings {
                providers: Providers {
                    codex: true,
                    claude: true,
                    ..Default::default()
                },
                local_logs: true,
                ..Default::default()
            },
            ..Default::default()
        },
        store: store::Store::open(&path.join("usage.sqlite")).unwrap(),
        codex_schedule: Default::default(),
        grok_schedule: Default::default(),
        claude_schedule: Default::default(),
        openrouter_schedule: Default::default(),
        antigravity_schedule: Default::default(),
        openrouter_setup: false,
        openrouter_generation: 0,
        openrouter_retry_after: 0,
        directory: path.to_owned(),
        claude_receiver_ready: false,
        settings_generation: 0,
        log_scan_generation: 0,
        ping: Default::default(),
        ping_generation: 0,
        ping_running: false,
        last_feed_forecast: 0,
        last_prune: 0,
    }));
    let root = |kind| {
        Some(path.join(match kind {
            Kind::Codex => "sessions",
            Kind::Claude => "projects",
            Kind::AntigravityCli => "agy-cli",
            Kind::AntigravityDesktop => "agy-desktop",
            Kind::AntigravityIde => "agy-ide",
        }))
    };
    let import = |codex: Result<Value, String>, claude: Result<Value, String>| {
        {
            let mut inner = state.0.lock().unwrap();
            if let Ok(snapshot) = &codex {
                inner.dashboard.snapshot = Some(snapshot.clone());
            }
            if let Ok(snapshot) = &claude {
                inner.dashboard.claude = Some(snapshot.clone());
            }
        }
        for (kind, reading) in [(Kind::Codex, codex), (Kind::Claude, claude)] {
            let confirmed = refreshed_account(&reading);
            tauri::async_runtime::block_on(read_local_logs(
                &state,
                kind,
                0,
                confirmed.as_deref(),
                || root(kind),
                std::future::ready(reading),
            ));
        }
    };
    let files = [
        (Kind::Codex, "codex_rollout"),
        (Kind::Claude, "claude_transcript"),
    ];
    let append = |n| {
        for (kind, _) in files {
            let dir = root(kind).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let row = match kind {
                Kind::Codex => json!({"timestamp":"2026-10-30T00:00:10Z","type":"event_msg",
                    "payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":n}}}}),
                Kind::Claude => {
                    json!({"timestamp":"2026-10-30T00:00:10Z","type":"assistant","requestId":format!("request-{n}"),
                    "message":{"model":"synthetic","usage":{"input_tokens":n}}})
                }
                Kind::AntigravityCli | Kind::AntigravityDesktop | Kind::AntigravityIde => {
                    json!({"step_index":n,"type":"PLANNER_RESPONSE","status":"DONE",
                    "created_at":"2026-10-30T00:00:10Z","model":"synthetic","input_tokens":n,"output_tokens":1})
                }
            };
            writeln!(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("session.jsonl"))
                    .unwrap(),
                "{row}"
            )
            .unwrap();
        }
    };
    let cursors =
        || files.map(|(_, provider)| state.0.lock().unwrap().store.log_cursors(provider).unwrap());
    let counts = || {
        ["codex-old", "claude:old", "codex-new", "claude:new"].map(|account| {
            state
                .0
                .lock()
                .unwrap()
                .store
                .usage_list(account, &Default::default(), 1_793_318_410, false)
                .unwrap()["total"]
                .as_u64()
                .unwrap()
        })
    };

    append(1);
    import(Ok(old_codex.clone()), Ok(old_claude.clone()));
    let saved = cursors();
    assert_eq!(counts(), [1, 1, 0, 0]);
    let before_switch = [
        refreshed_account(&Ok(old_codex)),
        refreshed_account(&Ok(old_claude)),
    ];
    // Both reads succeeded under the old sign-ins. The new accounts now append their rows.
    append(2);
    for signed_in in [[Some("codex-new"), Some("claude:new")], [None, None]] {
        for ((kind, before), current) in [
            (Kind::Codex, &before_switch[0]),
            (Kind::Claude, &before_switch[1]),
        ]
        .into_iter()
        .zip(signed_in)
        {
            let result = current
                .map(|id| json!({"accountId": id}))
                .ok_or_else(|| "signed out".to_owned());
            tauri::async_runtime::block_on(read_local_logs(
                &state,
                kind,
                0,
                before.as_deref(),
                || root(kind),
                std::future::ready(result),
            ));
        }
        assert_eq!(counts(), [1, 1, 0, 0]);
        assert_eq!(cursors(), saved);
    }

    let failed = || Err("Sign-in changed".to_owned());
    import(failed(), failed());
    assert_eq!(counts(), [1, 1, 0, 0]);
    assert_eq!(cursors(), saved);

    let codex = || Ok(json!({"accountId":"codex-new"}));
    let claude = || Ok(json!({"accountId":"claude:new"}));
    import(codex(), failed());
    assert_eq!(counts(), [1, 1, 1, 0]);
    assert_eq!(cursors()[1], saved[1]);
    import(codex(), claude());
    import(codex(), claude());
    assert_eq!(counts(), [1, 1, 1, 1]);
    for ((kind, _), positions) in files.into_iter().zip(cursors()) {
        let file = root(kind).unwrap().join("session.jsonl");
        let key = if cfg!(windows) {
            file.to_string_lossy().to_lowercase()
        } else {
            file.to_string_lossy().into_owned()
        };
        assert_eq!(positions[&key].offset, file.metadata().unwrap().len());
    }

    let saved = cursors();
    append(3);
    for (kind, account) in [(Kind::Codex, "codex-new"), (Kind::Claude, "claude:new")] {
        tauri::async_runtime::block_on(read_local_logs(
            &state,
            kind,
            0,
            Some(account),
            || {
                state.0.lock().unwrap().dashboard.settings.local_logs = false;
                root(kind)
            },
            async { panic!("Identity read after log import was disabled") },
        ));
    }
    assert_eq!(counts(), [1, 1, 1, 1]);
    assert_eq!(cursors(), saved);
}
