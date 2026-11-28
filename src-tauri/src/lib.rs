pub mod antigravity;
pub mod burn;
pub mod claude;
pub mod claude_feed;
pub mod claude_ping;
pub mod claude_ping_runner;
pub mod claude_server;
pub mod claude_setup;
pub mod codex;
pub mod collector;
pub mod compact;
pub mod credentials;
pub mod dashboard;
pub mod forecast;
pub mod grok;
#[cfg(test)]
mod local_log_tests;
pub mod local_logs;
#[cfg(target_os = "macos")]
mod macos_lifecycle;
pub mod mobile_sync;
pub mod openrouter;
pub mod settings;
pub mod store;
pub mod usage;
pub mod usage_relay;

use dashboard::{Dashboard, Schedule};
use local_logs::Kind;
use settings::{Provider, Providers, Settings};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

struct AppState(Mutex<Inner>);
#[derive(Default)]
struct CompactState(Mutex<Option<Vec<compact::Allowance>>>);
struct Inner {
    dashboard: Dashboard,
    store: store::Store,
    codex_schedule: Schedule,
    grok_schedule: Schedule,
    claude_schedule: Schedule,
    openrouter_schedule: Schedule,
    antigravity_schedule: Schedule,
    openrouter_setup: bool,
    openrouter_generation: u64,
    openrouter_retry_after: u64,
    directory: PathBuf,
    claude_receiver_ready: bool,
    settings_generation: u64,
    log_scan_generation: u64,
    ping: claude_ping::State,
    ping_generation: u64,
    ping_running: bool,
    last_feed_forecast: u64,
    last_prune: u64,
}

#[cfg(target_os = "linux")]
#[link(name = "gdk-3")]
extern "C" {
    fn gdk_x11_get_server_time(window: *mut gtk::gdk::ffi::GdkWindow) -> u32;
}

impl Inner {
    fn apply_settings(&mut self, settings: Settings) {
        let providers_changed = self.dashboard.settings.providers != settings.providers;
        if providers_changed || self.dashboard.settings.local_logs != settings.local_logs {
            self.log_scan_generation += 1;
        }
        if providers_changed {
            self.settings_generation += 1;
            self.codex_schedule = Schedule::default();
            self.grok_schedule = Schedule::default();
            self.antigravity_schedule = Schedule::default();
            // Keep Claude's five-minute minimum and OpenRouter's server retry delay.
        }
        self.dashboard.settings = settings;
        self.dashboard.needs_setup = false;
        self.update_next_refresh();
    }

    fn local_log_account<'a>(
        &self,
        kind: Kind,
        generation: u64,
        confirmed: Option<&'a str>,
    ) -> Option<&'a str> {
        let settings = &self.dashboard.settings;
        if self.log_scan_generation != generation || !settings.local_logs {
            return None;
        }
        // Session logs name no account. The same signed-in account must still own the scan.
        let snapshot = match kind {
            Kind::Codex if settings.providers.codex => self.dashboard.snapshot.as_ref()?,
            Kind::Claude if settings.providers.claude => self.dashboard.claude.as_ref()?,
            Kind::AntigravityCli | Kind::AntigravityDesktop | Kind::AntigravityIde
                if settings.providers.antigravity =>
            {
                self.dashboard.antigravity.as_ref()?
            }
            Kind::Codex
            | Kind::Claude
            | Kind::AntigravityCli
            | Kind::AntigravityDesktop
            | Kind::AntigravityIde => return None,
        };
        confirmed.filter(|account| snapshot["accountId"].as_str() == Some(*account))
    }

    fn finish_log_scan(
        &mut self,
        kind: Kind,
        account: &str,
        generation: u64,
        confirmed: Option<&str>,
        scan: local_logs::Scan,
    ) -> Result<(), String> {
        if self.local_log_account(kind, generation, confirmed) == Some(account) {
            self.store.ingest_local_logs(kind.source(), account, scan)?;
        }
        Ok(())
    }

    fn update_next_refresh(&mut self) {
        self.dashboard.next_refresh = [
            (
                self.dashboard.settings.providers.codex,
                self.codex_schedule.next,
            ),
            (
                self.dashboard.settings.providers.grok,
                self.grok_schedule.next,
            ),
            (
                self.dashboard.settings.providers.claude,
                self.claude_schedule.next,
            ),
            (
                self.dashboard.settings.providers.openrouter
                    && self.dashboard.openrouter_configured,
                self.openrouter_schedule
                    .next
                    .max(self.openrouter_retry_after),
            ),
            (
                self.dashboard.settings.providers.antigravity,
                self.antigravity_schedule.next,
            ),
        ]
        .into_iter()
        .filter_map(|(enabled, next)| enabled.then_some(next))
        .min();
    }
}

fn publish_compact(app: &tauri::AppHandle, dashboard: &Dashboard) {
    let rows = compact::allowances(dashboard, collector::now());
    let state = app.state::<CompactState>();
    let Ok(mut previous) = state.0.lock() else {
        return;
    };
    if previous.as_ref() == Some(&rows) {
        return;
    }
    let handle = app.clone();
    let update = rows.clone();
    // Never wait for the UI thread while a collector holds the dashboard lock.
    if let Err(error) = app.run_on_main_thread(move || {
        if let Some(tray) = handle.tray_by_id("accounts") {
            if let Err(error) = tray.set_icon(Some(compact::tray_image(&update))) {
                eprintln!("Could not update the allowance tray icon: {error}");
            }
            let _ = tray.set_tooltip(Some(compact::tray_tooltip(&update)));
        }
        let _ = handle.emit_to("compact", "compact-updated", &update);
    }) {
        eprintln!("Could not queue the compact allowance update: {error}");
        return;
    }
    *previous = Some(rows);
}

fn publish_dashboard(app: &tauri::AppHandle, dashboard: &Dashboard) {
    let _ = app.emit("dashboard-updated", dashboard);
    publish_compact(app, dashboard);
}

