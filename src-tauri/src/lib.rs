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
#[cfg(target_os = "macos")]
mod macos_lifecycle;
pub mod mobile_sync;
pub mod openrouter;
pub mod settings;
pub mod store;

use dashboard::{Dashboard, Schedule};
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
    openrouter_setup: bool,
    openrouter_generation: u64,
    openrouter_retry_after: u64,
    directory: PathBuf,
    claude_receiver_ready: bool,
    settings_generation: u64,
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
    if inner.dashboard.settings.providers != settings.providers {
        // Ignore reads started under old choices, including disable/re-enable races.
        inner.settings_generation += 1;
        inner.codex_schedule = Schedule::default();
        inner.grok_schedule = Schedule::default();
        // Keep Claude's five-minute minimum and OpenRouter's server retry delay.
    }
    inner.dashboard.settings = settings;
    inner.dashboard.needs_setup = false;
    inner.update_next_refresh();
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
        openrouter_generation,
        settings_generation,
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
        if !read_codex && !read_grok && !read_claude && !read_openrouter {
            return Ok(inner.dashboard.clone());
        }
        inner.dashboard.refreshing = true;
        inner.dashboard.last_attempt = Some(now);
        // A settings change may discard this result; retain the polling cooldown.
        if read_claude {
            inner.claude_schedule.next = now.saturating_add(300);
        }
        publish_dashboard(&app, &inner.dashboard);
        (
            read_codex,
            read_grok,
            read_claude,
            read_openrouter,
            inner.openrouter_generation,
            inner.settings_generation,
        )
    };
    let grok_task = read_grok.then(|| tauri::async_runtime::spawn(collector::collect_grok()));
    let claude_task = read_claude.then(|| tauri::async_runtime::spawn(claude::collect()));
    let openrouter_task =
        read_openrouter.then(|| tauri::async_runtime::spawn(openrouter::collect()));
    let visible = app.get_webview_window("main").is_some_and(|window| {
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true)
    });
    if read_codex {
        let mut result = collector::collect().await;
        let mut inner = state
            .0
            .lock()
            .map_err(|_| "The collector state is unavailable.")?;
        if inner.settings_generation == settings_generation {
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
    let mut ping_snapshot = None;
    if let Some(task) = claude_task {
        let result = task.await.unwrap_or_else(|_| {
            Err("Claude's account usage reader stopped. Saved values were retained.".into())
        });
        ping_snapshot = result.as_ref().ok().cloned();
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
    let mut inner = state
        .0
        .lock()
        .map_err(|_| "The collector state is unavailable.")?;
    inner.dashboard.refreshing = false;
    let now = collector::now();
    if now >= inner.last_prune.saturating_add(86_400) {
        let _ = inner.store.prune(now);
        inner.last_prune = now;
    }
    update_forecasts(&mut inner);
    inner.update_next_refresh();
    publish_dashboard(&app, &inner.dashboard);
    let dashboard = inner.dashboard.clone();
    if let Some(snapshot) = ping_snapshot.filter(|_| !inner.ping_running) {
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app,_,_| {
            let _ = open_dashboard(app.clone());
        }))
        .manage(CompactState::default())
        .manage(mobile_sync::State::default())
        .invoke_handler(tauri::generate_handler![mobile_sync::sync_relay,mobile_sync::sync_status,mobile_sync::sync_pair,mobile_sync::sync_approve,mobile_sync::sync_revoke,get_dashboard, get_compact_dashboard, open_dashboard, refresh_dashboard,save_settings,set_claude_enabled,set_claude_ping,connect_openrouter,disconnect_openrouter,open_openrouter_settings])
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
            let mut claude = store.claude_snapshot().map_err(std::io::Error::other)?;
            let receiver = claude_setup::settings_path().and_then(|path| claude_setup::prepare_receiver(&directory, &path, claude_setup::PORT));
            let claude_enabled = receiver.is_ok() && claude_setup::settings_path().and_then(|path| claude_setup::enabled(&directory,&path)).unwrap_or(false);
            let previous_settings = store.settings().map_err(std::io::Error::other)?;
            let existing = Providers { codex: snapshot.is_some(), claude: claude.is_some() || claude_enabled, grok: grok.is_some(), openrouter: openrouter.is_some() || openrouter_configured };
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
                    openrouter_configured,
                    openrouter_error,
                    claude_enabled,
                    claude_ping: ping.status(),
                    claude_error,
                    observations,
                    ..Default::default()
                },
                store,
                codex_schedule: Schedule::default(),
                grok_schedule: Schedule::default(),
                claude_schedule: Schedule::default(),
                openrouter_schedule: Schedule::default(),
                openrouter_setup:false,
                openrouter_generation:0,
                openrouter_retry_after:0,
                directory,
                claude_receiver_ready,
                settings_generation: 0,
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
