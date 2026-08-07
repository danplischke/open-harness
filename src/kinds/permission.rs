//! The permissions kind — normalized policy → best-effort per-harness.
//!
//! Permissions are the least unifiable surface: five incompatible models. A
//! normalized policy is per-tool / per-pattern rules with a verdict
//! (`allow` | `ask` | `deny`) plus a default. Only Claude and OpenCode can
//! express that faithfully; everyone else is an **approximation**, so those
//! targets are emitted **Degraded** with a loud loss report, and the harnesses
//! with no committed permission file at all are **Unsupported** — never silently
//! dropped.
//!
//! | harness | target | fidelity |
//! |---|---|---|
//! | Claude | `.claude/settings.json` `permissions` allow/deny/ask | faithful |
//! | OpenCode | `opencode.json` `permission` per-tool globs | faithful |
//! | Codex | `approval_policy` + `sandbox_mode` (TOML) | coarse collapse |
//! | Gemini | `tools.allowed` / `tools.exclude` | shell-focused, lossy |
//! | Cursor | `permissions.json` terminalAllowlist / autoRun block | shell-only |
//! | Windsurf | `cascadeCommandsAllowList` / `DenyList` (settings) | shell-only |
//! | Copilot | `chat.tools.terminal.autoApprove` (VS Code settings) | shell-only |
//! | Cline | runtime auto-approve UI / YOLO — no committed file | Unsupported |
//! | Aider | CLI flags (`--yes-always`) — no committed file | Unsupported |
//! | Pi | no permission system by design (containerize) | Unsupported |

use crate::adapters::Harness;
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub struct PermissionKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Verdict {
    Allow,
    #[default]
    Ask,
    Deny,
}

