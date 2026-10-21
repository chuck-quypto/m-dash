use m_dash_sync::{Phone, Result};
use serde_json::{json, Value};
#[cfg(target_os = "android")]
use tauri::Manager;
#[derive(Default)]
pub struct State(pub tokio::sync::Mutex<()>);

#[cfg(target_os = "android")]
#[allow(non_snake_case)]
#[jni::jni_mangle("net.chuckdudley.mdash.mobile.SyncVaultPlugin")]
pub fn initTls<'local>(
    mut env: jni::EnvUnowned<'local>,
    _this: jni::objects::JObject<'local>,
    context: jni::objects::JObject<'local>,
) {
    env.with_env(|env| rustls_platform_verifier::android::init_with_env(env, context))
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[cfg(target_os = "android")]
async fn vault(app: &tauri::AppHandle, op: &str, contents: Option<String>) -> Result<Value> {
    app.state::<tauri::plugin::PluginHandle<tauri::Wry>>()
        .run_mobile_plugin_async("vault", json!({"op":op,"contents":contents}))
        .await
        .map_err(|_| "Protected sync storage unavailable. Unlock your phone and retry.".into())
}
#[cfg(not(target_os = "android"))]
async fn vault(_: &tauri::AppHandle, _: &str, _: Option<String>) -> Result<Value> {
    Err("Live pairing requires the installed Android app.".into())
}
async fn read(app: &tauri::AppHandle) -> Result<Option<Phone>> {
    let v = vault(app, "read", None).await?;
    match v["contents"].as_str() {
        None => Ok(None),
        Some(s) if s.len() <= 140000 => serde_json::from_str(s)
            .map(Some)
            .map_err(|_| "Saved pairing is invalid; disconnect and re-pair.".into()),
        _ => Err("Saved pairing is invalid.".into()),
    }
}
async fn save(app: &tauri::AppHandle, phone: &Phone) -> Result<()> {
    vault(
        app,
        "write",
        Some(serde_json::to_string(phone).map_err(|_| "Could not save pairing.")?),
    )
    .await?;
    Ok(())
}
#[tauri::command]
pub async fn live_status(app: tauri::AppHandle, state: tauri::State<'_, State>) -> Result<Value> {
    let _guard = state.0.lock().await;
    let Some(phone) = read(&app).await? else {
        return Ok(Value::Null);
    };
    phone.offer.validate()?;
    let cached = phone.cached(now());
    let warning = if phone.read_token.is_none() {
        Some("Legacy pairing: upgrade the private relay and create a new desktop pairing. Saved readings remain available.".to_owned())
    } else {
        cached.as_ref().err().cloned()
    };
    Ok(
        json!({"verification":phone.offer.verification(&phone.claim)?,"snapshot":cached.as_ref().ok().and_then(|s|s.as_ref()),"receivedAt":phone.received_at,"warning":warning}),
    )
}
#[tauri::command]
pub async fn live_pair(
    app: tauri::AppHandle,
    state: tauri::State<'_, State>,
    code: String,
    consent: bool,
) -> Result<String> {
    if !consent {
        return Err("Review the sync privacy notice first.".into());
    }
    let _guard = state.0.lock().await;
    if read(&app).await?.is_some() {
        return Err("Disconnect this phone before pairing another desktop.".into());
    }
    let phone = Phone::pair(code.trim(), now())?;
    save(&app, &phone).await?;
    phone.claim().await?;
    phone.offer.verification(&phone.claim)
}
#[tauri::command]
pub async fn live_pull(app: tauri::AppHandle, state: tauri::State<'_, State>) -> Result<Value> {
    let _guard = state.0.lock().await;
    let mut phone = read(&app).await?.ok_or("Not connected.")?;
    // A failed first claim is retryable after process/network interruption.
    if phone.last.is_none() {
        let _ = phone.claim().await;
    }
    let snapshot = phone.pull(now()).await?;
    save(&app, &phone).await?;
    Ok(snapshot)
}
#[tauri::command]
pub async fn live_clear(app: tauri::AppHandle, state: tauri::State<'_, State>) -> Result<()> {
    let _guard = state.0.lock().await;
    vault(&app, "clear", None).await?;
    Ok(())
}
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("sync-vault")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            _app.manage(
                _api.register_android_plugin("net.chuckdudley.mdash.mobile", "SyncVaultPlugin")?,
            );
            Ok(())
        })
        .build()
}
