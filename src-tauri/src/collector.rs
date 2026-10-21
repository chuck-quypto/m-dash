use serde_json::{json, Value};
use std::{
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

pub async fn read_response<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    id: u64,
) -> Result<Value, String> {
    loop {
        let mut line = Vec::new();
        let size = (&mut *reader)
            .take(4 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| "Could not read the CLI response.")?;
        if size == 0 {
            return Err("The CLI closed before returning account data.".into());
        }
        if size > 4 * 1024 * 1024 {
            return Err("The CLI response exceeded the supported size.".into());
        }
        let response: Value =
            serde_json::from_slice(&line).map_err(|_| "The CLI returned invalid JSON.")?;
        if response["id"].as_u64() != Some(id) {
            continue;
        }
        if response.get("error").is_some() {
            return Err(format!(
                "The CLI could not read this resource (code {}). Check its sign-in and connection.",
                response["error"]["code"].as_i64().unwrap_or(0)
            ));
        }
        return response
            .get("result")
            .cloned()
            .ok_or_else(|| "The CLI returned no result for the account request.".into());
    }
}

struct Rpc {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Rpc {
    fn start(executable: PathBuf, arguments: &[&str]) -> Result<Self, String> {
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command
            .spawn()
            .map_err(|_| "Could not start the account CLI. Verify that it runs normally.")?;
        let input = child.stdin.take().ok_or("Could not open CLI input.")?;
        let output = BufReader::new(child.stdout.take().ok_or("Could not open CLI output.")?);
        Ok(Self {
            child,
            input,
            output,
            next_id: 0,
        })
    }
    async fn send(&mut self, message: Value) -> Result<(), String> {
        self.input
            .write_all(format!("{message}\n").as_bytes())
            .await
            .map_err(|_| "Could not send a read request to the CLI.".into())
    }
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params}))
            .await?;
        timeout(
            Duration::from_secs(15),
            read_response(&mut self.output, self.next_id),
        )
        .await
        .map_err(|_| "The CLI did not respond within 15 seconds. Check the connection and retry.")?
    }
}

pub fn cli_directories() -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        directories.push(PathBuf::from(appdata).join("npm"));
    }
    #[cfg(not(windows))]
    directories.extend([
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ]);
    #[cfg(target_os = "linux")]
    if let Some(home) = std::env::var_os("HOME") {
        directories.push(PathBuf::from(home).join(".local/bin"));
    }
    directories
}

fn find_codex() -> Result<PathBuf, String> {
    let directories = cli_directories();
    for directory in directories {
        let executable = directory.join(if cfg!(windows) { "codex.exe" } else { "codex" });
        if executable.is_file() {
            return Ok(executable);
        }
        #[cfg(windows)]
        {
            let (package, triple) = if cfg!(target_arch = "aarch64") {
                ("codex-win32-arm64", "aarch64-pc-windows-msvc")
            } else {
                ("codex-win32-x64", "x86_64-pc-windows-msvc")
            };
            let vendor = directory.join(format!(
                "node_modules/@openai/codex/node_modules/@openai/{package}/vendor/{triple}"
            ));
            for relative in ["bin/codex.exe", "codex/codex.exe"] {
                let candidate = vendor.join(relative);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    Err("Codex CLI was not found. Install Codex, sign in with your subscription, and restart m-dash.".into())
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub async fn collect() -> Result<Value, String> {
    timeout(Duration::from_secs(45), async {
        let mut rpc = Rpc::start(find_codex()?, &["app-server"])?;
        rpc.call("initialize", json!({"clientInfo":{"name":"m_dash","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":false,"explicitGatewayOauth":true}})).await?;
        rpc.send(json!({"method":"initialized"})).await?;
        let identity = rpc.call("account/read", json!({"refreshToken":false})).await?;
        if identity["account"]["type"].as_str() != Some("chatgpt") {
            return Err("Sign in to the Codex CLI with your ChatGPT subscription, then refresh m-dash.".into());
        }
        let limits = rpc.call("account/rateLimits/read", json!({})).await?;
        let usage = rpc.call("account/usage/read", json!({})).await;
        let result = crate::codex::finish_snapshot(limits, usage, identity["account"]["planType"].as_str(), now());
        let _ = rpc.child.kill().await;
        result
    }).await.map_err(|_| "The account refresh timed out. Your last successful values have been retained.".to_string())?
}

pub async fn collect_grok() -> Result<Value, String> {
    timeout(Duration::from_secs(45), async {
        let mut directories = cli_directories();
        if let Some(home) = std::env::var_os(if cfg!(windows) {"USERPROFILE"} else {"HOME"}) {
            directories.push(PathBuf::from(home).join(".grok/bin"));
        }
        let executable = directories.into_iter().map(|path| path.join(if cfg!(windows) {"grok.exe"} else {"grok"})).find(|path| path.is_file())
            .ok_or("Grok CLI was not found. Install Grok, run grok login, and restart m-dash.")?;
        let mut rpc = Rpc::start(executable, &["agent","--no-leader","stdio"])?;
        rpc.call("initialize",json!({"protocolVersion":"1","clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}})).await?;
        let identity = rpc.call("_x.ai/auth/info",json!({})).await?;
        let billing = rpc.call("_x.ai/billing",json!({})).await?;
        let result = crate::grok::normalize(identity,billing,now());
        let _ = rpc.child.kill().await;
        result
    }).await.map_err(|_| "Grok billing read timed out. Check grok login and your connection.".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::BufReader;

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_cli_discovery_includes_the_native_user_install_directory() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert!(cli_directories().contains(&home.join(".local/bin")));
    }

    #[test]
    fn rpc_ignores_notifications_and_returns_only_the_requested_response() {
        tauri::async_runtime::block_on(async {
            let bytes = b"{\"method\":\"account/rateLimits/updated\",\"params\":{}}\n{\"id\":9,\"result\":{\"ok\":true}}\n";
            let mut reader = BufReader::new(&bytes[..]);
            assert_eq!(
                read_response(&mut reader, 9).await.unwrap(),
                json!({"ok":true})
            );
        });
    }
}