#[tauri::command]
fn get_compact_dashboard(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<compact::Allowance>, String> {
    let inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    Ok(compact::allowances(&inner.dashboard, collector::now()))
}

#[tauri::command]
fn open_dashboard(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or("The dashboard window is unavailable.")?;
    #[cfg(target_os = "linux")]
    {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let native = window.clone();
        app.run_on_main_thread(move || {
            use gtk::glib::translate::ToGlibPtr;
            use gtk::prelude::*;
            // Tao skips focus while its minimized/visible flags still hold the old state.
            let result = native
                .gtk_window()
                .map(|window| {
                    window.show_all();
                    let timestamp = window
                        .window()
                        .filter(|surface| surface.type_().name() == "GdkX11Window")
                        .map_or(0, |surface| {
                            surface.set_events(
                                surface.events() | gtk::gdk::EventMask::PROPERTY_CHANGE_MASK,
                            );
                            // Tray/IPC requests have no GTK event; a cached time can deny restore.
                            // SAFETY: realized X11 window with property events, on the UI thread.
                            unsafe { gdk_x11_get_server_time(surface.to_glib_none().0) }
                        });
                    window.present_with_time(timestamp);
                })
                .map_err(|_| "Could not restore the dashboard.");
            let _ = send.send(result);
        })
        .map_err(|_| "Could not restore the dashboard.")?;
        receive
            .recv()
            .map_err(|_| "Could not restore the dashboard.")??;
    }
    #[cfg(not(target_os = "linux"))]
    window
        .unminimize()
        .and_then(|()| window.show())
        .and_then(|()| window.set_focus())
        .map_err(|_| "Could not restore the dashboard.")?;
    if let Some(strip) = app.get_webview_window("compact") {
        let _ = strip.hide();
    }
    Ok(())
}

fn sync_compact_window(main: &tauri::Window) -> tauri::Result<()> {
    let Some(strip) = main.get_webview_window("compact") else {
        return Ok(());
    };
    if !compact::show_strip(main.is_visible()?, main.is_minimized()?) {
        if strip.is_visible()? {
            strip.hide()?;
        }
        return Ok(());
    }
    if strip.is_visible()? {
        return Ok(());
    }
    if let Some(monitor) = main.current_monitor()?.or(main.primary_monitor()?) {
        let work = monitor.work_area();
        let scale = monitor.scale_factor();
        let size = tauri::LogicalSize::new(528.0, 88.0).to_physical::<u32>(scale);
        let gap = (12.0 * scale).round() as i32;
        strip.set_position(tauri::PhysicalPosition::new(
            (work.position.x + work.size.width as i32 - size.width as i32 - gap)
                .max(work.position.x),
            (work.position.y + work.size.height as i32 - size.height as i32 - gap)
                .max(work.position.y),
        ))?;
    }
    strip.show()
}

#[cfg(target_os = "linux")]
fn desktop_tray_available(app: &tauri::AppHandle) -> bool {
    app.tray_by_id("accounts").is_some()
        && std::process::Command::new("/usr/bin/gdbus")
            .args([
                "call",
                "--session",
                "--timeout",
                "1",
                "--dest",
                "org.freedesktop.DBus",
                "--object-path",
                "/org/freedesktop/DBus",
                "--method",
                "org.freedesktop.DBus.NameHasOwner",
                "org.kde.StatusNotifierWatcher",
            ])
            .stderr(std::process::Stdio::null())
            .output()
            .is_ok_and(|output| output.status.success() && output.stdout == b"(true,)\n")
}

fn update_forecasts(inner: &mut Inner) {
    inner.dashboard.forecasts.clear();
    for (provider, name, snapshot, unavailable) in [
        (
            Provider::Codex,
            "Codex",
            &inner.dashboard.snapshot,
            inner.dashboard.error.is_some(),
        ),
        (
            Provider::Claude,
            "Claude",
            &inner.dashboard.claude,
            inner.dashboard.claude_failed(),
        ),
        (
            Provider::Grok,
            "Grok",
            &inner.dashboard.grok,
            inner.dashboard.grok_error.is_some(),
        ),
        (
            Provider::Openrouter,
            "OpenRouter",
            &inner.dashboard.openrouter,
            !inner.dashboard.openrouter_configured || inner.dashboard.openrouter_error.is_some(),
        ),
        (
            Provider::Antigravity,
            "Antigravity",
            &inner.dashboard.antigravity,
            inner.dashboard.antigravity_error.is_some(),
        ),
    ] {
        if !inner.dashboard.settings.providers.enabled(provider) {
            continue;
        }
        if let Some(snapshot) = snapshot {
            let mut forecasts = inner
                .store
                .forecasts(snapshot, collector::now())
                .unwrap_or_else(|message| {
                    vec![serde_json::json!({"label":"Capacity","reason":message})]
                });
            for forecast in &mut forecasts {
                forecast["provider"] = serde_json::json!(name);
                if unavailable {
                    forecast["perHour"] = serde_json::Value::Null;
                    forecast["exhaustsAt"] = serde_json::Value::Null;
                    forecast["alert"] = serde_json::Value::Null;
                    forecast["alertReason"] = serde_json::Value::Null;
                    forecast["runsOutAt"] = serde_json::Value::Null;
                    forecast["reason"] =
                        serde_json::json!("Reconnect or refresh this provider before estimating.");
                }
            }
            inner.dashboard.forecasts.extend(forecasts);
        }
    }
}

#[tauri::command]
fn open_openrouter_settings() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("rundll32.exe")
            .args([
                "url.dll,FileProtocolHandler",
                "https://openrouter.ai/settings/management-keys",
            ])
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| {
                "Could not open the browser. Visit openrouter.ai/settings/management-keys."
            })?;
        Ok(())
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let opener = if cfg!(target_os = "macos") {
            "/usr/bin/open"
        } else {
            "/usr/bin/xdg-open"
        };
        let status = std::process::Command::new(opener)
            .arg("https://openrouter.ai/settings/management-keys")
            .status()
            .map_err(|_| {
                "Could not open the browser. Visit openrouter.ai/settings/management-keys."
            })?;
        if status.success() {
            Ok(())
        } else {
            Err("Could not open the browser. Visit openrouter.ai/settings/management-keys.".into())
        }
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    Err("Open openrouter.ai/settings/management-keys in your browser.".into())
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod native_link_tests {
    #[test]
    #[ignore = "opens the system browser; run as a native host check"]
    fn management_key_link_opens_the_system_browser() {
        assert!(super::open_openrouter_settings().is_ok());
    }
}

#[tauri::command]
async fn connect_openrouter(app: tauri::AppHandle) -> Result<Dashboard, String> {
    let state = app.state::<AppState>();
    {
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if inner.openrouter_setup {
            return Err("OpenRouter setup is already open.".into());
        }
        if !inner.dashboard.settings.providers.openrouter {
            return Err("Enable OpenRouter in Settings before connecting a key.".into());
        }
        inner.openrouter_setup = true;
        inner.openrouter_generation += 1;
    }
    let result: Result<(), String> = async {
        #[cfg(windows)]
        let parent = app
            .get_webview_window("main")
            .and_then(|window| window.hwnd().ok())
            .map_or(0, |hwnd| hwnd.0 as isize);
        #[cfg(not(any(windows, target_os = "macos")))]
        let parent = 0;
        #[cfg(not(target_os = "macos"))]
        let key = tauri::async_runtime::spawn_blocking(move || credentials::prompt(parent))
            .await
            .map_err(|_| "The secure key prompt stopped.")??;
        #[cfg(target_os = "macos")]
        let key = {
            let (send, receive) = tokio::sync::oneshot::channel();
            app.run_on_main_thread(move || {
                let _ = send.send(credentials::prompt(0));
            })
            .map_err(|_| "Could not open the secure key prompt.")?;
            receive
                .await
                .map_err(|_| "The secure key prompt stopped.")??
        };
        if let Some(key) = key {
            let mut snapshot = openrouter::collect_with_key(&key)
                .await
                .map_err(|error| error.message)?;
            credentials::write(&key)?;
            let mut inner = state
                .0
                .lock()
                .map_err(|_| "The collector state is unavailable.")?;
            inner.dashboard.openrouter_configured = true;
            let saved = inner
                .store
                .save(&snapshot)
                .and_then(|()| inner.store.attach_history(&mut snapshot));
            inner.openrouter_retry_after = collector::now()
                .saturating_add(snapshot["retryAfterSeconds"].as_u64().unwrap_or(0));
            inner.dashboard.openrouter = Some(snapshot);
            inner.dashboard.openrouter_error = saved.err();
            inner
                .openrouter_schedule
                .finish(collector::now(), false, true);
        }
        Ok(())
    }
    .await;
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    inner.openrouter_setup = false;
    inner.update_next_refresh();
    update_forecasts(&mut inner);
    publish_dashboard(&app, &inner.dashboard);
    result?;
    Ok(inner.dashboard.clone())
}

