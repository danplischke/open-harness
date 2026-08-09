//! A minimal MCP client — the runtime behind the MCP→CLI bridge (#19, #20).
//!
//! Some orgs block MCP by disabling the harness's MCP *client/config*. This lets
//! an existing MCP *server's* tools still be called through the allowlisted
//! shell: `oh mcp call` speaks MCP to the server and returns the tool result on
//! stdout, so the agent never needs the harness's MCP client.
//!
//! **Honest scope:** the bridge still *reaches* the MCP server, so it only helps
//! when the block is on the harness's MCP client (auditability, no dynamic tool
//! surface), not when the risk is the server process or its network egress.
//!
//! Two transports, both deliberately small JSON-RPC 2.0 clients doing
//! `initialize` → `notifications/initialized` → `tools/list` / `tools/call`:
//!   - **stdio** (#19): newline-delimited messages over a spawned server's pipes.
//!   - **streamable-HTTP** (#20): one `POST` per request to an HTTP endpoint,
//!     parsing either an `application/json` reply or a `text/event-stream` (SSE)
//!     one, threading the `Mcp-Session-Id` header. The transport itself lives in
//!     [`crate::http`] (shared with remote capability sources), hand-rolled over
//!     `TcpStream` to keep the default build dependency-free. `http://` needs no
//!     dependency; `https://` uses the optional **`http-tls`** feature (rustls,
//!     #25) — without it, an `https` endpoint is **honestly refused** with a
//!     pointer to native MCP config (`oh emit`/`oh sync`) or a local http proxy.
//!     With TLS on, `OPEN_HARNESS_CA_FILE` adds a private/corporate CA to the
//!     Mozilla roots.

use crate::http;
use crate::manifest::{LoadedCapability, RunSpec};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The MCP protocol version this client advertises on `initialize`.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// How to launch a stdio MCP server.
#[derive(Debug, Clone, Default)]
pub struct ServerSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

