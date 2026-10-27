use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub const PORT: u16 = 43183;
pub const FEED_SUSPENDED: &str = "The Claude local feed was disabled for safety. Quit Claude Code, enable the feed again, then reopen Claude Code.";

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub token: String,
    env_existed: Option<bool>,
    #[serde(default)]
    pub suspended: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cleanup_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managed_command: Option<String>,
}

pub fn settings_path() -> Result<PathBuf, String> {
    let directory = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|path| PathBuf::from(path).join(".claude"))
        })
        .ok_or("Cannot find your Claude settings directory.")?;
    Ok(directory.join("settings.json"))
}

pub fn load(directory: &Path) -> Result<Config, String> {
    std::fs::create_dir_all(directory)
        .map_err(|_| "Cannot create the local Claude bridge directory.")?;
    let path = directory.join("claude-bridge.json");
    if path.exists() {
        let value =
            std::fs::read(path).map_err(|_| "Cannot read the Claude bridge configuration.")?;
        serde_json::from_slice(&value)
            .map_err(|_| "The Claude bridge configuration is invalid.".into())
    } else {
        let config = Config {
            token: uuid::Uuid::new_v4().to_string(),
            env_existed: None,
            suspended: false,
            cleanup_token: None,
            managed_command: None,
        };
        write_atomic(
            &path,
            &serde_json::to_vec_pretty(&config).map_err(|_| "Cannot encode bridge settings.")?,
        )?;
        Ok(config)
    }
}

pub fn prepare_receiver(
    directory: &Path,
    settings_path: &Path,
    port: u16,
) -> Result<(std::net::TcpListener, Config), String> {
    adopt_orphaned_feed(directory, settings_path)?;
    let mut config = load(directory)?;
    let was_enabled = if config.suspended || config.env_existed.is_some() {
        let was_enabled = suspend(directory, settings_path)?;
        config = load(directory)?;
        was_enabled
    } else {
        false
    };
    let receiver =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).and_then(|listener| {
            listener.set_nonblocking(true)?;
            Ok(listener)
        });
    match receiver {
        Ok(listener) => {
            // Never serve a credential that could have been captured during downtime.
            config.token = uuid::Uuid::new_v4().to_string();
            save_config(directory, &config)?;
            if was_enabled {
                if let Err(message) = set_enabled(directory, settings_path, true) {
                    let cleanup = suspend(directory, settings_path);
                    return Err(match cleanup {
                        Ok(_) => format!("{message} {FEED_SUSPENDED}"),
                        Err(cleanup) => {
                            format!("{message} Local feed cleanup also failed: {cleanup}")
                        }
                    });
                }
                config.suspended = false;
            }
            Ok((listener, config))
        }
        Err(_) => {
            suspend(directory, settings_path)?;
            Err("The local Claude receiver could not start, so its feed was disabled for safety. Quit Claude Code and restart m-dash before enabling the feed again.".into())
        }
    }
}

// Deleting the data directory (the Windows uninstaller offers this) loses the bridge
// configuration but leaves m-dash's entries in Claude's settings, still carrying the old
// token. Without this, the feed would look enabled while every upload is rejected, and
// disabling it would refuse to touch the unrecognized token. Only this directory's exact
// statusline and the receiver's endpoint shape are reclaimed; anything else is left alone.
fn adopt_orphaned_feed(directory: &Path, settings_path: &Path) -> Result<(), String> {
    if directory.join("claude-bridge.json").exists() {
        return Ok(());
    }
    let Ok(settings) = read_settings(settings_path) else {
        return Ok(());
    };
    let status = json!({"type":"command","command":command(directory)?});
    if settings["statusLine"] != status {
        return Ok(());
    }
    let Some(token) = settings["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"]
        .as_str()
        .and_then(|endpoint| endpoint.strip_prefix(&format!("http://127.0.0.1:{PORT}/claude/")))
        .and_then(|rest| rest.strip_suffix("/logs"))
        .filter(|token| uuid::Uuid::parse_str(token).is_ok())
    else {
        return Ok(());
    };
    if managed_env(token)
        .as_object()
        .unwrap()
        .iter()
        .any(|(key, value)| settings["env"][key] != *value)
    {
        return Ok(());
    }
    std::fs::create_dir_all(directory)
        .map_err(|_| "Cannot create the local Claude bridge directory.")?;
    save_config(
        directory,
        &Config {
            token: token.to_owned(),
            // Unknown after the loss, so keep Claude's env object when the feed is removed.
            env_existed: Some(true),
            suspended: false,
            cleanup_token: None,
            managed_command: status["command"].as_str().map(str::to_owned),
        },
    )
}