#[tauri::command]
fn disconnect_openrouter(app: tauri::AppHandle) -> Result<Dashboard, String> {
    let state = app.state::<AppState>();
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    if inner.openrouter_setup {
        return Err("Finish or cancel the secure key prompt first.".into());
    }
    credentials::delete()?;
    inner.dashboard.openrouter_configured = false;
    inner.openrouter_generation += 1;
    inner.dashboard.openrouter_error = None;
    inner.update_next_refresh();
    update_forecasts(&mut inner);
    publish_dashboard(&app, &inner.dashboard);
    Ok(inner.dashboard.clone())
}

#[tauri::command]
fn set_claude_enabled(app: tauri::AppHandle, enabled: bool) -> Result<Dashboard, String> {
    let state = app.state::<AppState>();
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    if enabled && !inner.dashboard.settings.providers.claude {
        return Err("Enable Claude in Settings before enabling its local feed.".into());
    }
    if enabled && !inner.claude_receiver_ready {
        return Err("The local Claude receiver could not start. Close other m-dash copies and restart this one.".into());
    }
    claude_setup::set_enabled(&inner.directory, &claude_setup::settings_path()?, enabled)?;
    inner.dashboard.claude_enabled = enabled;
    inner.dashboard.claude_error = None;
    update_forecasts(&mut inner);
    publish_dashboard(&app, &inner.dashboard);
    Ok(inner.dashboard.clone())
}

#[tauri::command]
fn set_ledger_days(app: tauri::AppHandle, days: u64) -> Result<Dashboard, String> {
    let state = app.state::<AppState>();
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    inner.store.save_ledger_days(days)?;
    inner.dashboard.ledger_days = days;
    update_consistency(&mut inner, collector::now());
    publish_dashboard(&app, &inner.dashboard);
    Ok(inner.dashboard.clone())
}

