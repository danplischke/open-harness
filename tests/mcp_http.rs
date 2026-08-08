//! Streamable-HTTP MCP bridge tests (#20).
//!
//! The client is run against a local mock MCP server over `http://`, exercising
//! all three reply encodings the transport must decode — chunked
//! `application/json` (initialize), Content-Length `application/json`
//! (tools/list), and `text/event-stream` (tools/call) — plus `Mcp-Session-Id`
//! threading and the honest `https` refusal. No subprocess, so it runs on every
//! platform: a plain `TcpListener` on an ephemeral port.

use open_harness::mcp::{self, HttpServerSpec};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

/// Spawn the mock server on an ephemeral port; return its endpoint URL.
fn start_mock() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            handle(&mut stream);
        }
    });
    format!("http://127.0.0.1:{port}/mcp")
}

fn handle(stream: &mut TcpStream) {
    let (headers, body) = read_request(stream);
    let req: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(Value::Null);

    match method {
        // Chunked application/json + the session header the client must echo back.
        "initialize" => {
            let result = json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "serverInfo": { "name": "mock", "version": "0" }
            });
            let body = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
            write_chunked(
                stream,
                "application/json",
                &[("Mcp-Session-Id", "sess-123")],
                &body,
            );
        }
        "notifications/initialized" => {
            write_status(stream, "202 Accepted");
        }
        // Plain Content-Length application/json — and prove the client threaded
        // the session + negotiated-protocol headers onto this later request.
        "tools/list" => {
            let h = headers.to_lowercase();
            if !h.contains("mcp-session-id: sess-123") || !h.contains("mcp-protocol-version:") {
                let err = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"missing session/protocol headers"}});
                write_json(stream, &[], &err.to_string());
                return;
            }
            let result = json!({ "tools": [
                { "name": "echo", "description": "echo text back", "inputSchema": {} },
                { "name": "add",  "description": "add a + b",      "inputSchema": {} }
            ]});
            let body = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
            write_json(stream, &[], &body);
        }
        // Reply as SSE (no Content-Length; read-to-EOF) to exercise the parser.
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let msg = match name {
                "echo" => {
                    let text = args.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": text }] } })
                }
                "add" => {
                    let a = args.get("a").and_then(|x| x.as_i64()).unwrap_or(0);
                    let b = args.get("b").and_then(|x| x.as_i64()).unwrap_or(0);
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": (a + b).to_string() }] } })
                }
                _ => {
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "unknown tool" } })
                }
            };
            write_sse(stream, &msg.to_string());
        }
        _ => write_status(stream, "404 Not Found"),
    }
}

fn read_request(stream: &mut TcpStream) -> (String, String) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        let n = match stream.read(&mut tmp) {
            Ok(0) | Err(_) => return (String::new(), String::new()),
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length = headers
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
        }
    }
    body.truncate(content_length);
    (headers, String::from_utf8_lossy(&body).to_string())
}

fn write_json(stream: &mut TcpStream, extra: &[(&str, &str)], body: &str) {
    let mut resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra {
        resp.push_str(&format!("{k}: {v}\r\n"));
    }
    resp.push_str("\r\n");
    resp.push_str(body);
    let _ = stream.write_all(resp.as_bytes());
}

fn write_chunked(stream: &mut TcpStream, content_type: &str, extra: &[(&str, &str)], body: &str) {
    let mut resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n"
    );
    for (k, v) in extra {
        resp.push_str(&format!("{k}: {v}\r\n"));
    }
    resp.push_str("\r\n");
    // One data chunk, then the terminating zero-length chunk.
    resp.push_str(&format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len()));
    let _ = stream.write_all(resp.as_bytes());
}