fn save_config(directory: &Path, config: &Config) -> Result<(), String> {
    write_atomic(
        &directory.join("claude-bridge.json"),
        &serde_json::to_vec_pretty(config).map_err(|_| "Cannot encode bridge settings.")?,
    )
}

pub fn suspend(directory: &Path, settings_path: &Path) -> Result<bool, String> {
    let mut config = load(directory)?;
    let was_suspended = config.suspended;
    // Legacy helpers read only token. Retain the retired value solely to retry cleanup.
    if !config.token.is_empty() {
        config.cleanup_token = Some(std::mem::take(&mut config.token));
    }
    config.suspended = true;
    save_config(directory, &config)?;
    let mut settings = read_settings(settings_path)?;
    let original = settings.clone();
    let expected_status =
        json!({"type":"command","command":owned_command(directory, &config, &settings)?});
    let was_enabled = !was_suspended && settings["statusLine"] == expected_status;
    let modified_status = settings["statusLine"]["command"] == expected_status["command"]
        && settings["statusLine"] != expected_status;
    if settings["statusLine"] == expected_status {
        settings.as_object_mut().unwrap().remove("statusLine");
    }
    let managed = managed_env(config.cleanup_token.as_deref().unwrap_or(""));
    if settings["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"]
        == managed["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"]
    {
        if let Some(env) = settings["env"].as_object_mut() {
            for (key, value) in managed.as_object().unwrap() {
                if env.get(key) == Some(value) {
                    env.remove(key);
                }
            }
            if env.is_empty() && config.env_existed == Some(false) {
                settings.as_object_mut().unwrap().remove("env");
            }
        }
    }
    if settings != original {
        write_atomic(
            settings_path,
            &serde_json::to_vec_pretty(&settings).map_err(|_| "Cannot encode Claude settings.")?,
        )?;
    }
    if modified_status {
        return Err("Claude's m-dash statusline has custom fields and was preserved. Quit Claude Code and repair or remove that entry before restarting m-dash.".into());
    }
    config.cleanup_token = None;
    config.env_existed = None;
    config.managed_command = None;
    save_config(directory, &config)?;
    Ok(was_enabled)
}