/// The signed-in account a ledger belongs to: "codex", "claude", or "antigravity".
fn ledger_account(inner: &Inner, provider: &str) -> Result<String, String> {
    let snapshot = match provider {
        "codex" => &inner.dashboard.snapshot,
        "claude" => &inner.dashboard.claude,
        "antigravity" => &inner.dashboard.antigravity,
        _ => return Err("Unknown ledger.".into()),
    };
    snapshot
        .as_ref()
        .and_then(|snapshot| snapshot["accountId"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "Sign in before adding notes to this ledger.".into())
}

/// Midnight UTC of a YYYY-MM-DD date, the day boundary the ledger uses.
fn ledger_day(date: &str) -> Result<i64, String> {
    let invalid = || "Choose a valid date.".to_string();
    let mut parts = date.splitn(3, '-').map(|part| part.parse::<i32>().ok());
    let (Some(Some(year)), Some(Some(month)), Some(Some(day))) =
        (parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    let month = u8::try_from(month)
        .ok()
        .and_then(|month| time::Month::try_from(month).ok())
        .ok_or_else(invalid)?;
    let day = u8::try_from(day).map_err(|_| invalid())?;
    time::Date::from_calendar_date(year, month, day)
        .map(|day| day.midnight().assume_utc().unix_timestamp())
        .map_err(|_| invalid())
}

#[tauri::command]
fn add_ledger_mark(
    app: tauri::AppHandle,
    provider: String,
    date: String,
    plan: bool,
    text: String,
) -> Result<Dashboard, String> {
    let state = app.state::<AppState>();
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    let account = ledger_account(&inner, &provider)?;
    let at = ledger_day(&date)?;
    if at > collector::now() as i64 {
        return Err("Choose today or an earlier date.".into());
    }
    inner.store.add_ledger_mark(&account, at, plan, &text)?;
    update_consistency(&mut inner, collector::now());
    publish_dashboard(&app, &inner.dashboard);
    Ok(inner.dashboard.clone())
}

#[tauri::command]
fn remove_ledger_mark(
    app: tauri::AppHandle,
    provider: String,
    at: i64,
    kind: String,
) -> Result<Dashboard, String> {
    let state = app.state::<AppState>();
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    let account = ledger_account(&inner, &provider)?;
    inner.store.remove_ledger_mark(&account, at, &kind)?;
    update_consistency(&mut inner, collector::now());
    publish_dashboard(&app, &inner.dashboard);
    Ok(inner.dashboard.clone())
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: Settings) -> Result<Dashboard, String> {
    let state = app.state::<AppState>();
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    if inner.openrouter_setup {
        return Err("Finish or cancel the secure key prompt before saving settings.".into());
    }
    settings.validate()?;
    if !settings.providers.claude && inner.ping.enabled_account.is_some() {
        let mut ping = inner.ping.clone();
        ping.configure(None, false)?;
        inner.store.save_claude_ping(&ping)?;
        inner.ping = ping;
        inner.ping_generation += 1;
        inner.dashboard.claude_ping = inner.ping.status();
    }
    inner.store.save_settings(&settings)?;
    inner.apply_settings(settings);
    update_forecasts(&mut inner);
    publish_dashboard(&app, &inner.dashboard);
    Ok(inner.dashboard.clone())
}

#[tauri::command]
async fn set_claude_ping(
    app: tauri::AppHandle,
    enabled: bool,
    confirmed: bool,
) -> Result<Dashboard, String> {
    let state = app.state::<AppState>();
    let (generation, ping_generation) = {
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if !enabled {
            let mut ping = inner.ping.clone();
            ping.configure(None, false)?;
            inner.store.save_claude_ping(&ping)?;
            inner.ping = ping;
            inner.ping_generation += 1;
            inner.dashboard.claude_ping = inner.ping.status();
            publish_dashboard(&app, &inner.dashboard);
            return Ok(inner.dashboard.clone());
        }
        if !confirmed {
            return Err("Confirm the usage notice before enabling automatic pings.".into());
        }
        if !inner.dashboard.settings.providers.claude {
            return Err("Enable the Claude provider first.".into());
        }
        (inner.settings_generation, inner.ping_generation)
    };
    // These are metadata checks, not inference. Consent is bound to this sign-in.
    let _prepared = claude_ping_runner::Prepared::check(claude::executable()?).await?;
    let snapshot = claude::collect().await?;
    let account = snapshot["accountId"]
        .as_str()
        .ok_or("Claude did not supply its account identity.")?;
    if !matches!(snapshot["plan"].as_str(), Some("pro" | "max")) {
        return Err("Automatic pings currently support Claude Pro and Max subscriptions.".into());
    }
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    if inner.settings_generation != generation || inner.ping_generation != ping_generation {
        return Err("Settings changed during setup. Review and confirm again.".into());
    }
    let mut ping = inner.ping.clone();
    ping.configure(Some(account), confirmed)?;
    inner.store.save_claude_ping(&ping)?;
    inner.ping = ping;
    inner.ping_generation += 1;
    inner.dashboard.claude_ping = inner.ping.status();
    publish_dashboard(&app, &inner.dashboard);
    Ok(inner.dashboard.clone())
}

async fn maybe_ping_claude(
    app: &tauri::AppHandle,
    snapshot: &serde_json::Value,
    generation: u64,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let ping_generation = {
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if inner.settings_generation != generation || !inner.dashboard.settings.providers.claude {
            return Ok(());
        }
        let mut candidate = inner.ping.clone();
        if candidate.observe(snapshot, collector::now()).is_none() {
            inner.store.save_claude_ping(&candidate)?;
            inner.ping = candidate;
            inner.dashboard.claude_ping = inner.ping.status();
            return Ok(());
        }
        inner.ping_generation
    };
    let prepared = match claude::executable() {
        Ok(path) => claude_ping_runner::Prepared::check(path).await,
        Err(message) => Err(message),
    };
    let (attempt, started) = {
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if inner.settings_generation != generation
            || inner.ping_generation != ping_generation
            || !inner.dashboard.settings.providers.claude
        {
            return Ok(());
        }
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(message) => {
                let mut candidate = inner.ping.clone();
                candidate.defer(&message);
                inner.store.save_claude_ping(&candidate)?;
                inner.ping = candidate;
                inner.dashboard.claude_ping = inner.ping.status();
                return Ok(());
            }
        };
        // Another Claude session may have started a window during preflight.
        let mut latest = claude::read_usage_cache()?;
        if latest["quotaFetchedAtMs"].as_u64() < snapshot["quotaFetchedAtMs"].as_u64() {
            return Ok(());
        }
        latest["pingFresh"] = snapshot["pingFresh"].clone();
        latest["plan"] = snapshot["plan"].clone();
        let mut candidate = inner.ping.clone();
        let attempt = candidate.observe(&latest, collector::now());
        // Durable before spawn: interruption or uncertain output must never cause replay.
        inner.store.save_claude_ping(&candidate)?;
        inner.ping = candidate;
        inner.dashboard.claude_ping = inner.ping.status();
        let Some(attempt) = attempt else {
            return Ok(());
        };
        let started = prepared.start();
        publish_dashboard(app, &inner.dashboard);
        (attempt, started)
    };
    let result = match started {
        Ok(run) => run.finish().await,
        Err(message) => Err(message),
    };
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    let mut candidate = inner.ping.clone();
    candidate.finish(&attempt, result);
    inner.store.save_claude_ping(&candidate)?;
    inner.ping = candidate;
    inner.dashboard.claude_ping = inner.ping.status();
    publish_dashboard(app, &inner.dashboard);
    Ok(())
}

#[tauri::command]
fn get_dashboard(state: tauri::State<'_, AppState>) -> Result<Dashboard, String> {
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    update_forecasts(&mut inner);
    Ok(inner.dashboard.clone())
}

#[tauri::command]
async fn refresh_dashboard(app: tauri::AppHandle) -> Result<Dashboard, String> {
    refresh(app, true).await
}

async fn refresh(app: tauri::AppHandle, force: bool) -> Result<Dashboard, String> {
    let state = app.state::<AppState>();
    let (
        read_codex,
        read_grok,
        read_claude,
        read_openrouter,
        read_antigravity,
        openrouter_generation,
        settings_generation,
        log_scan_generation,
    ) = {
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if inner.dashboard.refreshing {
            return Ok(inner.dashboard.clone());
        }
        let now = collector::now();
        let read_codex =
            inner.dashboard.settings.providers.codex && inner.codex_schedule.due(now, force);
        let read_grok =
            inner.dashboard.settings.providers.grok && inner.grok_schedule.due(now, force);
        let read_claude = inner.dashboard.settings.providers.claude
            && inner
                .claude_schedule
                .due(now, force && inner.dashboard.claude_usage_error.is_none());
        let read_openrouter = inner.dashboard.settings.providers.openrouter
            && inner.dashboard.openrouter_configured
            && !inner.openrouter_setup
            && inner.openrouter_retry_after <= now
            && inner.openrouter_schedule.due(now, force);
        let read_antigravity = inner.dashboard.settings.providers.antigravity
            && inner.antigravity_schedule.due(now, force);
        if !read_codex && !read_grok && !read_claude && !read_openrouter && !read_antigravity {
            return Ok(inner.dashboard.clone());
        }
        inner.dashboard.refreshing = true;
        inner.dashboard.last_attempt = Some(now);
        // A settings change may discard this result; retain the polling cooldown.
        if read_claude {
            inner.claude_schedule.next = now.saturating_add(300);
        }
        if read_antigravity {
            inner.antigravity_schedule.next = now.saturating_add(300);
        }
        publish_dashboard(&app, &inner.dashboard);
        (
            read_codex,
            read_grok,
            read_claude,
            read_openrouter,
            read_antigravity,
            inner.openrouter_generation,
            inner.settings_generation,
            inner.log_scan_generation,
        )
    };
    let grok_task = read_grok.then(|| tauri::async_runtime::spawn(collector::collect_grok()));
    let claude_task = read_claude.then(|| tauri::async_runtime::spawn(claude::collect()));
    let openrouter_task =
        read_openrouter.then(|| tauri::async_runtime::spawn(openrouter::collect()));
    let antigravity_task =
        read_antigravity.then(|| tauri::async_runtime::spawn(antigravity::collect()));
    let visible = app.get_webview_window("main").is_some_and(|window| {
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true)
    });
    let mut codex_log_account = None;
    if read_codex {
        let mut result = collector::collect().await;
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if inner.settings_generation == settings_generation {
            codex_log_account = refreshed_account(&result);
            let storage_error = result.as_mut().ok().and_then(|snapshot| {
                inner
                    .store
                    .save(snapshot)
                    .and_then(|()| {
                        inner.store.attach_history(snapshot)?;
                        let count = inner.store.observation_count(
                            snapshot["accountId"].as_str().unwrap_or_default(),
                        )?;
                        inner.dashboard.observations = count;
                        Ok(())
                    })
                    .err()
            });
            inner
                .codex_schedule
                .finish(collector::now(), result.is_err(), visible);
            inner.dashboard.apply_result(result);
            if let Some(message) = storage_error {
                inner.dashboard.error = Some(format!(
                    "{message} Current values are shown but may not survive restart."
                ));
            }
            publish_dashboard(&app, &inner.dashboard);
        }
    }
    if let Some(task) = openrouter_task {
        let result = task
            .await
            .unwrap_or_else(|_| Err("The OpenRouter collector stopped. Refresh to retry.".into()));
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if inner.openrouter_generation == openrouter_generation {
            let delay = result.as_ref().map_or_else(
                |error| error.retry_after,
                |snapshot| snapshot["retryAfterSeconds"].as_u64().unwrap_or(0),
            );
            inner.openrouter_retry_after = inner
                .openrouter_retry_after
                .max(collector::now().saturating_add(delay));
        }
        if inner.settings_generation == settings_generation
            && inner.openrouter_generation == openrouter_generation
        {
            let failed = result
                .as_ref()
                .map_or(true, |snapshot| !snapshot["usageError"].is_null());
            inner
                .openrouter_schedule
                .finish(collector::now(), failed, visible);
            match result {
                Ok(mut snapshot) => {
                    let saved = inner
                        .store
                        .save(&snapshot)
                        .and_then(|()| inner.store.attach_history(&mut snapshot));
                    inner.dashboard.openrouter = Some(snapshot);
                    inner.dashboard.openrouter_error = saved.err();
                }
                Err(error) => {
                    inner.dashboard.openrouter_error = Some(error.message);
                }
            }
            inner.openrouter_schedule.next = inner
                .openrouter_schedule
                .next
                .max(inner.openrouter_retry_after);
            publish_dashboard(&app, &inner.dashboard);
        }
    }
    if let Some(task) = grok_task {
        let result = task.await.unwrap_or_else(|_| {
            Err("The Grok collector stopped unexpectedly. Refresh to retry.".into())
        });
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if inner.settings_generation == settings_generation {
            let storage_error = result
                .as_ref()
                .ok()
                .and_then(|snapshot| inner.store.save(snapshot).err());
            inner
                .grok_schedule
                .finish(collector::now(), result.is_err(), visible);
            inner.dashboard.apply_grok_result(result);
            if let Some(message) = storage_error {
                inner.dashboard.grok_error = Some(format!(
                    "{message} Current values are shown but may not survive restart."
                ));
            }
            publish_dashboard(&app, &inner.dashboard);
        }
    }
    let mut claude_reading = None;
    if let Some(task) = claude_task {
        let result = task.await.unwrap_or_else(|_| {
            Err("Claude's account usage reader stopped. Saved values were retained.".into())
        });
        claude_reading = result.as_ref().ok().cloned();
        {
            let mut inner = state
                .0
                .lock()
                .map_err(|_| "The collector state is unavailable.")?;
            if inner.settings_generation == settings_generation {
                inner
                    .claude_schedule
                    .finish(collector::now(), result.is_err(), false);
                inner.claude_schedule.next = inner.claude_schedule.next.max(collector::now() + 300);
                inner.dashboard.claude_usage_error = match result {
                    Ok(snapshot) => {
                        match inner.store.ingest_claude_usage(snapshot, collector::now()) {
                            Ok(snapshot) => {
                                inner.dashboard.claude = snapshot;
                                None
                            }
                            Err(message) => Some(message),
                        }
                    }
                    Err(message) => Some(message),
                };
                publish_dashboard(&app, &inner.dashboard);
            }
        }
    }
    let mut antigravity_reading = None;
    if let Some(task) = antigravity_task {
        let mut result = task.await.unwrap_or_else(|_| {
            Err("The Antigravity collector stopped unexpectedly. Refresh to retry.".into())
        });
        antigravity_reading = result.as_ref().ok().cloned();
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if inner.settings_generation == settings_generation {
            let storage_error = result.as_mut().ok().and_then(|snapshot| {
                inner
                    .store
                    .save(snapshot)
                    .and_then(|()| inner.store.attach_usage_history(snapshot))
                    .err()
            });
            inner
                .antigravity_schedule
                .finish(collector::now(), result.is_err(), visible);
            inner.antigravity_schedule.next =
                inner.antigravity_schedule.next.max(collector::now() + 300);
            inner.dashboard.apply_antigravity_result(result);
            if let Some(message) = storage_error {
                inner.dashboard.antigravity_error = Some(format!(
                    "{message} Current values are shown but may not survive restart."
                ));
            }
            publish_dashboard(&app, &inner.dashboard);
        }
    }
    let claude_log_account = claude_reading
        .as_ref()
        .and_then(|snapshot| snapshot["accountId"].as_str());
    let antigravity_log_account = antigravity_reading
        .as_ref()
        .and_then(|snapshot| snapshot["accountId"].as_str());
    for (kind, account) in [
        (Kind::Codex, codex_log_account.as_deref()),
        (Kind::Claude, claude_log_account),
        (Kind::AntigravityCli, antigravity_log_account),
        (Kind::AntigravityDesktop, antigravity_log_account),
        (Kind::AntigravityIde, antigravity_log_account),
    ] {
        read_local_logs(
            &state,
            kind,
            log_scan_generation,
            account,
            || local_logs::root(kind),
            async {
                match kind {
                    Kind::Codex => collector::current_codex_limits().await,
                    Kind::Claude => match claude::current_account_id().await {
                        Some(id) => Ok(serde_json::json!({"accountId": id})),
                        None => Err("Could not confirm the current Claude account.".into()),
                    },
                    Kind::AntigravityCli | Kind::AntigravityDesktop | Kind::AntigravityIde => {
                        antigravity::collect().await
                    }
                }
            },
        )
        .await;
    }
    if let Ok(mut inner) = state.0.lock() {
        if let Some(mut snapshot) = inner.dashboard.antigravity.clone() {
            if inner.store.attach_usage_history(&mut snapshot).is_ok() {
                inner.dashboard.antigravity = Some(snapshot);
            }
        }
    }
    let me = {
        let inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        usage_relay::Device {
            name: device_name(&inner.store),
            details: format!(
                "m-dash {};logs={};feed={}",
                app.package_info().version,
                u8::from(inner.dashboard.settings.local_logs),
                u8::from(inner.dashboard.claude_enabled)
            ),
        }
    };
    let relay = usage_relay::sync(
        |apply| {
            let mut inner = state
                .0
                .lock()
                .map_err(|_| "The collector state is unavailable.".to_string())?;
            apply(&mut inner.store)
        },
        me,
    )
    .await;
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    inner.dashboard.refreshing = false;
    inner.dashboard.usage_relay_error = match relay {
        Ok((connected, warning)) => {
            inner.dashboard.usage_channel = connected;
            warning
        }
        Err(error) => Some(error),
    };
    let now = collector::now();
    if now >= inner.last_prune.saturating_add(86_400) {
        let _ = inner.store.prune(now);
        inner.last_prune = now;
    }
    update_forecasts(&mut inner);
    update_consistency(&mut inner, now);
    inner.update_next_refresh();
    publish_dashboard(&app, &inner.dashboard);
    let dashboard = inner.dashboard.clone();
    if let Some(snapshot) = claude_reading.filter(|_| !inner.ping_running) {
        inner.ping_running = true;
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            let result = maybe_ping_claude(&handle, &snapshot, settings_generation).await;
            if let Ok(mut inner) = handle.state::<AppState>().0.lock() {
                inner.ping_running = false;
                if let Err(message) = result {
                    inner.ping.message = message;
                    inner.dashboard.claude_ping = inner.ping.status();
                    publish_dashboard(&handle, &inner.dashboard);
                }
            };
        });
    }
    Ok(dashboard)
}

