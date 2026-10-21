use crate::claude_ping::MODEL;
use serde_json::Value;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
};

struct RunDirectory(PathBuf);
impl RunDirectory {
    fn create() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("m-dash-ping-{}", uuid::Uuid::new_v4()));
        let builder = &mut std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|_| "Could not create an isolated ping directory.")?;
        Ok(Self(path))
    }
}
impl Drop for RunDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Prepared {
    executable: PathBuf,
    directory: RunDirectory,
}
pub struct Run {
    child: Child,
    _directory: RunDirectory,
}

impl Prepared {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.env_clear();
        // Keep native subscription/Keychain discovery, never inherited API keys or telemetry.
        for name in [
            "HOME",
            "USER",
            "LOGNAME",
            "PATH",
            "LANG",
            "TMPDIR",
            "TMP",
            "TEMP",
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "SYSTEMROOT",
            "WINDIR",
            "CLAUDE_CONFIG_DIR",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .current_dir(&self.directory.0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        command
    }

    pub async fn check(executable: PathBuf) -> Result<Self, String> {
        let prepared = Self {
            executable,
            directory: RunDirectory::create()?,
        };
        let mut version = prepared
            .command()
            .arg("--version")
            .spawn()
            .map_err(|_| "Could not start Claude CLI.")?;
        let bytes = bounded_output(&mut version, 15).await?;
        let version: Vec<u32> = std::str::from_utf8(&bytes)
            .unwrap_or("")
            .split_whitespace()
            .next()
            .unwrap_or("")
            .split('.')
            .filter_map(|part| part.parse().ok())
            .collect();
        if version.len() != 3 || version.as_slice() < [2, 1, 293].as_slice() {
            return Err(
                "Automatic pings require Claude Code 2.1.293 or later for Haiku 5.5 at low effort."
                    .into(),
            );
        }
        let mut auth = prepared
            .command()
            .args([
                "--safe-mode",
                "--setting-sources",
                "",
                "auth",
                "status",
                "--json",
            ])
            .spawn()
            .map_err(|_| "Could not check Claude subscription authentication.")?;
        let bytes = bounded_output(&mut auth, 15).await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Claude subscription authentication could not be verified.")?;
        if value["loggedIn"] != true
            || value["authMethod"] != "claude.ai"
            || value["apiProvider"] != "firstParty"
            || !matches!(value["subscriptionType"].as_str(), Some("pro" | "max"))
        {
            return Err("Automatic pings require your own Claude Pro or Max subscription sign-in. No API-key fallback is used.".into());
        }
        Ok(prepared)
    }

    /// Called only after the attempt marker is durable, under the settings lock.
    pub fn start(self) -> Result<Run, String> {
        let mut command = self.command();
        command.args(["--safe-mode", "--setting-sources", "", "--settings",
            r#"{"disableAllHooks":true,"availableModels":["claude-haiku-5-5"],"fallbackModel":[],"switchModelsOnFlag":false}"#,
            "--strict-mcp-config", "--mcp-config", r#"{"mcpServers":{}}"#, "--disallowedTools", "mcp__*",
            "--no-chrome", "--disable-slash-commands", "--tools", "", "--permission-mode", "default",
            "--permission-prompts", "none", "--no-session-persistence", "--max-turns", "1",
            "--model", MODEL, "--effort", "low", "--system-prompt", "Reply with pong.",
            "--print", "--output-format", "json", "ping"]);
        let child = command
            .spawn()
            .map_err(|_| "Claude ping could not start. No automatic retry will be made.")?;
        Ok(Run {
            child,
            _directory: self.directory,
        })
    }
}

async fn bounded_output(child: &mut Child, seconds: u64) -> Result<Vec<u8>, String> {
    let result = tokio::time::timeout(Duration::from_secs(seconds), async {
        let output = child.stdout.take().ok_or("Claude returned no output stream.")?;
        let mut bytes = Vec::new();
        output.take(65537).read_to_end(&mut bytes).await.map_err(|_| "Could not read Claude's response.")?;
        if bytes.len() > 65536 { return Err("Claude returned an oversized response. No automatic retry will be made.".into()); }
        let status = child.wait().await.map_err(|_| "Could not confirm Claude's completion.")?;
        if !status.success() { return Err("Claude could not complete the request. Check its sign-in, model availability and limits. No automatic retry will be made.".into()); }
        Ok(bytes)
    }).await.unwrap_or_else(|_| Err("Claude timed out; the outcome is unknown. No automatic retry will be made.".into()));
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

impl Run {
    pub async fn finish(mut self) -> Result<Option<u64>, String> {
        let bytes = bounded_output(&mut self.child, 60).await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Claude's result was unrecognized. No automatic retry will be made.")?;
        let models = value["modelUsage"].as_object();
        if value["type"] != "result"
            || value["subtype"] != "success"
            || value["is_error"] != false
            || !models.is_some_and(|models| {
                models.len() == 1
                    && models
                        .keys()
                        .all(|m| m == MODEL || m.starts_with(&format!("{MODEL}-20")))
            })
        {
            return Err(
                "Claude did not confirm a successful Haiku ping. No automatic retry will be made."
                    .into(),
            );
        }
        let usage = models.and_then(|m| m.values().next()).unwrap();
        // Report accounting metadata only; generated content and raw errors are discarded.
        Ok([
            "inputTokens",
            "outputTokens",
            "cacheReadInputTokens",
            "cacheCreationInputTokens",
        ]
        .iter()
        .try_fold(0_u64, |total, key| {
            usage[key].as_u64().and_then(|n| total.checked_add(n))
        }))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn cli(directory: &std::path::Path, auth: &str, result: &str) -> std::path::PathBuf {
        let path = directory.join("claude-fixture");
        let script = format!(
            r##"#!/usr/bin/python3
import sys, os, json
args=sys.argv[1:]
if '--version' in args:
 print('2.1.300 (Claude Code)')
elif 'auth' in args:
 print({auth:?})
else:
 assert args[-1]=='ping'
 assert args[args.index('--model')+1]=='claude-haiku-5-5'
 assert args[args.index('--effort')+1]=='low'
 assert args[args.index('--tools')+1]==''
 assert args[args.index('--setting-sources')+1]==''
 assert '--safe-mode' in args and '--no-session-persistence' in args
 assert '--strict-mcp-config' in args and '--no-chrome' in args
 assert args[args.index('--max-turns')+1]=='1'
 settings=json.loads(args[args.index('--settings')+1])
 assert settings['disableAllHooks'] and settings['fallbackModel']==[]
 assert settings['availableModels']==['claude-haiku-5-5']
 assert not any(k.startswith(('ANTHROPIC_', 'OTEL_', 'CLAUDE_CODE_OAUTH_TOKEN')) for k in os.environ)
 assert not os.listdir('.')
 print({result:?})
"##
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    #[ignore = "reads this host's signed-in subscription metadata; no inference"]
    fn native_ping_preflight_checks_subscription_and_fresh_usage_without_inference() {
        tauri::async_runtime::block_on(async {
            let _prepared = Prepared::check(crate::claude::executable().unwrap())
                .await
                .unwrap();
            let snapshot = crate::claude::collect().await.unwrap();
            println!(
                "Claude metadata: plan={}, fiveHourState={}, pingFresh={}",
                snapshot["plan"], snapshot["fiveHourState"], snapshot["pingFresh"]
            );
            assert!(matches!(snapshot["plan"].as_str(), Some("pro" | "max")));
            assert!(snapshot["fiveHourState"].is_string());
        });
    }

    #[test]
    fn isolated_cli_run_sends_only_the_consented_prompt_and_returns_usage_not_content() {
        tauri::async_runtime::block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let path = cli(
                directory.path(),
                r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"pro"}"#,
                r#"{"type":"result","subtype":"success","is_error":false,"result":"private generated content","modelUsage":{"claude-haiku-5-5":{"inputTokens":10,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4}}}"#,
            );
            let prepared = Prepared::check(path).await.unwrap();
            let run = prepared.start().unwrap();
            assert_eq!(run.finish().await.unwrap(), Some(19));
        });
    }

    #[test]
    fn api_auth_and_unexpected_models_fail_without_exposing_provider_output() {
        tauri::async_runtime::block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let path = cli(
                directory.path(),
                r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty","subscriptionType":"pro","secret":"do-not-display"}"#,
                "{}",
            );
            assert!(Prepared::check(path)
                .await
                .err()
                .unwrap()
                .contains("subscription"));
            let path = cli(
                directory.path(),
                r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"pro"}"#,
                r#"{"type":"result","subtype":"success","is_error":false,"result":"do-not-display","modelUsage":{"claude-sonnet-5-5":{}}}"#,
            );
            let error = Prepared::check(path)
                .await
                .unwrap()
                .start()
                .unwrap()
                .finish()
                .await
                .unwrap_err();
            assert!(!error.contains("do-not-display"));
        });
    }
}
