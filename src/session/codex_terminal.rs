//! One native app server shared by a terminal and its autonomous senders.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{os::unix::net::UnixStream, path::Path, process::Stdio, time::Duration};
use tokio_tungstenite::tungstenite::{client, Message, WebSocket};

pub const ENDPOINT_KEY: &str = "AOE_CODEX_QUEUE_ENDPOINT";

pub async fn run(mut args: impl Iterator<Item = String>) -> Result<()> {
    let session = args.next().context("Missing tmux session")?;
    let executable = args.next().context("Missing Codex executable")?;
    let args: Vec<String> = args.collect();
    let instance = std::env::var("AOE_INSTANCE_ID").context("Missing AoE identity")?;
    let base = crate::hooks::ensure_instance_dir_path(&instance)?;
    let directory = tempfile::Builder::new().prefix("codex-").tempdir_in(base)?;
    let socket = directory.path().join("server.sock");
    let endpoint = format!("unix://{}", socket.display());
    let log = std::fs::File::create(directory.path().join("server.log"))?;
    let mut command = tokio::process::Command::new(&executable);
    command.args(["app-server", "--listen", &endpoint]);
    command.args(server_config_args(&args)?);
    crate::process::configure_process_group(command.as_std_mut());
    let mut server = command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .context("Could not start Codex app server")?;
    let group = ServerGroup(server.id().context("Codex server has no process ID")?);
    let ready = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(status) = server.try_wait()? {
                bail!("Codex app server exited during startup: {status}");
            }
            if socket.exists() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("Codex app server startup timed out")?;
    ready?;
    crate::tmux::env::set_hidden_env(&session, ENDPOINT_KEY, &endpoint)?;
    let mut terminal = tokio::process::Command::new(executable)
        .args(["--remote", &endpoint])
        .args(args)
        .kill_on_drop(true)
        .spawn()
        .context("Could not start Codex terminal")?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let result = tokio::select! {
        status = terminal.wait() => match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(anyhow::anyhow!("Codex terminal exited: {status}")),
            Err(error) => Err(error.into()),
        },
        status = server.wait() => Err(anyhow::anyhow!("Codex app server exited: {status:?}")),
        _ = terminate.recv() => Ok(()),
        _ = hangup.recv() => Ok(()),
        _ = tokio::signal::ctrl_c() => Ok(()),
    };
    let _ = terminal.kill().await;
    crate::process::worker::terminate_process_group(group.0);
    let _ = tokio::time::timeout(Duration::from_secs(2), server.wait()).await;
    drop(group);
    let _ = server.kill().await;
    result
}

struct ServerGroup(u32);
impl Drop for ServerGroup {
    fn drop(&mut self) {
        crate::process::worker::kill_process_group(self.0);
    }
}

fn server_config_args(args: &[String]) -> Result<Vec<String>> {
    let mut config = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-c" | "--config" | "--enable" | "--disable" => {
                config.push(arg.clone());
                config.push(iter.next().context("Missing Codex configuration value")?.clone());
            }
            "-p" | "--profile" => bail!("Codex profiles are not supported by managed terminal delivery; use configuration overrides"),
            _ if arg.starts_with("--profile=") => bail!("Codex profiles are not supported by managed terminal delivery; use configuration overrides"),
            _ if arg.starts_with("--config=") || arg.starts_with("--enable=") || arg.starts_with("--disable=") => config.push(arg.clone()),
            _ => {}
        }
    }
    Ok(config)
}