fn ledger_snapshots(
    dashboard: &Dashboard,
) -> impl Iterator<Item = (&'static str, &serde_json::Value, burn::Provider)> {
    let providers = &dashboard.settings.providers;
    [
        (
            "codex",
            providers.codex,
            &dashboard.snapshot,
            burn::Provider::Codex,
        ),
        (
            "claude",
            providers.claude,
            &dashboard.claude,
            burn::Provider::Claude,
        ),
        (
            "antigravity",
            providers.antigravity,
            &dashboard.antigravity,
            burn::Provider::Antigravity,
        ),
    ]
    .into_iter()
    .filter_map(|(name, enabled, snapshot, provider)| {
        snapshot
            .as_ref()
            .filter(|_| enabled)
            .map(|snapshot| (name, snapshot, provider))
    })
}

/// Rebuilds the burn ledgers for the enabled, signed-in Codex and Claude accounts.
fn update_consistency(inner: &mut Inner, now: u64) {
    let mut ledgers = serde_json::Map::new();
    for (name, snapshot, provider) in ledger_snapshots(&inner.dashboard) {
        if let Ok(ledger) =
            inner
                .store
                .consistency(provider, snapshot, now, inner.dashboard.ledger_days)
        {
            ledgers.insert(name.into(), ledger);
        }
    }
    inner.dashboard.consistency = serde_json::Value::Object(ledgers);
    if let Ok(mut computers) = inner.store.computers() {
        computers["name"] = serde_json::json!(device_name(&inner.store));
        inner.dashboard.computers = computers;
    }
}