fn write_sse(stream: &mut TcpStream, message_json: &str) {
    let payload = format!("event: message\ndata: {message_json}\n\n");
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{payload}"
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn write_status(stream: &mut TcpStream, status: &str) {
    let _ = stream.write_all(
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
    );
}

/// Acceptance: the bridge lists + calls tools over streamable-HTTP, decoding a
/// chunked handshake, a Content-Length list, and an SSE call result.
#[test]
fn http_bridge_lists_and_calls_over_streamable_http() {
    let url = start_mock();
    let mut client = mcp::Client::connect_http(&HttpServerSpec {
        url,
        headers: Vec::new(),
    })
    .expect("connect http MCP server");

    let names: Vec<String> = client
        .list_tools()
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(
        names.contains(&"echo".to_string()) && names.contains(&"add".to_string()),
        "tools/list over http should advertise echo + add, got {names:?}"
    );

    let echo = client
        .call_tool("echo", json!({ "text": "round-trip" }))
        .unwrap();
    assert_eq!(mcp::result_text(&echo), "round-trip");

    let add = client
        .call_tool("add", json!({ "a": 19, "b": 23 }))
        .unwrap();
    assert_eq!(mcp::result_text(&add), "42");
}

/// An unknown tool surfaces the server's JSON-RPC error (delivered over SSE).
#[test]
fn http_bridge_unknown_tool_errors() {
    let url = start_mock();
    let mut client = mcp::Client::connect_http(&HttpServerSpec {
        url,
        headers: Vec::new(),
    })
    .unwrap();
    assert!(client.call_tool("does-not-exist", json!({})).is_err());
}

/// The `oh mcp call --id <cap>` path: a `transport: http` capability resolves to
/// an HTTP server (url + headers), not a stdio one.
#[test]
fn server_from_capability_reads_an_http_transport() {
    let dir = std::env::temp_dir().join(format!("oh-mcp-http-{}", std::process::id()));
    let cap_dir = dir.join("remote-tool");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&cap_dir).unwrap();
    std::fs::write(
        cap_dir.join("capability.yaml"),
        r#"id: remote-tool
name: Remote tool
description: an http MCP server
kind: tool
tool:
  provider: mcp
  delivery: cli
  server:
    transport: http
    url: http://127.0.0.1:9/mcp
    headers:
      X-Team: research
"#,
    )
    .unwrap();

    let cap = open_harness::manifest::discover(&dir)
        .unwrap()
        .into_iter()
        .find(|c| c.manifest.id == "remote-tool")
        .expect("capability discovered");

    match mcp::server_from_capability(&cap).unwrap() {
        mcp::McpServer::Http(h) => {
            assert_eq!(h.url, "http://127.0.0.1:9/mcp");
            assert!(
                h.headers
                    .iter()
                    .any(|(k, v)| k == "X-Team" && v == "research"),
                "custom headers carry through: {:?}",
                h.headers
            );
        }
        mcp::McpServer::Stdio(_) => panic!("expected an http server, got stdio"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the `mcp-http-tls` feature, `https` is refused honestly (no TLS),
/// pointing at native config — never a silent failure or a hang.
#[cfg(not(feature = "mcp-http-tls"))]
#[test]
fn http_bridge_refuses_https_with_a_clear_message() {
    let err = mcp::Client::connect_http(&HttpServerSpec {
        url: "https://mcp.example.com/mcp".to_string(),
        headers: Vec::new(),
    })
    .err()
    .expect("https must be refused");
    assert!(
        err.contains("https") && (err.contains("native") || err.contains("proxy")),
        "the refusal should be actionable, got: {err}"
    );
}

/// With the feature on, `https` routes to the TLS transport — a closed port
/// yields a connect/TLS error, NOT the "no TLS in this build" refusal.
#[cfg(feature = "mcp-http-tls")]
#[test]
fn https_routes_to_the_tls_transport_when_enabled() {
    let err = mcp::Client::connect_http(&HttpServerSpec {
        url: "https://127.0.0.1:1/mcp".to_string(), // port 1: nothing listens
        headers: Vec::new(),
    })
    .err()
    .expect("connecting to a closed port fails");
    assert!(
        !err.contains("no TLS") && !err.contains("Rebuild"),
        "https must reach the TLS transport, not the no-TLS refusal, got: {err}"
    );
}