/// How to reach a streamable-HTTP MCP server.
#[derive(Debug, Clone, Default)]
pub struct HttpServerSpec {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// A resolved MCP server: a stdio process or a streamable-HTTP endpoint.
pub enum McpServer {
    Stdio(ServerSpec),
    Http(HttpServerSpec),
}

/// A tool advertised by `tools/list`.
#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Build an [`McpServer`] from a `provider: mcp` capability's `tool.server`.
/// `transport: http` (aka `streamable-http`) yields an HTTP endpoint; anything
/// else (the default) yields a stdio process.
pub fn server_from_capability(cap: &LoadedCapability) -> Result<McpServer, String> {
    let server = cap
        .manifest
        .config
        .get("tool")
        .and_then(|t| t.get("server"))
        .ok_or("capability has no `tool.server` (not a provider:mcp tool)")?;

    let transport = server
        .get("transport")
        .and_then(|t| t.as_str())
        .unwrap_or("stdio");

    match transport {
        "http" | "streamable-http" | "streamablehttp" => {
            let url = server
                .get("url")
                .and_then(|u| u.as_str())
                .ok_or("tool.server.url is required for an http MCP bridge")?
                .to_string();
            let mut headers: Vec<(String, String)> = server
                .get("headers")
                .and_then(|h| h.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            // A bearer token is read from the environment at call time, never
            // committed — matching the native-config `bearer_token_env` field.
            if let Some(var) = server.get("bearer_token_env").and_then(|b| b.as_str()) {
                if let Ok(token) = std::env::var(var) {
                    headers.push(("Authorization".to_string(), format!("Bearer {token}")));
                }
            }
            Ok(McpServer::Http(HttpServerSpec { url, headers }))
        }
        "sse" => Err(
            "the bridge does not support the legacy SSE transport; use `transport: http` \
             (streamable-HTTP) or install native MCP config with `oh emit`/`oh sync`"
                .to_string(),
        ),
        _ => {
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
            Ok(McpServer::Stdio(ServerSpec {
                command,
                args,
                env,
                cwd: Some(cap.dir.clone()),
            }))
        }
    }
}

/// A live MCP session over one of the transports, past the `initialize` handshake.
pub struct Client {
    transport: Transport,
    next_id: i64,
    timeout: Duration,
}

enum Transport {
    Stdio(StdioState),
    Http(HttpState),
}

impl Client {
    /// Connect to whichever transport `server` describes and handshake.
    pub fn connect(server: &McpServer) -> Result<Client, String> {
        match server {
            McpServer::Stdio(s) => Client::start(s),
            McpServer::Http(h) => Client::connect_http(h),
        }
    }

    /// Spawn a stdio server and complete the `initialize` handshake.
    pub fn start(spec: &ServerSpec) -> Result<Client, String> {
        let stdio = StdioState::spawn(spec)?;
        let mut client = Client {
            transport: Transport::Stdio(stdio),
            next_id: 1,
            timeout: Duration::from_secs(10),
        };
        client.initialize()?;
        Ok(client)
    }

    /// Connect to a streamable-HTTP endpoint and complete the handshake.
    pub fn connect_http(spec: &HttpServerSpec) -> Result<Client, String> {
        let mut client = Client {
            transport: Transport::Http(HttpState {
                url: spec.url.clone(),
                extra_headers: spec.headers.clone(),
                session_id: None,
                negotiated_version: None,
            }),
            next_id: 1,
            timeout: Duration::from_secs(10),
        };
        client.initialize()?;
        Ok(client)
    }

    /// Send a request and return its `result` (or the server's error).
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let timeout = self.timeout;
        match &mut self.transport {
            Transport::Stdio(s) => s.request(id, method, params, timeout),
            Transport::Http(h) => h.request(id, method, params, timeout),
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let timeout = self.timeout;
        match &mut self.transport {
            Transport::Stdio(s) => s.notify(method, params),
            Transport::Http(h) => h.notify(method, params, timeout),
        }
    }

    fn initialize(&mut self) -> Result<(), String> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "open-harness", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        // On HTTP, subsequent requests must carry the negotiated protocol version.
        if let Transport::Http(h) = &mut self.transport {
            h.negotiated_version = result
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| Some(PROTOCOL_VERSION.to_string()));
        }
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

// ---- stdio transport (#19) -------------------------------------------------

struct StdioState {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
}

impl StdioState {
    fn spawn(spec: &ServerSpec) -> Result<StdioState, String> {
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

        Ok(StdioState { child, stdin, rx })
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
    fn request(
        &mut self,
        id: i64,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;

        let deadline = Instant::now() + timeout;
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
                    return interpret_rpc(method, &v);
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
}

impl Drop for StdioState {
    fn drop(&mut self) {
        // Closing stdin (EOF) lets a well-behaved server exit; kill as a backstop.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- streamable-HTTP transport (#20) ---------------------------------------

struct HttpState {
    url: String,
    extra_headers: Vec<(String, String)>,
    session_id: Option<String>,
    negotiated_version: Option<String>,
}

impl HttpState {
    fn request(
        &mut self,
        id: i64,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let body = serde_json::to_string(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .map_err(|e| e.to_string())?;
        let resp = self.post(&body, timeout)?;
        self.capture_session(&resp);

        // Find the JSON-RPC message with our id among the reply's messages
        // (an SSE reply may interleave server notifications before the response).
        let messages = rpc_messages(&resp);
        for m in &messages {
            if m.get("id").and_then(|x| x.as_i64()) == Some(id) {
                return interpret_rpc(method, m);
            }
        }
        Err(format!(
            "{method}: no JSON-RPC response with id {id} in the server's reply"
        ))
    }

    fn notify(&mut self, method: &str, params: Value, timeout: Duration) -> Result<(), String> {
        let body =
            serde_json::to_string(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
                .map_err(|e| e.to_string())?;
        let resp = self.post(&body, timeout)?;
        self.capture_session(&resp);
        Ok(())
    }

    fn capture_session(&mut self, resp: &http::Response) {
        if let Some(sid) = resp.header("mcp-session-id") {
            self.session_id = Some(sid.to_string());
        }
    }

    fn post(&self, body: &str, timeout: Duration) -> Result<http::Response, String> {
        let mut headers = vec![(
            "Accept".to_string(),
            "application/json, text/event-stream".to_string(),
        )];
        if let Some(sid) = &self.session_id {
            headers.push(("Mcp-Session-Id".to_string(), sid.clone()));
        }
        if let Some(v) = &self.negotiated_version {
            headers.push(("MCP-Protocol-Version".to_string(), v.clone()));
        }
        headers.extend(self.extra_headers.iter().cloned());
        // The transport reports *that* TLS is missing; the remedy is the
        // bridge's to give — native MCP config or a local http proxy.
        http::post(&self.url, body, &headers, timeout).map_err(|e| match e {
            http::Error::NoTls { host } => format!(
                "this build has no TLS: the MCP→CLI bridge speaks stdio and http:// \
                 (not https, for {host}). Rebuild with `--features http-tls`, install the \
                 server as native MCP config (`oh emit`/`oh sync`), or front it with a \
                 local http proxy."
            ),
            other => other.to_string(),
        })
    }
}

/// The JSON-RPC messages in a reply body: SSE `data:` events, or a single JSON
/// document (object or batch array) for `application/json`.
fn rpc_messages(resp: &http::Response) -> Vec<Value> {
    if resp.content_type().contains("text/event-stream") {
        parse_sse(resp.body())
    } else {
        match serde_json::from_str::<Value>(resp.body()) {
            Ok(Value::Array(a)) => a,
            Ok(v) => vec![v],
            Err(_) => Vec::new(),
        }
    }
}

/// Interpret a JSON-RPC response object: `error` → Err, else its `result`.
fn interpret_rpc(method: &str, v: &Value) -> Result<Value, String> {
    if let Some(e) = v.get("error") {
        let msg = e
            .get("message")
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or_else(|| e.to_string());
        return Err(format!("{method}: {msg}"));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

/// Collect JSON-RPC messages from an SSE body (`data:` payloads, one per event).
fn parse_sse(body: &str) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut data = String::new();
    let flush = |data: &mut String, out: &mut Vec<Value>| {
        if !data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                out.push(v);
            }
            data.clear();
        }
    };
    for raw in body.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            flush(&mut data, &mut messages);
        } else if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
        }
        // Other SSE fields (event:, id:, retry:, comments) are ignored.
    }
    flush(&mut data, &mut messages);
    messages
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