fn managed_env(token: &str) -> Value {
    json!({"CLAUDE_CODE_ENABLE_TELEMETRY":"1","OTEL_LOGS_EXPORTER":"otlp",
        "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL":"http/json",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":format!("http://127.0.0.1:{PORT}/claude/{token}/logs")})
}

fn quote(path: &Path) -> String {
    #[cfg(windows)]
    {
        format!("'{}'", path.to_string_lossy().replace('\'', "''"))
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }
}

fn command(directory: &Path) -> Result<String, String> {
    let executable =
        std::env::current_exe().map_err(|_| "Cannot locate the installed m-dash executable.")?;
    let invocation = format!(
        "{} --claude-statusline {}",
        quote(&executable),
        quote(directory)
    );
    #[cfg(windows)]
    {
        use base64::Engine;
        // One literal native invocation must survive both Git Bash and PowerShell shells.
        let bytes: Vec<u8> = format!("& {invocation}")
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        Ok(format!(
            "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }
    #[cfg(not(windows))]
    Ok(invocation)
}

fn owned_command(directory: &Path, config: &Config, settings: &Value) -> Result<String, String> {
    if let Some(command) = &config.managed_command {
        return Ok(command.clone());
    }
    if config.env_existed.is_some() {
        #[cfg(windows)]
        return Ok(format!(
            "powershell.exe -NoProfile -File \"{}\"",
            directory
                .join("claude-statusline.ps1")
                .to_string_lossy()
                .replace('\\', "/")
        ));
        #[cfg(not(windows))]
        if let Some(raw) = settings["statusLine"]["command"].as_str() {
            if let Some(exe) =
                raw.strip_suffix(&format!(" --claude-statusline {}", quote(directory)))
            {
                if let Some(path) = exe.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
                    let path = PathBuf::from(path.replace("'\\''", "'"));
                    if path.is_absolute()
                        && path.file_name().is_some_and(|name| name == "m-dash")
                        && quote(&path) == exe
                    {
                        return Ok(raw.to_owned());
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = settings;
    }
    command(directory)
}

fn read_settings(path: &Path) -> Result<Value, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .filter(|value| value.is_object())
            .ok_or_else(|| "Claude settings could not be parsed. Nothing was changed.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(_) => Err("Claude settings could not be read. Nothing was changed.".into()),
    }
}

pub fn enabled(directory: &Path, settings_path: &Path) -> Result<bool, String> {
    let config = load(directory)?;
    let settings = read_settings(settings_path)?;
    Ok(!config.suspended
        && settings["statusLine"]
            == json!({"type":"command","command":owned_command(directory,&config,&settings)?}))
}

pub fn set_enabled(directory: &Path, settings_path: &Path, enable: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if enable
        && std::env::current_exe()
            .map_err(|_| "Cannot locate m-dash.")?
            .components()
            .any(|part| part.as_os_str() == "AppTranslocation")
    {
        return Err(
            "Move m-dash to Applications and reopen it before enabling the local Claude feed."
                .into(),
        );
    }
    let mut config = load(directory)?;
    let mut settings = read_settings(settings_path)?;
    let expected_env = managed_env(&config.token);
    let expected_status = json!({"type":"command","command":command(directory)?});
    let ours = settings["statusLine"]
        == json!({"type":"command","command":owned_command(directory,&config,&settings)?});
    if enable {
        if config.token.is_empty() || (config.suspended && config.env_existed.is_some()) {
            return Err("Restart m-dash before enabling the Claude local feed again.".into());
        }
        if ours {
            return Ok(());
        }
        // shortcut: existing custom feeds need manual composition; add composition when one is configured.
        if !settings["statusLine"].is_null() {
            return Err("Claude already has a custom statusline. Its settings were preserved; compose that feed before enabling m-dash.".into());
        }
        if settings["env"].as_object().is_some_and(|env| {
            env.keys().any(|key| {
                key == "CLAUDE_CODE_ENABLE_TELEMETRY"
                    || key == "OTEL_LOGS_EXPORTER"
                    || key.starts_with("OTEL_EXPORTER_OTLP")
            })
        }) {
            return Err("Claude already has telemetry settings. They were preserved; configure forwarding before enabling this local feed.".into());
        }
        if settings.get("env").is_some_and(|value| !value.is_object()) {
            return Err("Claude's env setting is not an object. Nothing was changed.".into());
        }
        config.env_existed = Some(settings.get("env").is_some());
        config.suspended = false;
        config.managed_command = expected_status["command"].as_str().map(str::to_owned);
        write_atomic(
            &directory.join("claude-bridge.json"),
            &serde_json::to_vec_pretty(&config).map_err(|_| "Cannot encode bridge settings.")?,
        )?;
        if settings.get("env").is_none() {
            settings["env"] = json!({});
        }
        for (key, value) in expected_env.as_object().unwrap() {
            settings["env"][key] = value.clone();
        }
        settings["statusLine"] = expected_status;
    } else {
        if !ours {
            if config.env_existed.is_none() {
                return Ok(());
            }
            return Err("Claude's statusline changed after setup. It was left untouched; restore the m-dash entry before disabling.".into());
        }
        if expected_env
            .as_object()
            .unwrap()
            .iter()
            .any(|(key, value)| settings["env"][key] != *value)
        {
            return Err("Claude's telemetry settings changed after setup. They were left untouched to preserve your edits.".into());
        }
        settings.as_object_mut().unwrap().remove("statusLine");
        if let Some(env) = settings["env"].as_object_mut() {
            for key in expected_env.as_object().unwrap().keys() {
                env.remove(key);
            }
            if env.is_empty() && config.env_existed == Some(false) {
                settings.as_object_mut().unwrap().remove("env");
            }
        }
    }
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| "Cannot create the Claude settings directory.")?;
    }
    write_atomic(
        settings_path,
        &serde_json::to_vec_pretty(&settings).map_err(|_| "Cannot encode Claude settings.")?,
    )?;
    if !enable {
        config.env_existed = None;
        config.managed_command = None;
        write_atomic(
            &directory.join("claude-bridge.json"),
            &serde_json::to_vec_pretty(&config).map_err(|_| "Cannot encode bridge settings.")?,
        )?;
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    let preserve_permissions = path
        .file_name()
        .is_some_and(|name| name != "claude-bridge.json");
    let target = if path.is_symlink() {
        Some(
            std::fs::canonicalize(path)
                .map_err(|_| "Cannot resolve the Claude settings symlink. Nothing was changed.")?,
        )
    } else {
        None
    };
    let path = target.as_deref().unwrap_or(path);
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        #[cfg(unix)]
        if preserve_permissions {
            use std::os::unix::fs::MetadataExt;
            match std::fs::metadata(path) {
                Ok(previous) => {
                    if previous.uid() != file.metadata()?.uid() {
                        return Err(std::io::Error::other("Settings belong to another user"));
                    }
                    file.set_permissions(previous.permissions())?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
        }
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result.map_err(|_: std::io::Error| {
        "Could not safely save Claude bridge settings. Existing settings were retained.".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    #[test]
    fn a_feed_left_behind_by_a_deleted_data_directory_is_reclaimed_with_a_new_token() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        let settings = directory.path().join("settings.json");
        std::fs::write(&settings, r#"{"model":"opus"}"#).unwrap();
        set_enabled(&app, &settings, true).unwrap();
        let old =
            read_settings(&settings).unwrap()["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"].clone();
        std::fs::remove_dir_all(&app).unwrap();
        let (_listener, config) = prepare_receiver(&app, &settings, 0).unwrap();
        let after = read_settings(&settings).unwrap();
        assert_ne!(after["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"], old);
        assert_eq!(
            after["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"],
            managed_env(&config.token)["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"]
        );
        assert!(enabled(&app, &settings).unwrap());
        set_enabled(&app, &settings, false).unwrap();
        let after = read_settings(&settings).unwrap();
        assert!(after["statusLine"].is_null());
        assert_eq!(after["model"], "opus");
        assert_eq!(after["env"], json!({}));
        set_enabled(&app, &settings, true).unwrap();
    }
    #[test]
    fn a_foreign_feed_is_not_reclaimed_after_the_data_directory_is_lost() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        let settings = directory.path().join("settings.json");
        let foreign = json!({"statusLine":{"type":"command","command":command(&app).unwrap()},
            "env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1","OTEL_LOGS_EXPORTER":"otlp","OTEL_EXPORTER_OTLP_LOGS_PROTOCOL":"http/json",
                "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":"http://collector.example/v1/logs"}});
        std::fs::write(&settings, foreign.to_string()).unwrap();
        prepare_receiver(&app, &settings, 0).unwrap();
        assert_eq!(read_settings(&settings).unwrap(), foreign);
    }
    #[cfg(unix)]
    #[test]
    fn feed_tokens_are_private_and_settings_keep_their_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app space's");
        let settings = directory.path().join("settings.json");
        std::fs::write(&settings, "{}").unwrap();
        std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o640)).unwrap();
        set_enabled(&app, &settings, true).unwrap();
        assert_eq!(
            std::fs::metadata(app.join("claude-bridge.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&settings).unwrap().permissions().mode() & 0o777,
            0o640
        );
        set_enabled(&app, &settings, false).unwrap();
        assert_eq!(
            std::fs::metadata(&settings).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    #[cfg(unix)]
    #[test]
    fn feed_permissions_follow_logical_file_roles_through_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        load(&app).unwrap();
        let bridge = app.join("claude-bridge.json");
        let bridge_target = directory.path().join("shared.json");
        std::fs::rename(&bridge, &bridge_target).unwrap();
        std::fs::set_permissions(&bridge_target, std::fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&bridge_target, &bridge).unwrap();
        let settings = directory.path().join("settings.json");
        let settings_target = directory.path().join("claude-bridge.json");
        std::fs::write(&settings_target, "{}").unwrap();
        std::fs::set_permissions(&settings_target, std::fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&settings_target, &settings).unwrap();

        for enable in [true, false] {
            set_enabled(&app, &settings, enable).unwrap();
            assert_eq!(enabled(&app, &settings).unwrap(), enable);
            assert!(bridge.is_symlink() && settings.is_symlink());
            assert_eq!(
                std::fs::metadata(&bridge_target)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&settings_target)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o640
            );
        }
    }
    #[cfg(windows)]
    #[test]
    fn encoded_launcher_preserves_extended_windows_paths() {
        use base64::Engine;
        let directory = Path::new(r"\\?\C:\long\owner's $HOME\bridge");
        let generated = command(directory).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(generated.split_whitespace().last().unwrap())
            .unwrap();
        let utf16: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes(*b))
            .collect();
        let invocation = String::from_utf16(&utf16).unwrap();
        assert!(invocation.ends_with(r"--claude-statusline '\\?\C:\long\owner''s $HOME\bridge'"));
        assert_eq!(
            quote(Path::new(r"\\?\C:\long\m-dash.exe")),
            r"'\\?\C:\long\m-dash.exe'"
        );
    }
    #[test]
    fn toggling_feed_keeps_user_key_order() {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("settings.json");
        std::fs::write(&settings, "{\"zeta\":1,\"alpha\":2}").unwrap();
        set_enabled(&directory.path().join("app"), &settings, true).unwrap();
        let saved = std::fs::read_to_string(&settings).unwrap();
        assert!(saved.find("zeta").unwrap() < saved.find("alpha").unwrap());
        let mut value = read_settings(&settings).unwrap();
        value["later-a"] = json!(1);
        value["later-b"] = json!(2);
        std::fs::write(&settings, value.to_string()).unwrap();
        set_enabled(&directory.path().join("app"), &settings, false).unwrap();
        let saved = std::fs::read_to_string(&settings).unwrap();
        assert!(saved.find("later-a").unwrap() < saved.find("later-b").unwrap());
    }
    #[test]
    fn saved_managed_command_migrates_after_the_application_moves() {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("settings.json");
        let app = directory.path().join("app");
        set_enabled(&app, &settings, true).unwrap();
        let mut config = load(&app).unwrap();
        let moved = "old installed m-dash location --claude-statusline";
        config.managed_command = Some(moved.into());
        save_config(&app, &config).unwrap();
        let mut value = read_settings(&settings).unwrap();
        value["statusLine"]["command"] = json!(moved);
        std::fs::write(&settings, value.to_string()).unwrap();
        let (_listener, _) = prepare_receiver(&app, &settings, 0).unwrap();
        assert!(enabled(&app, &settings).unwrap());
        assert_ne!(
            read_settings(&settings).unwrap()["statusLine"]["command"],
            moved
        );
    }

    #[cfg(windows)]
    #[test]
    fn legacy_powershell_feed_migrates_without_overwriting_user_settings() {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("settings.json");
        let app = directory.path().join("app");
        set_enabled(&app, &settings, true).unwrap();
        let mut config = load(&app).unwrap();
        config.managed_command = None;
        save_config(&app, &config).unwrap();
        let legacy = format!(
            "powershell.exe -NoProfile -File \"{}\"",
            app.join("claude-statusline.ps1")
                .to_string_lossy()
                .replace('\\', "/")
        );
        let mut value = read_settings(&settings).unwrap();
        value["statusLine"]["command"] = json!(legacy);
        value["permissions"] = json!({"deny":["Read(private)"]});
        std::fs::write(&settings, value.to_string()).unwrap();
        let (_listener, _) = prepare_receiver(&app, &settings, 0).unwrap();
        assert!(enabled(&app, &settings).unwrap());
        assert_eq!(
            read_settings(&settings).unwrap()["permissions"],
            value["permissions"]
        );
        assert!(!app.join("claude-statusline.ps1").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn legacy_mac_helper_path_is_repaired_after_relocation() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app space's");
        let settings = directory.path().join("settings.json");
        set_enabled(&app, &settings, true).unwrap();
        let mut config = load(&app).unwrap();
        config.managed_command = None;
        save_config(&app, &config).unwrap();
        let mut value = read_settings(&settings).unwrap();
        value["statusLine"]["command"] = json!(format!(
            "{} --claude-statusline {}",
            quote(Path::new("/old app's/m-dash")),
            quote(&app)
        ));
        std::fs::write(&settings, value.to_string()).unwrap();
        let (_listener, _) = prepare_receiver(&app, &settings, 0).unwrap();
        assert!(enabled(&app, &settings).unwrap());
        assert_eq!(
            read_settings(&settings).unwrap()["statusLine"]["command"],
            command(&app).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn toggling_feed_preserves_a_symlinked_settings_file() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("dotfiles.json");
        let link = directory.path().join("settings.json");
        std::fs::write(&target, "{\"permissions\":{\"deny\":[\"Read(private)\"]}}").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        set_enabled(&directory.path().join("app"), &link, true).unwrap();
        assert!(link.is_symlink());
        assert!(read_settings(&target).unwrap()["statusLine"].is_object());
    }
    #[test]
    fn feed_uses_the_native_helper_without_a_powershell_policy_dependency() {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("settings.json");
        let app = directory.path().join("app");
        set_enabled(&app, &settings, true).unwrap();
        let value = read_settings(&settings).unwrap();
        let command = value["statusLine"]["command"].as_str().unwrap();
        #[cfg(windows)]
        {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(command.split_whitespace().last().unwrap())
                .unwrap();
            let utf16: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect();
            let invocation = String::from_utf16(&utf16).unwrap();
            assert_eq!(
                invocation,
                format!(
                    "& {} --claude-statusline {}",
                    quote(&std::env::current_exe().unwrap()),
                    quote(&app)
                )
            );
            assert!(!command.contains("ExecutionPolicy"));
        }
        #[cfg(not(windows))]
        assert!(command.contains(" --claude-statusline "));
        assert!(!command.contains(".ps1"));
    }

    #[test]
    fn invalid_startup_settings_retire_the_legacy_token_before_failing() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        let settings = directory.path().join("settings.json");
        set_enabled(&app, &settings, true).unwrap();
        std::fs::write(&settings, "invalid JSON").unwrap();
        assert!(prepare_receiver(&app, &settings, 0).is_err());
        let blocked = load(&app).unwrap();
        assert!(blocked.suspended);
        assert!(blocked.token.is_empty());
    }

    #[test]
    fn modified_managed_statusline_is_preserved_and_requires_manual_repair() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        let settings = directory.path().join("settings.json");
        set_enabled(&app, &settings, true).unwrap();
        let mut edited: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        edited["statusLine"]["padding"] = json!(7);
        std::fs::write(&settings, edited.to_string()).unwrap();
        assert!(set_enabled(&app, &settings, false).is_err());
        assert!(suspend(&app, &settings).is_err());
        let after: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(after["statusLine"], edited["statusLine"]);
        assert!(after["env"].is_null());
        assert!(load(&app).unwrap().token.is_empty());
        assert!(prepare_receiver(&app, &settings, 0).is_err());
        assert!(set_enabled(&app, &settings, true).is_err());
        let mut repaired = after;
        repaired.as_object_mut().unwrap().remove("statusLine");
        std::fs::write(&settings, repaired.to_string()).unwrap();
        let (_listener, _) = prepare_receiver(&app, &settings, 0).unwrap();
        assert!(!enabled(&app, &settings).unwrap());
        set_enabled(&app, &settings, true).unwrap();
        assert!(enabled(&app, &settings).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn locked_bridge_refuses_startup_and_never_reuses_the_old_token_after_repair() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        let settings = directory.path().join("settings.json");
        set_enabled(&app, &settings, true).unwrap();
        let old = load(&app).unwrap().token;
        // Allow reads but deny replacement of this temporary bridge file.
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(app.join("claude-bridge.json"))
            .unwrap();
        assert!(prepare_receiver(&app, &settings, 0).is_err());
        drop(locked);
        let (_listener, config) = prepare_receiver(&app, &settings, 0).unwrap();
        assert_ne!(config.token, old);
        assert!(enabled(&app, &settings).unwrap());
    }

    #[test]
    fn each_receiver_start_retires_prior_tokens_and_preserves_a_healthy_feed() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        let settings = directory.path().join("settings.json");
        std::fs::write(&settings, json!({"env":{"KEEP":"yes"}}).to_string()).unwrap();
        set_enabled(&app, &settings, true).unwrap();
        let old = load(&app).unwrap().token;
        let (listener, config) = prepare_receiver(&app, &settings, 0).unwrap();
        assert_ne!(
            config.token, old,
            "a token captured during downtime must never be served again"
        );
        assert!(enabled(&app, &settings).unwrap());
        let value: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(value["env"]["KEEP"], "yes");
        assert_eq!(
            value["env"]["OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"],
            format!("http://127.0.0.1:43183/claude/{}/logs", config.token)
        );
        drop(listener);
        let (_listener, next) = prepare_receiver(&app, &settings, 0).unwrap();
        assert_ne!(next.token, config.token);
        assert!(enabled(&app, &settings).unwrap());
    }

    #[test]
    fn receiver_failure_preserves_user_edits_and_disables_only_managed_delivery() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        let settings = directory.path().join("settings.json");
        std::fs::write(
            &settings,
            json!({"env":{"KEEP":"yes"},"hooks":{"Stop":["original"]}}).to_string(),
        )
        .unwrap();
        set_enabled(&app, &settings, true).unwrap();
        let mut edited: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        edited["statusLine"] = json!({"type":"command","command":"custom-status"});
        edited["env"]["OTEL_EXPORTER_OTLP_LOGS_PROTOCOL"] = json!("http/protobuf");
        std::fs::write(&settings, edited.to_string()).unwrap();
        suspend(&app, &settings).unwrap();
        let after: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(
            after,
            json!({"env":{"KEEP":"yes","OTEL_EXPORTER_OTLP_LOGS_PROTOCOL":"http/protobuf"},
            "hooks":{"Stop":["original"]},"statusLine":{"type":"command","command":"custom-status"}})
        );
        assert!(!enabled(&app, &settings).unwrap());
        assert!(load(&app).unwrap().suspended);
        assert!(load(&app).unwrap().token.is_empty());
    }

    #[test]
    fn interrupted_security_cleanup_blocks_serving_until_cleanup_succeeds() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        let settings = directory.path().join("settings.json");
        set_enabled(&app, &settings, true).unwrap();
        let managed = std::fs::read(&settings).unwrap();
        let old_token = load(&app).unwrap().token;
        std::fs::write(&settings, "invalid JSON").unwrap();
        assert!(suspend(&app, &settings).is_err());
        assert!(load(&app).unwrap().suspended);
        assert!(
            load(&app).unwrap().token.is_empty(),
            "legacy helpers must see no credential even when settings cleanup fails"
        );
        assert!(prepare_receiver(&app, &settings, 0).is_err());
        std::fs::write(&settings, managed).unwrap();
        assert!(
            set_enabled(&app, &settings, true).is_err(),
            "pending cleanup cannot be bypassed"
        );
        let (_listener, recovered) = prepare_receiver(&app, &settings, 0).unwrap();
        assert_ne!(recovered.token, old_token);
        assert!(!enabled(&app, &settings).unwrap());
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&settings).unwrap()).unwrap(),
            json!({})
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_feed_setup_uses_the_installed_app_without_powershell() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app space's");
        let settings = directory.path().join("settings.json");
        set_enabled(&app, &settings, true).unwrap();
        let value: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        let command = value["statusLine"]["command"].as_str().unwrap();
        assert!(!command.contains("powershell"));
        assert!(command.contains("--claude-statusline"));
        set_enabled(&app, &settings, false).unwrap();
        assert!(!enabled(&app, &settings).unwrap());
    }

    #[test]
    fn enable_and_disable_preserve_other_claude_settings_and_refuse_an_existing_feed() {
        let directory = tempfile::tempdir().unwrap();
        let app = directory.path().join("app");
        let settings = directory.path().join("settings.json");
        let original = json!({"theme":"dark","hooks":{"Stop":["original"]},"env":{"KEEP":"yes"}});
        std::fs::write(&settings, original.to_string()).unwrap();
        set_enabled(&app, &settings, true).unwrap();
        let mut enabled: Value =
            serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(enabled["hooks"], original["hooks"]);
        assert_eq!(enabled["env"]["KEEP"], "yes");
        assert_eq!(enabled["env"]["OTEL_LOGS_EXPORTER"], "otlp");
        enabled["theme"] = json!("light");
        std::fs::write(&settings, enabled.to_string()).unwrap();
        set_enabled(&app, &settings, false).unwrap();
        let restored: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(
            restored,
            json!({"theme":"light","hooks":{"Stop":["original"]},"env":{"KEEP":"yes"}})
        );
        let custom = json!({"statusLine":{"type":"command","command":"my-existing-status"}});
        std::fs::write(&settings, custom.to_string()).unwrap();
        assert!(set_enabled(&app, &settings, true).is_err());
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&settings).unwrap()).unwrap(),
            custom
        );
    }
}
