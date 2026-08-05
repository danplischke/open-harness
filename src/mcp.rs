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
//!     one, threading the `Mcp-Session-Id` header. Hand-rolled over `TcpStream`
//!     to keep the default build dependency-free. `http://` needs no dependency;
//!     `https://` uses the optional **`mcp-http-tls`** feature (rustls, #25) —
//!     without it, an `https` endpoint is **honestly refused** with a pointer to
//!     native MCP config (`oh emit`/`oh sync`) or a local http proxy. With TLS
//!     on, `OPEN_HARNESS_CA_FILE` adds a private/corporate CA to the Mozilla
//!     roots.

use crate::manifest::{LoadedCapability, RunSpec};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
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
        let messages = resp.rpc_messages();
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

    fn capture_session(&mut self, resp: &HttpResponse) {
        if let Some(sid) = resp.header("mcp-session-id") {
            self.session_id = Some(sid.to_string());
        }
    }

    fn post(&self, body: &str, timeout: Duration) -> Result<HttpResponse, String> {
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
        http_post(&self.url, body, &headers, timeout)
    }
}

/// A parsed HTTP response (headers lower-cased for case-insensitive lookup).
struct HttpResponse {
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        let want = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == want)
            .map(|(_, v)| v.as_str())
    }

    fn content_type(&self) -> &str {
        self.header("content-type").unwrap_or("")
    }

    /// The JSON-RPC messages in the body: SSE `data:` events, or a single JSON
    /// document (object or batch array) for `application/json`.
    fn rpc_messages(&self) -> Vec<Value> {
        if self.content_type().contains("text/event-stream") {
            parse_sse(&self.body)
        } else {
            match serde_json::from_str::<Value>(&self.body) {
                Ok(Value::Array(a)) => a,
                Ok(v) => vec![v],
                Err(_) => Vec::new(),
            }
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

/// A tiny HTTP/1.1 `POST` — enough for streamable-HTTP MCP, hand-rolled over
/// `TcpStream`. `http://` needs no dependency; `https://` uses the optional
/// `mcp-http-tls` feature (rustls), and without it is refused honestly. Uses
/// `Connection: close` so the body is delimited by EOF (a per-request response
/// stream, per the spec).
fn http_post(
    url: &str,
    body: &str,
    extra_headers: &[(String, String)],
    timeout: Duration,
) -> Result<HttpResponse, String> {
    let (scheme, host, port, path) = parse_url(url)?;
    let request = build_request(&host, &path, body, extra_headers);
    let raw = match scheme.as_str() {
        "http" => send_plain(&host, port, request.as_bytes(), timeout)?,
        "https" => send_tls(&host, port, request.as_bytes(), timeout)?,
        other => {
            return Err(format!(
                "unsupported MCP URL scheme '{other}://' (want http/https)"
            ))
        }
    };
    parse_http_response(&raw)
}

/// Build the raw HTTP/1.1 request bytes (shared by the plain + TLS transports).
fn build_request(host: &str, path: &str, body: &str, extra_headers: &[(String, String)]) -> String {
    let mut req = String::new();
    req.push_str(&format!("POST {path} HTTP/1.1\r\n"));
    req.push_str(&format!("Host: {host}\r\n"));
    req.push_str("Content-Type: application/json\r\n");
    req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    req.push_str("Connection: close\r\n");
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    req
}

/// Connect a `TcpStream` to `host:port` with the timeout applied to connect,
/// read, and write.
fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}:{port}: {e}"))?
        .next()
        .ok_or_else(|| format!("no address for {host}:{port}"))?;
    let stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect {addr}: {e}"))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    Ok(stream)
}

/// Plain-HTTP transport: write the request, read the response to EOF.
fn send_plain(host: &str, port: u16, request: &[u8], timeout: Duration) -> Result<Vec<u8>, String> {
    let mut stream = connect(host, port, timeout)?;
    stream
        .write_all(request)
        .and_then(|_| stream.flush())
        .map_err(|e| format!("write to MCP server: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read from MCP server: {e}"))?;
    Ok(raw)
}