#[tauri::command]
async fn review_away_usage(
    app: tauri::AppHandle,
    start: i64,
    end: i64,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        let now = collector::now();
        let mut accounts = serde_json::Map::new();
        for (name, snapshot, provider) in ledger_snapshots(&inner.dashboard) {
            accounts.insert(
                name.into(),
                inner
                    .store
                    .away_usage(provider, snapshot, now, start..=end)?,
            );
        }
        Ok(serde_json::Value::Object(accounts))
    })
    .await
    .map_err(|_| "Could not review allowance usage.")?
}

/// This computer's name on the usage channel: the owner's choice, or the system's name.
fn device_name(store: &store::Store) -> String {
    store
        .setting("device-name")
        .ok()
        .flatten()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(system_name)
}

fn system_name() -> String {
    #[cfg(target_os = "macos")]
    let name = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok());
    #[cfg(windows)]
    let name = std::env::var("COMPUTERNAME").ok();
    #[cfg(not(any(windows, target_os = "macos")))]
    let name = std::fs::read_to_string("/etc/hostname").ok();
    name.map(|name| name.trim().chars().take(64).collect::<String>())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "This computer".into())
}

/// A page of the account's usage rows, or with `csv` every matching row as CSV text.
#[tauri::command]
fn usage_list(
    app: tauri::AppHandle,
    provider: String,
    filter: store::UsageFilter,
    csv: bool,
) -> Result<serde_json::Value, String> {
    let state = app.state::<AppState>();
    let inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    let account = ledger_account(&inner, &provider)?;
    inner
        .store
        .usage_list(&account, &filter, collector::now(), csv)
}

#[tauri::command]
fn set_device_name(app: tauri::AppHandle, name: String) -> Result<Dashboard, String> {
    let name = name.trim();
    if name.chars().count() > 64 {
        return Err("Use a name of up to 64 characters.".into());
    }
    let state = app.state::<AppState>();
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    inner.store.save_setting("device-name", name)?;
    update_consistency(&mut inner, collector::now());
    publish_dashboard(&app, &inner.dashboard);
    Ok(inner.dashboard.clone())
}

