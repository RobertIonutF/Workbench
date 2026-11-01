use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};
use tokio_util::sync::CancellationToken;

pub struct Session {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    client: reqwest::Client,
    url: Option<String>,
    session_id: Option<String>,
    sequence: u64,
    protocol: String,
    auth_headers: reqwest::header::HeaderMap,
}
impl Drop for Session {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(pid) = self.child.as_ref().and_then(Child::id) {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(0x08000000)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}
impl Session {
    pub async fn connect(
        config: &Value,
        root: &Path,
        data: &Path,
        client: reqwest::Client,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        std::fs::create_dir_all(data)?;
        let kind = config["type"].as_str().unwrap_or("stdio");
        let mut this = Self {
            child: None,
            stdin: None,
            stdout: None,
            client,
            url: None,
            session_id: None,
            sequence: 0,
            protocol: "2025-11-25".into(),
            auth_headers: reqwest::header::HeaderMap::new(),
        };
        if let Some(headers) = config["_workbenchHeaders"].as_object() {
            for (name, value) in headers {
                let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())?;
                if header != reqwest::header::AUTHORIZATION {
                    bail!("Unsupported host credential header");
                }
                this.auth_headers.insert(
                    header,
                    reqwest::header::HeaderValue::from_str(
                        value.as_str().context("Invalid credential header")?,
                    )?,
                );
            }
        }
        if kind == "stdio" {
            let command = config["command"]
                .as_str()
                .context("MCP server needs a command")?;
            let command_path = if command.starts_with("./") {
                let path = std::fs::canonicalize(root.join(command))?;
                if !path.starts_with(std::fs::canonicalize(root)?) {
                    bail!("MCP executable escapes its package")
                };
                path
            } else {
                std::path::PathBuf::from(command)
            };
            let expand = |s: &str| {
                s.replace("${PLUGIN_ROOT}", &root.to_string_lossy())
                    .replace("${PLUGIN_DATA}", &data.to_string_lossy())
            };
            let cwd = expand(config["cwd"].as_str().unwrap_or("${PLUGIN_ROOT}"));
            let cwd = if Path::new(&cwd).is_absolute() {
                std::path::PathBuf::from(cwd)
            } else {
                root.join(cwd)
            };
            let cwd = std::fs::canonicalize(cwd)?;
            if !cwd.starts_with(std::fs::canonicalize(root)?)
                && !cwd.starts_with(std::fs::canonicalize(data)?)
            {
                bail!("MCP working directory escapes its roots")
            }
            let mut cmd = tokio::process::Command::new(command_path);
            cmd.current_dir(cwd).env_clear();
            for key in [
                "PATH",
                "SystemRoot",
                "WINDIR",
                "TEMP",
                "TMP",
                "PATHEXT",
                "COMSPEC",
            ] {
                if let Ok(v) = std::env::var(key) {
                    cmd.env(key, v);
                }
            }
            if let Some(args) = config["args"].as_array() {
                for arg in args {
                    cmd.arg(expand(
                        arg.as_str().context("MCP arguments must be strings")?,
                    ));
                }
            }
            if let Some(env) = config["env"].as_object() {
                for (k, v) in env {
                    if k == "PLUGIN_ROOT" || k == "PLUGIN_DATA" {
                        bail!("Reserved MCP environment variable")
                    };
                    cmd.env(
                        k,
                        expand(v.as_str().context("MCP environment must be strings")?),
                    );
                }
            }
            cmd.env("PLUGIN_ROOT", root)
                .env("PLUGIN_DATA", data)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            cmd.creation_flags(0x08000000);
            let mut child = cmd
                .spawn()
                .context("MCP process could not start; check its command, runtime and platform")?;
            this.stdin = child.stdin.take();
            this.stdout = child.stdout.take().map(BufReader::new);
            this.child = Some(child);
        } else if kind == "streamable-http" || kind == "http" {
            let url = url::Url::parse(config["url"].as_str().context("MCP server needs a URL")?)?;
            if url.scheme() != "https"
                && !(url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1")))
            {
                bail!("Remote MCP requires HTTPS")
            };
            this.url = Some(url.to_string());
            if config["headers"].as_object().is_some_and(|h| !h.is_empty()) {
                bail!(
                    "Literal package headers are not accepted. Configure remote authentication outside a distributable package."
                )
            }
        } else {
            bail!("Unsupported MCP transport: {kind}; use stdio or Streamable HTTP")
        }
        let reply=this.call("initialize",json!({"protocolVersion":this.protocol,"capabilities":{},"clientInfo":{"name":"Workbench","version":env!("CARGO_PKG_VERSION")}}),cancel).await?;
        if let Some(version) = reply["protocolVersion"].as_str() {
            if ![
                "2024-11-05",
                "2025-03-26",
                "2025-06-18",
                "2025-11-25",
                "2026-07-28",
            ]
            .contains(&version)
            {
                bail!("Unsupported negotiated MCP version {version}")
            };
            this.protocol = version.into();
        }
        this.notify("notifications/initialized", json!({}), cancel)
            .await?;
        Ok(this)
    }
    pub async fn call(
        &mut self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence;
        let message = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let value = tokio::select! {r=self.exchange(message,Some(id))=>r?,_=cancel.cancelled()=>bail!("MCP request canceled"),_=tokio::time::sleep(Duration::from_secs(60))=>bail!("MCP request timed out")};
        if !value["error"].is_null() {
            bail!(
                "MCP {}: {}",
                method,
                value["error"]["message"].as_str().unwrap_or("RPC error")
            )
        };
        Ok(value["result"].clone())
    }
    async fn notify(
        &mut self,
        method: &str,
        params: Value,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":params});
        tokio::select! {r=self.exchange(msg,None)=>{r?;},_=cancel.cancelled()=>bail!("Canceled")};
        Ok(())
    }
    async fn exchange(&mut self, message: Value, id: Option<u64>) -> Result<Value> {
        if let Some(url) = &self.url {
            let mut req = self
                .client
                .post(url)
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", &self.protocol)
                .headers(self.auth_headers.clone())
                .json(&message);
            if let Some(session) = &self.session_id {
                req = req.header("Mcp-Session-Id", session);
            };
            let response = req.send().await?;
            if let Some(session) = response.headers().get("mcp-session-id") {
                self.session_id = Some(session.to_str()?.into());
            }
            if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                bail!(
                    "Remote MCP requires an authenticated connection; anonymous connection was refused"
                )
            }
            if !response.status().is_success() {
                bail!("MCP HTTP returned {}", response.status())
            }
            if id.is_none() {
                return Ok(Value::Null);
            }
            let event_stream = response
                .headers()
                .get("content-type")
                .is_some_and(|h| h.to_str().unwrap_or("").contains("text/event-stream"));
            let mut body_stream = response.bytes_stream();
            let mut bytes = Vec::new();
            if event_stream {
                let mut parser = crate::providers::SseParser::default();
                while let Some(chunk) = body_stream.next().await {
                    let chunk = chunk?;
                    for data in parser.push(&chunk)? {
                        let value: Value = serde_json::from_str(&data)?;
                        if value["id"].as_u64() == id {
                            return Ok(value);
                        }
                    }
                }
                bail!("MCP stream ended without matching response")
            } else {
                while let Some(chunk) = body_stream.next().await {
                    bytes.extend_from_slice(&chunk?);
                    if bytes.len() > 4 * 1024 * 1024 {
                        bail!("MCP response exceeds size limit");
                    }
                }
                let value: Value = serde_json::from_slice(&bytes)?;
                if value["id"].as_u64() != id {
                    bail!("MCP response ID mismatch")
                };
                return Ok(value);
            }
        }
        let stdin = self.stdin.as_mut().context("MCP stdin unavailable")?;
        stdin.write_all(format!("{message}\n").as_bytes()).await?;
        stdin.flush().await?;
        if id.is_none() {
            return Ok(Value::Null);
        }
        let reader = self.stdout.as_mut().context("MCP stdout unavailable")?;
        loop {
            let mut bytes = Vec::new();
            let size = (&mut *reader)
                .take(4 * 1024 * 1024 + 1)
                .read_until(b'\n', &mut bytes)
                .await?;
            if size == 0 {
                bail!("MCP server exited before responding")
            };
            if size > 4 * 1024 * 1024 {
                bail!("MCP response exceeds limit")
            };
            let value: Value = serde_json::from_slice(&bytes)
                .context("MCP server wrote non-JSON to stdout; logs belong on stderr")?;
            if value["id"].as_u64() == id {
                return Ok(value);
            }
            if value["method"].is_string() && !value["id"].is_null() {
                let reply = json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"Client-initiated sampling/elicitation is not enabled in this session"}});
                let stdin = self.stdin.as_mut().unwrap();
                stdin.write_all(format!("{reply}\n").as_bytes()).await?;
                stdin.flush().await?;
            }
        }
    }
    pub async fn close(&mut self) {
        if let Some(child) = &mut self.child {
            if let Some(pid) = child.id() {
                crate::runtime::kill_tree(pid).await;
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
    pub async fn tools(&mut self, cancel: &CancellationToken) -> Result<Vec<Value>> {
        let mut all = vec![];
        let mut cursor = Value::Null;
        for _ in 0..20 {
            let params = if cursor.is_null() {
                json!({})
            } else {
                json!({"cursor":cursor})
            };
            let reply = self.call("tools/list", params, cancel).await?;
            if let Some(items) = reply["tools"].as_array() {
                all.extend(items.iter().cloned());
            }
            cursor = reply["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
        }
        Ok(all)
    }
}
