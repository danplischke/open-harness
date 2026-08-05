//! MCP→CLI bridge tests (#19) — the client run against the `echo-bridge` fixture
//! server. `#[cfg(unix)]` because it spawns a Python server and does interactive
//! stdio; the client itself is platform-independent.
#![cfg(unix)]

use open_harness::manifest::{discover, LoadedCapability};
use open_harness::mcp::{self, ServerSpec};
use serde_json::json;
use std::path::Path;

fn echo_bridge() -> LoadedCapability {
    discover(Path::new("capabilities"))
        .expect("capabilities load")
        .into_iter()
        .find(|c| c.manifest.id == "echo-bridge")
        .expect("echo-bridge capability present")
}

/// Acceptance: `oh mcp` invokes a real stdio MCP server (via the capability's
/// `tool.server` spec) and returns tool results.
#[test]
fn mcp_client_lists_and_calls_tools() {
    let server = mcp::server_from_capability(&echo_bridge()).unwrap();
    let mut client = mcp::Client::connect(&server).expect("start MCP server");

    let names: Vec<String> = client
        .list_tools()
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(
        names.contains(&"echo".to_string()) && names.contains(&"add".to_string()),
        "tools/list should advertise echo + add, got {names:?}"
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

/// The direct `--command` mode (no capability): spawn the server by command.
#[test]
fn mcp_direct_command_spec_works() {
    let spec = ServerSpec {
        command: "python3".into(),
        args: vec!["capabilities/echo-bridge/server.py".into()],
        env: Vec::new(),
        cwd: None,
    };
    let mut client = mcp::Client::start(&spec).expect("start MCP server");
    let r = client
        .call_tool("echo", json!({ "text": "direct" }))
        .unwrap();
    assert_eq!(mcp::result_text(&r), "direct");
}

/// An unknown tool surfaces the server's JSON-RPC error, not a hang.
#[test]
fn mcp_unknown_tool_errors() {
    let server = mcp::server_from_capability(&echo_bridge()).unwrap();
    let mut client = mcp::Client::connect(&server).unwrap();
    assert!(client.call_tool("does-not-exist", json!({})).is_err());
}