// Local logs carry no identity. Only this refresh can supply the account for an import.
fn refreshed_account(result: &Result<serde_json::Value, String>) -> Option<String> {
    result.as_ref().ok()?["accountId"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

/// Best-effort: an unreadable log leaves its saved position and is retried on the next pass.
async fn read_local_logs(
    state: &AppState,
    kind: Kind,
    generation: u64,
    confirmed: Option<&str>,
    root: impl FnOnce() -> Option<PathBuf>,
    recheck: impl std::future::Future<Output = Result<serde_json::Value, String>>,
) {
    let prepared = (|| {
        let inner = state.0.lock().ok()?;
        let account = inner.local_log_account(kind, generation, confirmed)?;
        Some((account, inner.store.log_cursors(kind.source()).ok()?))
    })();
    let Some((account, cursors)) = prepared else {
        return;
    };
    let Some(root) = root() else { return };
    let now = collector::now();
    let Ok(scan) =
        tauri::async_runtime::spawn_blocking(move || local_logs::scan(kind, &root, &cursors, now))
            .await
    else {
        return;
    };
    if scan.cursors.is_empty()
        || !state.0.lock().is_ok_and(|inner| {
            inner
                .local_log_account(kind, generation, confirmed)
                .is_some()
        })
    {
        return;
    }
    // The CLI sign-in can change while the blocking file read is in progress.
    let current = recheck.await;
    let confirmed = current
        .as_ref()
        .ok()
        .and_then(|snapshot| snapshot["accountId"].as_str());
    if let Ok(mut inner) = state.0.lock() {
        let _ = inner.finish_log_scan(kind, account, generation, confirmed, scan);
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app,_,_| {
            let _ = open_dashboard(app.clone());
        }))
        .manage(CompactState::default())
        .manage(mobile_sync::State::default())
        .invoke_handler(tauri::generate_handler![mobile_sync::sync_relay,mobile_sync::sync_status,mobile_sync::sync_pair,mobile_sync::sync_approve,mobile_sync::sync_revoke,get_dashboard, get_compact_dashboard, set_ledger_days, add_ledger_mark, remove_ledger_mark, set_device_name, review_away_usage, usage_list, open_dashboard, refresh_dashboard,save_settings,set_claude_enabled,set_claude_ping,connect_openrouter,disconnect_openrouter,open_openrouter_settings,usage_relay::usage_relay_status,usage_relay::usage_relay_create,usage_relay::usage_relay_join,usage_relay::usage_relay_leave])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            macos_lifecycle::install(app.handle());
            #[cfg(target_os = "linux")]
            if let Some(strip) = app.get_webview_window("compact") {
                strip.with_webview(|webview| {
                    use gtk::prelude::*;
                    // WebKitGTK otherwise imposes a 200px minimum on the fixed-height strip.
                    webview.inner().set_size_request(528, 88);
                })?;
            }
            let directory = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&directory)?;
            let mut store = store::Store::open(&directory.join("usage.sqlite"))
                .map_err(std::io::Error::other)?;
            // Best-effort: a failed prune rolls back and leaves history intact.
            let _ = store.prune(collector::now());
            let mut snapshot = store.latest("codex").map_err(std::io::Error::other)?;
            if let Some(snapshot) = &mut snapshot { store.attach_history(snapshot).map_err(std::io::Error::other)?; }
            let mut openrouter = store.latest("openrouter").map_err(std::io::Error::other)?;
            if let Some(snapshot) = &mut openrouter { store.attach_history(snapshot).map_err(std::io::Error::other)?; }
            let credential = credentials::read();
            let openrouter_configured = credential.as_ref().is_ok_and(|key|key.is_some());
            let openrouter_error = credential.err();
            let grok = store.latest("grok").map_err(std::io::Error::other)?;
            let mut antigravity = store.latest("antigravity").map_err(std::io::Error::other)?;
            if let Some(snapshot) = &mut antigravity {
                let _ = store.attach_usage_history(snapshot);
            }
            let mut claude = store.claude_snapshot().map_err(std::io::Error::other)?;
            let receiver = claude_setup::settings_path().and_then(|path| claude_setup::prepare_receiver(&directory, &path, claude_setup::PORT));
            let claude_enabled = receiver.is_ok() && claude_setup::settings_path().and_then(|path| claude_setup::enabled(&directory,&path)).unwrap_or(false);
            let previous_settings = store.settings().map_err(std::io::Error::other)?;
            let existing = Providers { codex: snapshot.is_some(), claude: claude.is_some() || claude_enabled, grok: grok.is_some(), openrouter: openrouter.is_some() || openrouter_configured, antigravity: antigravity.is_some() };
            let needs_setup = previous_settings.is_none() && !existing.any();
            let settings = previous_settings.unwrap_or_else(|| if existing.any() { Settings::migrated(existing) } else { Settings::default() });
            if !needs_setup { store.save_settings(&settings).map_err(std::io::Error::other)?; }
            if settings.providers.claude {
                if let Ok(snapshot)=claude::read_usage_cache() {
                    store.ingest_claude_usage(snapshot,collector::now()).map_err(std::io::Error::other)?;
                    claude = store.claude_snapshot().map_err(std::io::Error::other)?;
                }
            }
            let claude_receiver_ready = receiver.is_ok();
            let claude_error = match &receiver {
                Ok((_, config)) if config.suspended => Some(claude_setup::FEED_SUSPENDED.into()),
                Ok(_) => None,
                Err(message) => Some(format!("{message} Quit Claude Code before trying to recover the local feed.")),
            };
            let observations = match &snapshot {
                Some(value) => store
                    .observation_count(value["accountId"].as_str().unwrap_or_default())
                    .map_err(std::io::Error::other)?,
                None => 0,
            };
            let ping = store.claude_ping().unwrap_or_else(|message| {
                let mut state = claude_ping::State::default(); state.message = message; state
            });
            app.manage(AppState(Mutex::new(Inner {
                dashboard: Dashboard {
                    settings,
                    needs_setup,
                    snapshot,
                    grok,
                    claude,
                    openrouter,
                    antigravity,
                    openrouter_configured,
                    openrouter_error,
                    claude_enabled,
                    claude_ping: ping.status(),
                    claude_error,
                    observations,
                    ledger_days: store.ledger_days(),
                    ..Default::default()
                },
                store,
                codex_schedule: Schedule::default(),
                grok_schedule: Schedule::default(),
                claude_schedule: Schedule::default(),
                openrouter_schedule: Schedule::default(),
            antigravity_schedule: Schedule::default(),
                openrouter_setup:false,
                openrouter_generation:0,
                openrouter_retry_after:0,
                directory,
                claude_receiver_ready,
                settings_generation: 0,
                log_scan_generation: 0,
                ping,
                ping_generation: 0,
                ping_running: false,
                last_feed_forecast: 0,
                last_prune: collector::now(),
            })));
            if let Ok((listener, bridge)) = receiver {
                let handle = app.handle().clone();
                let receive:claude_server::Receiver = Arc::new(move |kind,value| {
                    let state = handle.state::<AppState>();
                    let mut inner = state.0.lock().map_err(|_| "The local store is unavailable.")?;
                    if !inner.dashboard.settings.providers.claude || !inner.dashboard.claude_enabled { return Ok(()) }
                    let result = if kind == "logs" { inner.store.ingest_claude_telemetry(value,collector::now()) }
                        else { inner.store.ingest_claude_statusline(value,collector::now()) };
                    match &result {
                        Ok(snapshot) => { inner.dashboard.claude = snapshot.clone(); inner.dashboard.claude_error = None; }
                        Err(message) => inner.dashboard.claude_error = Some(message.clone()),
                    }
                    let now=collector::now();
                    if now>=inner.last_feed_forecast.saturating_add(30) {
                        update_forecasts(&mut inner);
                        inner.last_feed_forecast=now;
                    }
                    publish_dashboard(&handle,&inner.dashboard);
                    result.map(|_|())
                });
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let result = match tokio::net::TcpListener::from_std(listener) {
                        Ok(listener) => axum::serve(listener,claude_server::router(bridge.token,receive)).await,
                        Err(error) => Err(error),
                    };
                    if result.is_err() {
                        let state = handle.state::<AppState>();
                        if let Ok(mut inner) = state.0.lock() {
                            inner.claude_receiver_ready = false;
                            inner.dashboard.claude_enabled = false;
                            let cleanup = claude_setup::settings_path().and_then(|path| claude_setup::suspend(&inner.directory, &path));
                            inner.dashboard.claude_error = Some(match cleanup {
                                Ok(_) => "The local Claude receiver stopped and its feed was disabled for safety. Quit Claude Code and restart m-dash before enabling it again.".into(),
                                Err(message) => format!("The local Claude receiver stopped; its settings could not be fully removed. Quit Claude Code and repair its local feed settings before restarting: {message}"),
                            });
                            publish_dashboard(&handle,&inner.dashboard);
                        };
                    }
                });
            }
            let show = MenuItem::with_id(app, "show", "Open m-dash", true, None::<&str>)?;
            let refresh_item =
                MenuItem::with_id(app, "refresh", "Refresh accounts", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit m-dash", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &refresh_item, &quit])?;
            let tray = TrayIconBuilder::with_id("accounts")
                .icon(compact::tray_image(&compact::allowances(&Dashboard::default(),collector::now())))
                .tooltip("m-dash · AI resources")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray,event| {
                    if let TrayIconEvent::Click { button:MouseButton::Left, button_state:MouseButtonState::Up, .. } = event {
                        let _ = open_dashboard(tray.app_handle().clone());
                    }
                })
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        let _ = open_dashboard(app.clone());
                    }
                    "refresh" => {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = refresh_dashboard(handle).await;
                        });
                    }
                    "quit" => app.exit(0),
                    _ => (),
                }).build(app);
            #[cfg(target_os = "linux")]
            if tray.is_err() { eprintln!("The desktop tray is unavailable. Closing m-dash will quit; minimize for the compact strip."); }
            #[cfg(not(target_os = "linux"))]
            tray?;
            let sync_handle=app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let dashboard=sync_handle.state::<AppState>().0.lock().ok().map(|i|i.dashboard.clone());
                    if let Some(dashboard)=dashboard {
                        let result=mobile_sync::publish(&sync_handle.state::<mobile_sync::State>(),&dashboard).await;
                        let _=sync_handle.emit("mobile-sync-status",result.err());
                    }
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let due = {
                        let state = handle.state::<AppState>();
                        let inner = match state.0.lock() {
                            Ok(inner) => inner,
                            Err(_) => break,
                        };
                        publish_compact(&handle,&inner.dashboard);
                        !inner.dashboard.refreshing
                            && inner.dashboard.settings.providers.any()
                            && inner
                                .dashboard
                                .next_refresh
                                .is_none_or(|time| time <= collector::now())
                    };
                    if due {
                        let _ = refresh(handle.clone(), false).await;
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                #[cfg(target_os = "linux")]
                if !desktop_tray_available(window.app_handle()) {
                    window.app_handle().exit(0);
                    return;
                }
                if window.hide().is_ok() {
                    api.prevent_close();
                    if window.label() == "main" {
                        if let Some(strip) = window.get_webview_window("compact") { let _ = strip.hide(); }
                    }
                }
            } else if window.label() == "main" && matches!(event, tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(true)) {
                if let Err(error) = sync_compact_window(window) { eprintln!("Could not update the compact window: {error}"); }
            }
        })
        .build(tauri::generate_context!())
        .expect("m-dash could not start")
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            match _event {
                tauri::RunEvent::Reopen { .. } => { let _ = open_dashboard(_app.clone()); }
                tauri::RunEvent::Exit => macos_lifecycle::remove(),
                _ => (),
            }
        });
}

#[cfg(test)]
mod log_consent_tests {
    use super::*;
    use serde_json::json;