struct Connection(WebSocket<UnixStream>);
impl Connection {
    fn connect(endpoint: &str) -> Result<Self> {
        let path = endpoint
            .strip_prefix("unix://")
            .context("Expected local Codex socket")?;
        let socket = UnixStream::connect(Path::new(path))
            .context("Codex terminal connection is unavailable; restart this agent")?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let (ws, _) = client("ws://localhost", socket)
            .map_err(|error| anyhow::anyhow!("Codex handshake failed: {error}"))?;
        let mut connection = Self(ws);
        connection.rpc(1, "initialize", json!({"clientInfo":{"name":"aoe_terminal_queue","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        connection.0.send(Message::Text(
            json!({"method":"initialized"}).to_string().into(),
        ))?;
        Ok(connection)
    }
    fn rpc(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        self.0.send(Message::Text(
            json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Message::Text(text) = self.0.read()? {
                let response: Value = serde_json::from_str(&text)?;
                if response["id"] == id {
                    if let Some(error) = response.get("error") {
                        bail!("Codex {method} failed: {error}");
                    }
                    return response
                        .get("result")
                        .cloned()
                        .context("Missing Codex response");
                }
            }
        }
        bail!("Codex {method} timed out")
    }
}

/// Resolve only threads loaded in this pane's dedicated server, never by cwd.
pub fn active_thread(endpoint: &str) -> Result<Option<String>> {
    let mut connection = Connection::connect(endpoint)?;
    let loaded = connection.rpc(2, "thread/loaded/list", json!({}))?;
    anyhow::ensure!(
        loaded["nextCursor"].is_null(),
        "Codex has too many loaded threads to identify the terminal safely"
    );
    let ids = loaded["data"]
        .as_array()
        .context("Invalid Codex loaded threads response")?;
    let mut roots = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let read = connection.rpc(
            3 + index as u64,
            "thread/read",
            json!({"threadId":id,"includeTurns":false}),
        )?;
        let thread = &read["thread"];
        if thread["parentThreadId"].is_null() && thread["threadSource"] != "subAgent" {
            roots.push(
                thread["id"]
                    .as_str()
                    .context("Missing Codex thread identity")?
                    .to_string(),
            );
        }
    }
    select_root(roots)
}

fn select_root(roots: Vec<String>) -> Result<Option<String>> {
    anyhow::ensure!(
        roots.len() <= 1,
        "Multiple Codex conversations are loaded in this terminal; no message was sent"
    );
    roots
        .into_iter()
        .next()
        .map(|id| {
            uuid::Uuid::parse_str(&id)
                .map(|id| id.to_string())
                .context("Invalid Codex thread identity")
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_terminal_discovers_fresh_thread_without_hooks() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let root = uuid::Uuid::new_v4().to_string();
        let child = uuid::Uuid::new_v4().to_string();
        let expected = root.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut ws = tokio_tungstenite::tungstenite::accept(stream).unwrap();
            for method in [
                "initialize",
                "initialized",
                "thread/loaded/list",
                "thread/read",
                "thread/read",
            ] {
                let request: Value =
                    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(request["method"], method);
                let result = match method {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "thread/loaded/list" => json!({"data":[root,child],"nextCursor":null}),
                    _ if request["params"]["threadId"] == root => {
                        json!({"thread":{"id":root,"parentThreadId":null,"threadSource":"user","turns":[]}})
                    }
                    _ => {
                        json!({"thread":{"id":child,"parentThreadId":root,"threadSource":"subAgent"}})
                    }
                };
                ws.send(Message::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            }
        });
        assert_eq!(
            active_thread(&format!("unix://{}", path.display())).unwrap(),
            Some(expected)
        );
        server.join().unwrap();
    }

    #[test]
    fn native_terminal_identity_requires_one_valid_root() {
        assert_eq!(select_root(vec![]).unwrap(), None);
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(select_root(vec![id.clone()]).unwrap(), Some(id.clone()));
        assert!(select_root(vec![id.clone(), id]).is_err());
        assert!(select_root(vec!["bad".into()]).is_err());
    }
    #[test]
    fn native_terminal_server_inherits_configuration() {
        let args = [
            "--model",
            "gpt-6-astra",
            "-c",
            "service_tier=\"fast\"",
            "--disable",
            "hooks",
            "resume",
            "id",
        ]
        .map(String::from);
        assert_eq!(
            server_config_args(&args).unwrap(),
            ["-c", "service_tier=\"fast\"", "--disable", "hooks"]
        );
        assert!(server_config_args(&["-c".into()]).is_err());
        assert!(server_config_args(&["--profile".into(), "custom".into()]).is_err());
    }
}