/// TLS transport (`https`), gated on `mcp-http-tls` (rustls). Trusts the Mozilla
/// root set plus, if `OPEN_HARNESS_CA_FILE` points to a PEM bundle, that CA too
/// (for a private/corporate CA or a test server).
#[cfg(feature = "mcp-http-tls")]
fn send_tls(host: &str, port: u16, request: &[u8], timeout: Duration) -> Result<Vec<u8>, String> {
    use std::sync::Arc;

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Ok(ca_path) = std::env::var("OPEN_HARNESS_CA_FILE") {
        let pem = std::fs::read(&ca_path).map_err(|e| format!("read CA {ca_path}: {e}"))?;
        for cert in rustls_pemfile_certs(&pem) {
            let _ = roots.add(cert);
        }
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| format!("invalid TLS server name '{host}'"))?;
    let mut conn = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| format!("tls setup: {e}"))?;
    let mut sock = connect(host, port, timeout)?;
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);

    tls.write_all(request)
        .and_then(|_| tls.flush())
        .map_err(|e| format!("tls write to MCP server: {e}"))?;
    let mut raw = Vec::new();
    match tls.read_to_end(&mut raw) {
        Ok(_) => Ok(raw),
        // A server that closes the TLS session ungracefully after the response
        // still delivered a complete body; surface a hard failure only if empty.
        Err(_) if !raw.is_empty() => Ok(raw),
        Err(e) => Err(format!("tls read from MCP server: {e}")),
    }
}

#[cfg(not(feature = "mcp-http-tls"))]
fn send_tls(
    host: &str,
    _port: u16,
    _request: &[u8],
    _timeout: Duration,
) -> Result<Vec<u8>, String> {
    Err(format!(
        "this build has no TLS: the MCP→CLI bridge speaks stdio and http:// (not https, for {host}). \
         Rebuild with `--features mcp-http-tls`, install the server as native MCP config \
         (`oh emit`/`oh sync`), or front it with a local http proxy."
    ))
}

/// Minimal PEM certificate extractor (avoids a `rustls-pemfile` dependency):
/// pull each `-----BEGIN CERTIFICATE-----` block and base64-decode it.
#[cfg(feature = "mcp-http-tls")]
fn rustls_pemfile_certs(pem: &[u8]) -> Vec<rustls::pki_types::CertificateDer<'static>> {
    let text = String::from_utf8_lossy(pem);
    let mut out = Vec::new();
    let mut in_cert = false;
    let mut b64 = String::new();
    for line in text.lines() {
        if line.contains("BEGIN CERTIFICATE") {
            in_cert = true;
            b64.clear();
        } else if line.contains("END CERTIFICATE") {
            if let Some(der) = base64_decode(&b64) {
                out.push(rustls::pki_types::CertificateDer::from(der));
            }
            in_cert = false;
        } else if in_cert {
            b64.push_str(line.trim());
        }
    }
    out
}

/// Standard base64 decode (no external crate), for PEM certificate bodies.
#[cfg(feature = "mcp-http-tls")]
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Split a `scheme://host[:port][/path]` URL. Path defaults to `/`.
fn parse_url(url: &str) -> Result<(String, String, u16, String), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("malformed URL '{url}' (want scheme://host…)"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("malformed URL '{url}' (no host)"));
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| format!("bad port in '{url}'"))?,
        ),
        None => (authority.to_string(), default_port),
    };
    Ok((scheme.to_string(), host, port, path.to_string()))
}

/// Parse a raw HTTP/1.1 response: status line, headers (lower-cased), decoded body.
fn parse_http_response(raw: &[u8]) -> Result<HttpResponse, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("truncated HTTP response (no header terminator)")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body_bytes = &raw[split + 4..];

    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("bad HTTP status line '{status_line}'"))?;

    let mut headers = Vec::new();
    let mut chunked = false;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key == "transfer-encoding" && val.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
            headers.push((key, val));
        }
    }

    let body = if chunked {
        dechunk(body_bytes)
    } else {
        body_bytes.to_vec()
    };
    let body = String::from_utf8_lossy(&body).into_owned();

    if status >= 400 {
        let snippet: String = body.chars().take(200).collect();
        return Err(format!("MCP server returned HTTP {status}: {snippet}"));
    }
    Ok(HttpResponse { headers, body })
}

/// Decode HTTP/1.1 chunked transfer-encoding.
fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        // A chunk-size line may carry a `;ext` suffix; take the hex prefix only.
        let line = String::from_utf8_lossy(&b[..nl]);
        let hex = line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(hex, 16) else {
            break;
        };
        if size == 0 {
            break; // last chunk
        }
        let (start, end) = (nl + 2, nl + 2 + size);
        if end > b.len() {
            out.extend_from_slice(&b[start.min(b.len())..]); // truncated — salvage
            break;
        }
        out.extend_from_slice(&b[start..end]);
        b = &b[(end + 2).min(b.len())..]; // skip data + trailing CRLF
    }
    out
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