impl Verdict {
    fn as_str(&self) -> &'static str {
        match self {
            Verdict::Allow => "allow",
            Verdict::Ask => "ask",
            Verdict::Deny => "deny",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Rule {
    tool: String,
    #[serde(default = "star")]
    pattern: String,
    #[serde(default)]
    verdict: Verdict,
}

fn star() -> String {
    "*".to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PermissionConfig {
    #[serde(default)]
    default: Verdict,
    #[serde(default)]
    rules: Vec<Rule>,
}

struct Rendered {
    path: String,
    contents: String,
    installability: Installability,
}

impl Kind for PermissionKind {
    fn id(&self) -> KindId {
        KindId::Permission
    }

    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan {
        let cfg: PermissionConfig = match crate::kind::kind_config(cap, "permission") {
            Ok(c) => c,
            Err(e) => return KindPlan::unsupported(e),
        };

        let r = match harness {
            Harness::Claude => render_claude(&cfg),
            Harness::OpenCode => render_opencode(&cfg),
            Harness::Codex => render_codex(&cfg),
            Harness::Gemini => render_gemini(&cfg),
            Harness::Cursor => render_cursor(&cfg),
            Harness::Windsurf => render_windsurf(&cfg),
            Harness::Copilot => render_copilot(&cfg),
            Harness::Cline => {
                return KindPlan::unsupported(
                    "Cline uses a runtime auto-approve UI / YOLO, not a committed allowlist",
                )
            }
            Harness::Aider => {
                return KindPlan::unsupported(
                    "Aider uses CLI flags (--yes-always), not a committed permission file",
                )
            }
            Harness::Pi => return KindPlan::unsupported(
                "Pi has no permission system by design — run in a container or add an extension",
            ),
            Harness::Antigravity => {
                return KindPlan::unsupported("Antigravity has no known committed permission file")
            }
        };

        let notes = match &r.installability {
            Installability::Degraded(reason) => vec![reason.clone()],
            _ => Vec::new(),
        };
        KindPlan {
            installability: r.installability,
            artifacts: vec![Artifact::File {
                path: r.path,
                contents: r.contents,
            }],
            notes,
        }
    }
}

// ---- faithful targets -----------------------------------------------------

/// Claude `.claude/settings.json` — `ToolName(pattern)` in allow/deny/ask.
fn render_claude(cfg: &PermissionConfig) -> Rendered {
    let mut allow = Vec::new();
    let mut ask = Vec::new();
    let mut deny = Vec::new();
    for r in &cfg.rules {
        let entry = format!(
            "{}({})",
            crate::tools::native_tool(&r.tool, Harness::Claude),
            r.pattern
        );
        match r.verdict {
            Verdict::Allow => allow.push(entry),
            Verdict::Ask => ask.push(entry),
            Verdict::Deny => deny.push(entry),
        }
    }
    let v = json!({ "permissions": { "allow": allow, "deny": deny, "ask": ask } });
    // Claude's implicit default is "ask"; a non-ask default isn't a first-class
    // Claude concept, so flag it rather than silently changing behavior.
    let inst = if cfg.default == Verdict::Ask {
        Installability::Clean
    } else {
        Installability::Degraded(format!(
            "default verdict '{}' has no direct Claude equivalent; unmatched tools follow Claude's ask default",
            cfg.default.as_str()
        ))
    };
    Rendered {
        path: ".claude/settings.json".to_string(),
        contents: pretty(&v),
        installability: inst,
    }
}

/// OpenCode `opencode.json` — per-tool `{pattern: verdict}` maps + `*` default.
fn render_opencode(cfg: &PermissionConfig) -> Rendered {
    let mut perm = serde_json::Map::new();
    perm.insert("*".to_string(), json!(cfg.default.as_str()));
    for (tool, rules) in by_tool(&cfg.rules) {
        let mut obj = serde_json::Map::new();
        for r in rules {
            obj.insert(r.pattern.clone(), json!(r.verdict.as_str()));
        }
        perm.insert(tool, Value::Object(obj));
    }
    let v = json!({ "permission": perm });
    Rendered {
        path: "opencode.json".to_string(),
        contents: pretty(&v),
        installability: Installability::Clean,
    }
}

// ---- approximated targets (Degraded) --------------------------------------

/// Codex has no per-command rules — collapse to a coarse approval policy.
fn render_codex(cfg: &PermissionConfig) -> Rendered {
    let restrictive = cfg.default != Verdict::Allow
        || cfg
            .rules
            .iter()
            .any(|r| matches!(r.verdict, Verdict::Deny | Verdict::Ask));
    let policy = if restrictive { "on-request" } else { "never" };
    let contents = format!("approval_policy = \"{policy}\"\nsandbox_mode = \"workspace-write\"\n");
    Rendered {
        path: ".codex/config.toml".to_string(),
        contents,
        installability: Installability::Degraded(format!(
            "Codex has no per-command rules; {} rule(s) collapsed to approval_policy=\"{policy}\" + sandbox_mode — review manually",
            cfg.rules.len()
        )),
    }
}

/// Gemini `tools.allowed` (bypass) + `tools.exclude` (whole-tool block).
fn render_gemini(cfg: &PermissionConfig) -> Rendered {
    let mut allowed = Vec::new();
    let mut exclude = Vec::new();
    for r in &cfg.rules {
        match r.verdict {
            Verdict::Allow => allowed.push(gemini_allow_entry(r)),
            Verdict::Deny => exclude.push(crate::tools::native_tool(&r.tool, Harness::Gemini)),
            Verdict::Ask => {}
        }
    }
    exclude.sort();
    exclude.dedup();
    let v = json!({ "tools": { "allowed": allowed, "exclude": exclude } });
    Rendered {
        path: ".gemini/settings.json".to_string(),
        contents: pretty(&v),
        installability: Installability::Degraded(
            "Gemini can't express per-pattern deny or ask; approximated via tools.allowed (bypass) and tools.exclude (whole-tool block)".into(),
        ),
    }
}

/// Cursor `permissions.json` — shell allow/deny only.
fn render_cursor(cfg: &PermissionConfig) -> Rendered {
    let allow: Vec<String> = shell_patterns(cfg, Verdict::Allow);
    let block: Vec<String> = shell_patterns(cfg, Verdict::Deny);
    let v = json!({
        "terminalAllowlist": allow,
        "autoRun": { "block_instructions": block }
    });
    Rendered {
        path: ".cursor/permissions.json".to_string(),
        contents: pretty(&v),
        installability: Installability::Degraded(
            "Cursor permissions map only shell rules (terminalAllowlist / autoRun block); non-shell rules dropped".into(),
        ),
    }
}

/// Windsurf allow/deny lists live in IDE settings — emit a snippet.
fn render_windsurf(cfg: &PermissionConfig) -> Rendered {
    let v = json!({
        "windsurf.cascadeCommandsAllowList": shell_patterns(cfg, Verdict::Allow),
        "windsurf.cascadeCommandsDenyList": shell_patterns(cfg, Verdict::Deny)
    });
    Rendered {
        path: ".vscode/settings.json".to_string(),
        contents: pretty(&v),
        installability: Installability::Degraded(
            "Windsurf permissions are global IDE settings and shell-only; snippet covers command allow/deny lists".into(),
        ),
    }
}

/// Copilot (VS Code) terminal auto-approve map — shell allow=true / deny=false.
fn render_copilot(cfg: &PermissionConfig) -> Rendered {
    let mut map = serde_json::Map::new();
    for r in cfg.rules.iter().filter(|r| r.tool == "bash") {
        match r.verdict {
            Verdict::Allow => {
                map.insert(r.pattern.clone(), json!(true));
            }
            Verdict::Deny => {
                map.insert(r.pattern.clone(), json!(false));
            }
            Verdict::Ask => {}
        }
    }
    let v = json!({ "chat.tools.terminal.autoApprove": map });
    Rendered {
        path: ".vscode/settings.json".to_string(),
        contents: pretty(&v),
        installability: Installability::Degraded(
            "Copilot maps only terminal commands (chat.tools.terminal.autoApprove); non-shell rules dropped".into(),
        ),
    }
}

// ---- helpers --------------------------------------------------------------

fn by_tool(rules: &[Rule]) -> BTreeMap<String, Vec<&Rule>> {
    let mut m: BTreeMap<String, Vec<&Rule>> = BTreeMap::new();
    for r in rules {
        m.entry(r.tool.clone()).or_default().push(r);
    }
    m
}

fn shell_patterns(cfg: &PermissionConfig, v: Verdict) -> Vec<String> {
    cfg.rules
        .iter()
        .filter(|r| r.tool == "bash" && r.verdict == v)
        .map(|r| r.pattern.clone())
        .collect()
}

/// Gemini `tools.allowed` entries carry command specificity for shell.
fn gemini_allow_entry(r: &Rule) -> String {
    if r.tool == "bash" {
        let cmd = r.pattern.split_whitespace().next().unwrap_or("*");
        format!("run_shell_command({cmd})")
    } else {
        crate::tools::native_tool(&r.tool, Harness::Gemini)
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}
