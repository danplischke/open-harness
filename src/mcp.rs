//! A minimal MCP client — the runtime behind the MCP→CLI bridge (#19).
//!
//! Some orgs block MCP by disabling the harness's MCP *client/config*. This lets
//! an existing MCP *server's* tools still be called through the allowlisted
//! shell: `oh mcp call` speaks MCP to the server and returns the tool result on
//! stdout, so the agent never needs the harness's MCP client.
//!
//! **Honest scope:** the bridge still *runs* the MCP server, so it only helps
//! when the block is on the harness's MCP client (auditability, no dynamic tool
//! surface), not when the risk is the server process or its network egress.
//!
//! This is a deliberately small JSON-RPC 2.0 client over the **stdio** transport
//! (newline-delimited messages): `initialize` → `notifications/initialized` →
//! `tools/list` / `tools/call`. A streamable-HTTP transport is a follow-up
//! (needs an HTTP/TLS client the crate doesn't yet depend on).

use crate::manifest::{LoadedCapability, RunSpec};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How to launch a stdio MCP server.
#[derive(Debug, Clone, Default)]
pub struct ServerSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

/// A tool advertised by `tools/list`.
#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Build a [`ServerSpec`] from a `provider: mcp` capability's `tool.server`.
pub fn server_from_capability(cap: &LoadedCapability) -> Result<ServerSpec, String> {
    let server = cap
        .manifest
        .config
        .get("tool")
        .and_then(|t| t.get("server"))
        .ok_or("capability has no `tool.server` (not a provider:mcp tool)")?;
    let command = server
        .get("command")
        .and_then(|c| c.as_str())
        .ok_or("tool.server.command is required for a stdio MCP bridge")?
        .to_string();
    let args = server
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let env = server
        .get("env")
        .and_then(|e| e.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Ok(ServerSpec {
        command,
        args,
        env,
        cwd: Some(cap.dir.clone()),
    })
}

/// A live stdio MCP session (spawned server + initialized handshake).
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: i64,
    timeout: Duration,
}

impl Client {
    /// Spawn the server and complete the `initialize` handshake.
    pub fn start(spec: &ServerSpec) -> Result<Client, String> {
        // Reuse the runtime's cross-platform interpreter resolution.
        let run = RunSpec {
            command: spec.command.clone(),
            args: spec.args.clone(),
            interpreter: None,
        };
        let (program, argv) = crate::runtime::resolve_program(&run);

        let mut cmd = Command::new(&program);
        cmd.args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        if let Some(dir) = &spec.cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn MCP server '{program}': {e}"))?;

        let stdin = child.stdin.take().ok_or("no stdin on MCP server")?;
        let stdout = child.stdout.take().ok_or("no stdout on MCP server")?;
        // Drain stderr so the server can't deadlock on a full pipe.
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut sink = String::new();
                let _ = BufReader::new(err).read_to_string(&mut sink);
            });
        }
        // Read stdout lines on a thread; the channel closes when the server exits.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut client = Client {
            child,
            stdin,
            rx,
            next_id: 1,
            timeout: Duration::from_secs(10),
        };
        client.initialize()?;
        Ok(client)
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        let line = serde_json::to_string(msg).map_err(|e| e.to_string())?;
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("write to MCP server: {e}"))
    }

    /// Send a request and wait for the response with the matching id, skipping
    /// interleaved notifications. Times out rather than hanging on a stuck server.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| format!("timeout waiting for '{method}' response"))?;
            match self.rx.recv_timeout(remaining) {
                Ok(line) => {
                    let Ok(v) = serde_json::from_str::<Value>(&line) else {
                        continue; // not JSON (a log line) — ignore
                    };
                    if v.get("id").and_then(|x| x.as_i64()) != Some(id) {
                        continue; // a notification or another id
                    }
                    if let Some(e) = v.get("error") {
                        let msg = e
                            .get("message")
                            .and_then(|m| m.as_str())
                            .map(String::from)
                            .unwrap_or_else(|| e.to_string());
                        return Err(format!("{method}: {msg}"));
                    }
                    return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("timeout waiting for '{method}' response"))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("MCP server closed the connection".to_string())
                }
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn initialize(&mut self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "open-harness", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        self.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    /// `tools/list`.
    pub fn list_tools(&mut self) -> Result<Vec<Tool>, String> {
        let result = self.request("tools/list", json!({}))?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .iter()
            .map(|t| Tool {
                name: t
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                input_schema: t.get("inputSchema").cloned().unwrap_or(Value::Null),
            })
            .collect())
    }

    /// `tools/call` → the raw result object (typically `{content: [...]}`).
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Closing stdin (EOF) lets a well-behaved server exit; kill as a backstop.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Extract the plain-text content from a `tools/call` result, if any (the common
/// `{content:[{type:"text", text:"…"}]}` shape); else the compact JSON.
pub fn result_text(result: &Value) -> String {
    if let Some(items) = result.get("content").and_then(|c| c.as_array()) {
        let texts: Vec<&str> = items
            .iter()
            .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
            .collect();
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    result.to_string()
}
