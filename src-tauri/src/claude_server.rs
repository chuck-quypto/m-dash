use axum::{
    body::to_bytes,
    extract::{Path, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

pub type Receiver = Arc<dyn Fn(&str, Value) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
struct ReceiverState {
    token: String,
    receive: Receiver,
}

pub fn router(token: String, receive: Receiver) -> Router {
    Router::new()
        .route("/claude/{token}/{kind}", post(ingest))
        .with_state(ReceiverState { token, receive })
}

async fn ingest(
    State(state): State<ReceiverState>,
    Path((token, kind)): Path<(String, String)>,
    request: Request,
) -> Response {
    if token != state.token || !matches!(kind.as_str(), "statusline" | "logs") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if request.headers().contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|part| part.trim() == "application/json")
        })
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(3),
        to_bytes(request.into_body(), 2 * 1024 * 1024),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match (state.receive)(&kind, value) {
        Ok(()) => ([(header::CONTENT_TYPE, "application/json")], "{}").into_response(),
        Err(_) => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use std::{
        io::{Read, Write},
        sync::Mutex,
    };

    #[test]
    fn occupied_receiver_disables_managed_feed_and_recovery_rejects_old_token() {
        tauri::async_runtime::block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let settings = directory.path().join("settings.json");
            let app = directory.path().join("app");
            let original = serde_json::json!({"theme":"dark","env":{"KEEP":"yes"}});
            std::fs::write(&settings, original.to_string()).unwrap();
            crate::claude_setup::set_enabled(&app, &settings, true).unwrap();
            let old = crate::claude_setup::load(&app).unwrap().token;
            // An isolated port avoids interfering with an installed collector.
            let occupied = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = occupied.local_addr().unwrap().port();
            assert!(crate::claude_setup::prepare_receiver(&app, &settings, port).is_err());
            assert!(!crate::claude_setup::enabled(&app, &settings).unwrap());
            let after: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
            assert_eq!(after, original);
            assert!(
                crate::claude_setup::load(&app).unwrap().token.is_empty(),
                "legacy helpers must not expose a future credential while the port is unavailable"
            );
            drop(occupied);
            let (listener, config) =
                crate::claude_setup::prepare_receiver(&app, &settings, port).unwrap();
            let current = config.token.clone();
            assert_ne!(current, old, "an exposed token must not survive recovery");
            assert!(!current.is_empty());
            assert!(!crate::claude_setup::enabled(&app, &settings).unwrap());
            crate::claude_setup::set_enabled(&app, &settings, true).unwrap();
            let receiver: Receiver = Arc::new(|_, _| Ok(()));
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let server = tauri::async_runtime::spawn(async move {
                axum::serve(listener, router(config.token, receiver))
                    .await
                    .unwrap();
            });
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            for (token, expected) in [(old, StatusCode::NOT_FOUND), (current, StatusCode::OK)] {
                let response = client
                    .post(format!("http://127.0.0.1:{port}/claude/{token}/logs"))
                    .header("content-type", "application/json")
                    .body("{}")
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), expected);
            }
            server.abort();
        });
    }

    #[test]
    fn loopback_feed_rejects_browser_and_wrong_token_requests_and_accepts_selected_metadata() {
        tauri::async_runtime::block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let store = Arc::new(Mutex::new(
                Store::open(&directory.path().join("usage.sqlite")).unwrap(),
            ));
            let receiving = store.clone();
            let receiver: Receiver = Arc::new(move |kind, value| {
                let mut store = receiving.lock().unwrap();
                if kind == "logs" {
                    store.ingest_claude_telemetry(value, 1000).map(|_| ())
                } else {
                    store.ingest_claude_statusline(value, 1000).map(|_| ())
                }
            });
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let server = tauri::async_runtime::spawn(async move {
                axum::serve(listener, router("fixture-token".into(), receiver))
                    .await
                    .unwrap()
            });
            let call = move |path: &str, origin: bool, body: &str| {
                let mut stream = std::net::TcpStream::connect(address).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let origin = if origin {
                    "Origin: https://untrusted.example\r\n"
                } else {
                    ""
                };
                write!(stream,"POST {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n{origin}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                response
            };
            let replies = tauri::async_runtime::spawn_blocking(move || {
                let denied = call("/claude/wrong/logs",false,"{}");
                let browser = call("/claude/fixture-token/logs",true,"{}");
                let logs = r#"{"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code"}}]},"scopeLogs":[{"logRecords":[{"attributes":[{"key":"user.account_uuid","value":{"stringValue":"one"}},{"key":"organization.id","value":{"stringValue":"org"}},{"key":"session.id","value":{"stringValue":"session"}}]}]}]}]}"#;
                let identified = call("/claude/fixture-token/logs",false,logs);
                let status = call("/claude/fixture-token/statusline",false,r#"{"session_id":"session","cost":{"total_api_duration_ms":100},"rate_limits":{"five_hour":{"used_percentage":10,"resets_at":2000}}}"#);
                (denied,browser,identified,status)
            }).await.unwrap();
            assert!(replies.0.starts_with("HTTP/1.1 404"));
            assert!(replies.1.starts_with("HTTP/1.1 403"));
            assert!(replies.2.starts_with("HTTP/1.1 200"));
            assert!(replies.3.starts_with("HTTP/1.1 200"));
            assert_eq!(
                store.lock().unwrap().claude_snapshot().unwrap().unwrap()["windows"][0]
                    ["remainingPercent"],
                90.0
            );
            server.abort();
        });
    }
}