    fn test_inner(store: store::Store, dashboard: Dashboard) -> Inner {
        Inner {
            store,
            dashboard,
            codex_schedule: Schedule::default(),
            grok_schedule: Schedule::default(),
            claude_schedule: Schedule::default(),
            openrouter_schedule: Schedule::default(),
            antigravity_schedule: Schedule::default(),
            openrouter_setup: false,
            openrouter_generation: 0,
            openrouter_retry_after: 0,
            directory: PathBuf::new(),
            claude_receiver_ready: false,
            settings_generation: 0,
            log_scan_generation: 0,
            ping: claude_ping::State::default(),
            ping_generation: 0,
            ping_running: false,
            last_feed_forecast: 0,
            last_prune: 0,
        }
    }

    #[test]
    fn revoking_consent_discards_scanned_rows_and_cursors_even_after_reenabling() {
        let account = "codex:fixture";
        let mut settings = Settings {
            providers: Providers {
                codex: true,
                ..Default::default()
            },
            local_logs: true,
            ..Default::default()
        };
        let mut inner = test_inner(
            store::Store::open(std::path::Path::new(":memory:")).unwrap(),
            Dashboard {
                settings: settings.clone(),
                snapshot: Some(json!({"accountId": account})),
                ..Default::default()
            },
        );
        let scan = || local_logs::Scan {
            requests: vec![("fixture-request".into(), 100, usage::Request::default())],
            cursors: vec![(
                "fixture.jsonl".into(),
                local_logs::Cursor {
                    offset: 42,
                    ..Default::default()
                },
            )],
            ..Default::default()
        };
        let assert_empty = |inner: &Inner| {
            assert_eq!(
                inner
                    .store
                    .usage_list(account, &Default::default(), 100, false)
                    .unwrap()["total"],
                0
            );
            assert!(inner
                .store
                .log_cursors(Kind::Codex.source())
                .unwrap()
                .is_empty());
        };
        let started = inner.log_scan_generation;
        settings.local_logs = false;
        inner.apply_settings(settings.clone());
        inner
            .finish_log_scan(
                Kind::Codex,
                account,
                inner.log_scan_generation,
                Some(account),
                scan(),
            )
            .unwrap();
        assert_empty(&inner);
        settings.local_logs = true;
        inner.apply_settings(settings);
        inner
            .finish_log_scan(Kind::Codex, account, started, Some(account), scan())
            .unwrap();
        assert_empty(&inner);
        inner
            .finish_log_scan(
                Kind::Codex,
                account,
                inner.log_scan_generation,
                Some(account),
                scan(),
            )
            .unwrap();
        assert_eq!(
            inner
                .store
                .usage_list(account, &Default::default(), 100, false)
                .unwrap()["total"],
            1
        );
        assert_eq!(
            inner.store.log_cursors(Kind::Codex.source()).unwrap()["fixture.jsonl"].offset,
            42
        );
    }

    #[test]
    fn restart_waits_for_claude_identity_before_consuming_pending_transcript_rows() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("usage.sqlite");
        let file = directory.path().join("session.jsonl");
        let a = r#"claude:["a","org"]"#;
        let b = r#"claude:["b","org"]"#;
        let kind = Kind::Claude;
        let now = 40 * 86_400;
        let scan = |store: &store::Store| {
            local_logs::scan(
                kind,
                directory.path(),
                &store.log_cursors(kind.source()).unwrap(),
                now,
            )
        };
        let line = |id: &str| {
            format!(
                "{}\n",
                json!({"timestamp":"1970-02-10T00:00:00Z",
            "type":"assistant","requestId":id,"message":{"model":"fixture","usage":{"input_tokens":7}}})
            )
        };
        let first = line("saved-a");
        std::fs::write(&file, &first).unwrap();
        {
            let mut store = store::Store::open(&database).unwrap();
            store.ingest_claude_telemetry(json!({"resourceLogs":[{"resource":{"attributes":[
                {"key":"service.name","value":{"stringValue":"claude-code"}}]},"scopeLogs":[{"logRecords":[{"attributes":[
                {"key":"user.account_uuid","value":{"stringValue":"a"}},{"key":"organization.id","value":{"stringValue":"org"}},
                {"key":"session.id","value":{"stringValue":"old-session"}}]}]}]}]}), 1000).unwrap();
            store
                .ingest_local_logs(kind.source(), a, scan(&store))
                .unwrap();
            store.prune(now).unwrap();
        }
        std::fs::write(&file, first + &line("pending-b")).unwrap();
        let store = store::Store::open(&database).unwrap();
        let failed = claude::normalize_usage_cache(
            json!({"oauthAccount":{"accountUuid":"b","organizationUuid":"org"},
            "cachedUsageUtilization":{"accountUuid":"a"}}),
            now,
        );
        assert!(failed.is_err());
        let dashboard = Dashboard {
            claude: store.claude_snapshot().unwrap(),
            claude_usage_error: failed.as_ref().err().cloned(),
            settings: Settings {
                local_logs: true,
                providers: Providers {
                    claude: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let inner = test_inner(store, dashboard);
        assert_eq!(inner.dashboard.claude.as_ref().unwrap()["accountId"], a);
        assert_eq!(scan(&inner.store).requests[0].0, "claude:pending-b");
        let generation = inner.log_scan_generation;
        let state = AppState(Mutex::new(inner));
        let rows = |account: &str| {
            state
                .0
                .lock()
                .unwrap()
                .store
                .usage_list(account, &Default::default(), now, false)
                .unwrap()
        };
        let positions = || {
            state
                .0
                .lock()
                .unwrap()
                .store
                .log_cursors(kind.source())
                .unwrap()
        };
        let saved_a = rows(a);
        let saved_b = rows(b);
        assert_eq!(saved_a["total"], 1);
        assert_eq!(saved_b["total"], 0);
        let cursors = positions();
        let confirmed = failed
            .as_ref()
            .ok()
            .and_then(|snapshot| snapshot["accountId"].as_str());
        tauri::async_runtime::block_on(read_local_logs(
            &state,
            kind,
            generation,
            confirmed,
            || panic!("An unconfirmed account must not start a transcript scan"),
            std::future::ready(failed.clone()),
        ));
        assert_eq!(rows(a), saved_a);
        assert_eq!(rows(b), saved_b);
        assert_eq!(positions(), cursors);

        let reading_b = json!({"accountId":b,"quotaObservedAt":now,"windows":[]});
        {
            let mut inner = state.0.lock().unwrap();
            inner.dashboard.claude = inner
                .store
                .ingest_claude_usage(reading_b.clone(), now)
                .unwrap();
            inner.dashboard.claude_usage_error = None;
        }
        let scan_as_b = |recheck| {
            tauri::async_runtime::block_on(read_local_logs(
                &state,
                kind,
                generation,
                reading_b["accountId"].as_str(),
                || Some(directory.path().into()),
                std::future::ready(recheck),
            ))
        };
        for recheck in [failed, Ok(json!({"accountId":a}))] {
            scan_as_b(recheck);
            assert_eq!(rows(a), saved_a);
            assert_eq!(rows(b), saved_b);
            assert_eq!(positions(), cursors);
        }
        scan_as_b(Ok(reading_b.clone()));
        assert_eq!(rows(a), saved_a);
        assert_eq!(rows(b)["total"], 1);
        let key = if cfg!(windows) {
            file.to_string_lossy().to_lowercase()
        } else {
            file.to_string_lossy().into_owned()
        };
        assert_eq!(positions()[&key].offset, file.metadata().unwrap().len());
    }
}
