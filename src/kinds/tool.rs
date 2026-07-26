//! The tools kind — deliver a tool to each harness, MCP or MCP-free.
//!
//! A tool is defined once; open-harness picks a **delivery mode**:
//!   - `mcp`: generate the harness's native MCP config. This is where the real
//!     dialect divergence lives — `mcpServers` vs `servers` key, `url` vs
//!     `httpUrl` vs `serverUrl`, JSON vs Codex TOML, OpenCode's `type:local`.
//!   - `cli`: for a tool backed by a plain executable, emit an instructions
//!     artifact so the agent calls it through the (allowlisted) shell — **no MCP
//!     at all**. This is the answer for orgs that block MCP.
//!
//! For an MCP *server* under `cli` delivery, a true bridge needs the open-harness
//! MCP→CLI proxy runtime (tracked in #19); here we emit the documentation and a
//! loud Degraded note rather than pretend the proxy exists. And a bridge only
//! helps when the block is on the harness's MCP *client*, not on the server
//! process/egress — the emitted note says so.

use crate::adapters::Harness;
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub struct ToolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Provider {
    #[default]
    Mcp,
    Exec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Delivery {
    #[default]
    Auto,
    Mcp,
    Cli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Transport {
    #[default]
    Stdio,
    Http,
    Sse,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ServerSpec {
    #[serde(default)]
    transport: Transport,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    bearer_token_env: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ExecSpec {
    command: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolConfig {
    #[serde(default)]
    provider: Provider,
    #[serde(default)]
    delivery: Delivery,
    #[serde(default)]
    server: Option<ServerSpec>,
    #[serde(default)]
    exec: Option<ExecSpec>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    usage: Option<String>,
}

impl Kind for ToolKind {
    fn id(&self) -> KindId {
        KindId::Tool
    }

    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan {
        let cfg: ToolConfig = cap
            .manifest
            .config
            .get("tool")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();

        let delivery = match cfg.delivery {
            Delivery::Mcp => Delivery::Mcp,
            Delivery::Cli => Delivery::Cli,
            // auto: a plain executable is delivered through the shell; an MCP
            // server is delivered as native MCP config.
            Delivery::Auto => match cfg.provider {
                Provider::Exec => Delivery::Cli,
                Provider::Mcp => Delivery::Mcp,
            },
        };

        match delivery {
            Delivery::Cli => emit_cli(cap, &cfg),
            Delivery::Mcp => emit_mcp(cap, &cfg, harness),
            Delivery::Auto => unreachable!("resolved above"),
        }
    }
}

// ---- CLI delivery (MCP-free) ---------------------------------------------

fn emit_cli(cap: &LoadedCapability, cfg: &ToolConfig) -> KindPlan {
    let id = &cap.manifest.id;
    let name = if cap.manifest.name.is_empty() {
        id.clone()
    } else {
        cap.manifest.name.clone()
    };
    let desc = &cap.manifest.description;
    let path = format!(".open-harness/tools/{id}.md");

    match cfg.provider {
        Provider::Exec => {
            let Some(exec) = &cfg.exec else {
                return KindPlan::unsupported(
                    "delivery=cli with provider=exec requires an `exec` command",
                );
            };
            let cmdline = if exec.args.is_empty() {
                exec.command.clone()
            } else {
                format!("{} {}", exec.command, exec.args.join(" "))
            };
            let usage = cfg
                .usage
                .clone()
                .unwrap_or_else(|| format!("{cmdline} <args>"));
            let contents = format!(
                "# Tool: {name}\n\n{desc}\n\nThis tool runs as a plain shell command — **no MCP required**. \
                 Invoke it with your shell/Bash tool:\n\n```sh\n{usage}\n```\n\nBase command: `{cmdline}`\n"
            );
            KindPlan {
                installability: Installability::Clean,
                artifacts: vec![Artifact::File { path, contents }],
                notes: vec![format!(
                    "shell delivery: allowlist `{}` in the harness's permissions",
                    exec.command
                )],
            }
        }
        Provider::Mcp => {
            // Bridging an MCP server to the shell needs the proxy runtime (#19).
            let tools = if cfg.tools.is_empty() {
                "<tool>".to_string()
            } else {
                cfg.tools.join(", ")
            };
            let contents = format!(
                "# Tool bridge: {name}\n\n{desc}\n\nExposed through the shell so it works where the MCP client is \
                 disabled. Tools: {tools}.\n\n```sh\noh mcp call {id} <tool> --json '<args>'\n```\n\n\
                 > NOTE: this bridges the *call path* to the shell. The MCP server still runs, so it does not \
                 address a policy that blocks the server process or its network egress.\n"
            );
            KindPlan {
                installability: Installability::Degraded(
                    "MCP→CLI bridge needs the open-harness proxy runtime (#19); emitted as documentation only"
                        .into(),
                ),
                artifacts: vec![Artifact::File { path, contents }],
                notes: vec![
                    "MCP→CLI bridge runtime tracked in #19; the MCP server still runs".into(),
                ],
            }
        }
    }
}

// ---- MCP delivery (native config per harness) ----------------------------

fn emit_mcp(cap: &LoadedCapability, cfg: &ToolConfig, harness: Harness) -> KindPlan {
    let Some(s) = &cfg.server else {
        return KindPlan::unsupported("provider=mcp (mcp delivery) requires a `server` spec");
    };
    let id = &cap.manifest.id;

    let (path, contents, note): (String, String, Option<String>) = match harness {
        Harness::Aider => {
            return KindPlan::unsupported("Aider has no MCP support");
        }
        Harness::Claude => (
            ".mcp.json".into(),
            json_config("mcpServers", id, claude_entry(s)),
            None,
        ),
        Harness::Cursor => (
            ".cursor/mcp.json".into(),
            json_config("mcpServers", id, cursor_entry(s)),
            None,
        ),
        Harness::Gemini => (
            ".gemini/settings.json".into(),
            json_config("mcpServers", id, gemini_entry(s)),
            None,
        ),
        // The key divergence: VS Code / Copilot use `servers`, not `mcpServers`.
        Harness::Copilot => (
            ".vscode/mcp.json".into(),
            json_config("servers", id, vscode_entry(s)),
            None,
        ),
        Harness::OpenCode => (
            "opencode.json".into(),
            json_config("mcp", id, opencode_entry(s)),
            None,
        ),
        Harness::Windsurf => (
            "~/.codeium/windsurf/mcp_config.json".into(),
            json_config("mcpServers", id, windsurf_entry(s)),
            Some("Windsurf MCP config is global, not committed per-project".into()),
        ),
        Harness::Cline => (
            "cline_mcp_settings.json".into(),
            json_config("mcpServers", id, cline_entry(s)),
            Some(
                "Cline MCP config lives in the extension's global storage, not per-project".into(),
            ),
        ),
        Harness::Codex => (".codex/config.toml".into(), codex_toml(id, s), None),
        Harness::Pi => (
            "~/.pi/agent/mcp.json".into(),
            json_config("mcpServers", id, claude_entry(s)),
            Some(
                "Pi core has no MCP; requires the @earendil-works/pi-mcp-adapter extension".into(),
            ),
        ),
    };

    let installability = match &note {
        Some(n) => Installability::Degraded(n.clone()),
        None => Installability::Clean,
    };
    KindPlan {
        installability,
        artifacts: vec![Artifact::File { path, contents }],
        notes: note.into_iter().collect(),
    }
}

fn json_config(key: &str, id: &str, entry: Value) -> String {
    let v = json!({ key: { id: entry } });
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
}

fn map_to_value(m: &BTreeMap<String, String>) -> Value {
    Value::Object(m.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

fn stdio_common(s: &ServerSpec) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    if let Some(c) = &s.command {
        m.insert("command".into(), json!(c));
    }
    if !s.args.is_empty() {
        m.insert("args".into(), json!(s.args));
    }
    if !s.env.is_empty() {
        m.insert("env".into(), map_to_value(&s.env));
    }
    m
}

fn maybe_headers(m: &mut serde_json::Map<String, Value>, s: &ServerSpec) {
    if !s.headers.is_empty() {
        m.insert("headers".into(), map_to_value(&s.headers));
    }
}

/// Claude `.mcp.json`: stdio bare; http/sse carry an explicit `type` + `url`.
fn claude_entry(s: &ServerSpec) -> Value {
    match s.transport {
        Transport::Stdio => Value::Object(stdio_common(s)),
        Transport::Http | Transport::Sse => {
            let mut m = serde_json::Map::new();
            m.insert(
                "type".into(),
                json!(if s.transport == Transport::Sse {
                    "sse"
                } else {
                    "http"
                }),
            );
            m.insert("url".into(), json!(s.url));
            maybe_headers(&mut m, s);
            Value::Object(m)
        }
    }
}

/// Cursor infers remote from the presence of `url` (no `type`).
fn cursor_entry(s: &ServerSpec) -> Value {
    match s.transport {
        Transport::Stdio => Value::Object(stdio_common(s)),
        _ => {
            let mut m = serde_json::Map::new();
            m.insert("url".into(), json!(s.url));
            maybe_headers(&mut m, s);
            Value::Object(m)
        }
    }
}

/// Gemini uses `httpUrl` for streamable HTTP (and `url` for SSE).
fn gemini_entry(s: &ServerSpec) -> Value {
    match s.transport {
        Transport::Stdio => Value::Object(stdio_common(s)),
        Transport::Http => {
            let mut m = serde_json::Map::new();
            m.insert("httpUrl".into(), json!(s.url));
            maybe_headers(&mut m, s);
            Value::Object(m)
        }
        Transport::Sse => {
            let mut m = serde_json::Map::new();
            m.insert("url".into(), json!(s.url));
            maybe_headers(&mut m, s);
            Value::Object(m)
        }
    }
}

/// VS Code / Copilot always carry an explicit `type`.
fn vscode_entry(s: &ServerSpec) -> Value {
    match s.transport {
        Transport::Stdio => {
            let mut m = stdio_common(s);
            m.insert("type".into(), json!("stdio"));
            Value::Object(m)
        }
        _ => {
            let mut m = serde_json::Map::new();
            m.insert(
                "type".into(),
                json!(if s.transport == Transport::Sse {
                    "sse"
                } else {
                    "http"
                }),
            );
            m.insert("url".into(), json!(s.url));
            maybe_headers(&mut m, s);
            Value::Object(m)
        }
    }
}

/// OpenCode: `type: local` with `command` as an array, `environment` not `env`;
/// `type: remote` for HTTP.
fn opencode_entry(s: &ServerSpec) -> Value {
    match s.transport {
        Transport::Stdio => {
            let mut command = Vec::new();
            if let Some(c) = &s.command {
                command.push(c.clone());
            }
            command.extend(s.args.iter().cloned());
            let mut m = serde_json::Map::new();
            m.insert("type".into(), json!("local"));
            m.insert("command".into(), json!(command));
            if !s.env.is_empty() {
                m.insert("environment".into(), map_to_value(&s.env));
            }
            m.insert("enabled".into(), json!(true));
            Value::Object(m)
        }
        _ => {
            let mut m = serde_json::Map::new();
            m.insert("type".into(), json!("remote"));
            m.insert("url".into(), json!(s.url));
            maybe_headers(&mut m, s);
            m.insert("enabled".into(), json!(true));
            Value::Object(m)
        }
    }
}

/// Windsurf uses `serverUrl` for remote.
fn windsurf_entry(s: &ServerSpec) -> Value {
    match s.transport {
        Transport::Stdio => Value::Object(stdio_common(s)),
        _ => {
            let mut m = serde_json::Map::new();
            m.insert("serverUrl".into(), json!(s.url));
            maybe_headers(&mut m, s);
            Value::Object(m)
        }
    }
}

/// Cline tags remote transport as `streamableHttp`.
fn cline_entry(s: &ServerSpec) -> Value {
    match s.transport {
        Transport::Stdio => Value::Object(stdio_common(s)),
        _ => {
            let mut m = serde_json::Map::new();
            m.insert(
                "type".into(),
                json!(if s.transport == Transport::Sse {
                    "sse"
                } else {
                    "streamableHttp"
                }),
            );
            m.insert("url".into(), json!(s.url));
            maybe_headers(&mut m, s);
            Value::Object(m)
        }
    }
}

/// Codex TOML `[mcp_servers.<id>]` — `bearer_token_env_var` for HTTP auth.
fn codex_toml(id: &str, s: &ServerSpec) -> String {
    let mut out = format!("[mcp_servers.{id}]\n");
    match s.transport {
        Transport::Stdio => {
            if let Some(c) = &s.command {
                out.push_str(&format!("command = {}\n", toml_str(c)));
            }
            if !s.args.is_empty() {
                let a: Vec<String> = s.args.iter().map(|x| toml_str(x)).collect();
                out.push_str(&format!("args = [{}]\n", a.join(", ")));
            }
            if !s.env.is_empty() {
                out.push_str(&format!("\n[mcp_servers.{id}.env]\n"));
                for (k, v) in &s.env {
                    out.push_str(&format!("{k} = {}\n", toml_str(v)));
                }
            }
        }
        Transport::Http | Transport::Sse => {
            if let Some(u) = &s.url {
                out.push_str(&format!("url = {}\n", toml_str(u)));
            }
            if let Some(b) = &s.bearer_token_env {
                out.push_str(&format!("bearer_token_env_var = {}\n", toml_str(b)));
            }
        }
    }
    out
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
