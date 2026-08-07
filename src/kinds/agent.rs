//! The agent (subagent) kind — one canonical subagent → each harness's native
//! agent file (gap 1).
//!
//! A subagent is a named, scoped assistant with its own description, model, tool
//! allowance, and system prompt (the body). Three harnesses define them as files
//! today, and they disagree on frontmatter and — critically — on how the tool
//! allowance is expressed:
//!
//! | harness | path | frontmatter |
//! |---|---|---|
//! | Claude | `.claude/agents/<id>.md` | `name`, `description`, `model`, `tools` (list), `skills` (list) |
//! | OpenCode | `.opencode/agents/<id>.md` | `description`, `mode`, `model`, `temperature`, `permission` (map — `tools:` is deprecated) |
//! | Copilot | `.github/agents/<id>.agent.md` | `name`, `description`, `tools` (list), `model`, `handoffs`, `agents`, `user-invocable`, `disable-model-invocation`, `mcp-servers` |
//!
//! Everything else is **Unsupported** with a reason — most harnesses have no
//! file-defined subagent, and honest degradation says so rather than inventing a
//! target. (Antigravity is the exception: it has no native subagents but *can*
//! host a workflow shim — see the antigravity path in this kind.)
//!
//! Two model rules are load-bearing:
//!   * OpenCode has no standalone `write` permission key — writing is gated by
//!     `edit` — so a canonical `write` folds into `edit` in the permission map.
//!   * `model_tier` (`xl`/`l`/`m`/`s`) is **carried but not resolved**:
//!     open-harness is the compiler, not the model registry. When only a tier is
//!     given the file is emitted without a `model` line and a loud note asks the
//!     consumer to resolve it (or set an explicit `agent.model`).

use crate::adapters::Harness;
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;
use crate::{tools, yaml};
use serde::Deserialize;
use std::collections::BTreeMap;

pub struct AgentKind;

#[derive(Debug, Clone, Default, Deserialize)]
struct AgentConfig {
    #[serde(default)]
    description: Option<String>,
    /// Explicit model string. Portable — emitted verbatim where the harness has a
    /// `model` field.
    #[serde(default)]
    model: Option<String>,
    /// Portable tier hint (`xl`/`l`/`m`/`s`). Carried, never resolved here.
    #[serde(default, alias = "model-tier")]
    model_tier: Option<String>,
    /// Canonical tool names — mapped to each harness's native tokens.
    #[serde(default)]
    tools: Vec<String>,
    /// Canonical tool → verdict (`allow`/`ask`/`deny`), for harnesses that gate
    /// an agent's tools by permission (OpenCode) rather than a list.
    #[serde(default)]
    permissions: BTreeMap<String, String>,
    /// OpenCode agent mode: `all` | `subagent` | `primary`.
    #[serde(default)]
    mode: Option<String>,
    /// OpenCode sampling temperature.
    #[serde(default)]
    temperature: Option<f64>,
    /// Claude preloaded skills.
    #[serde(default)]
    skills: Vec<String>,
    // ---- Copilot-specific extras (emitted only for Copilot) ----
    #[serde(default)]
    handoffs: Vec<String>,
    #[serde(default)]
    agents: Vec<String>,
    #[serde(default, alias = "user-invocable")]
    user_invocable: Option<bool>,
    #[serde(default, alias = "disable-model-invocation")]
    disable_model_invocation: Option<bool>,
    #[serde(default, alias = "mcp-servers")]
    mcp_servers: Vec<String>,
    // ---- body (system prompt) ----
    #[serde(default)]
    body: String,
    #[serde(default)]
    body_file: Option<String>,
}

struct Rendered {
    path: String,
    contents: String,
    installability: Installability,
    notes: Vec<String>,
}

impl Kind for AgentKind {
    fn id(&self) -> KindId {
        KindId::Agent
    }

    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan {
        let cfg: AgentConfig = cap
            .manifest
            .config
            .get("agent")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();

        let body = match resolve_body(cap, &cfg) {
            Ok(b) => b,
            Err(e) => return KindPlan::unsupported(e),
        };
        let id = cap.manifest.id.as_str();
        let name = if cap.manifest.name.is_empty() {
            id
        } else {
            cap.manifest.name.as_str()
        };
        let desc = cfg
            .description
            .clone()
            .unwrap_or_else(|| cap.manifest.description.clone());

        let mut r = match harness {
            Harness::Claude => render_claude(id, name, &desc, &cfg, &body),
            Harness::OpenCode => render_opencode(id, &desc, &cfg, &body),
            Harness::Copilot => render_copilot(id, name, &desc, &cfg, &body),
            Harness::Antigravity => render_antigravity(id, &desc, &body),
            _ => {
                return KindPlan::unsupported(format!(
                    "{} has no file-defined subagents",
                    harness.id()
                ))
            }
        };

        apply_overrides(cap, harness, &mut r);

        let mut notes = r.notes;
        if let Installability::Degraded(reason) = &r.installability {
            notes.push(reason.clone());
        }
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

fn resolve_body(cap: &LoadedCapability, cfg: &AgentConfig) -> Result<String, String> {
    if let Some(bf) = &cfg.body_file {
        std::fs::read_to_string(cap.dir.join(bf))
            .map_err(|e| format!("cannot read body_file '{bf}': {e}"))
    } else if !cfg.body.trim().is_empty() {
        Ok(cfg.body.clone())
    } else {
        Ok(cap.manifest.description.clone())
    }
}

/// Resolve the model line for a list-style (Claude/Copilot) frontmatter. Returns
/// the pair to emit (if any) plus a note when a tier could not be resolved.
fn model_line(cfg: &AgentConfig) -> (Option<(String, String)>, Option<String>) {
    match (&cfg.model, &cfg.model_tier) {
        (Some(m), _) => (Some(("model".to_string(), yaml::scalar(m))), None),
        (None, Some(tier)) => (
            None,
            Some(format!(
                "model_tier '{tier}' is carried but not resolved by open-harness — set an explicit `agent.model` or resolve the tier to a concrete model in your profile/registry before install"
            )),
        ),
        (None, None) => (None, None),
    }
}

/// Claude `.claude/agents/<id>.md` — the reference target: name, description,
/// model, a `tools` list, and preloaded `skills`.
fn render_claude(id: &str, name: &str, desc: &str, cfg: &AgentConfig, body: &str) -> Rendered {
    let mut pairs: Vec<(String, String)> = vec![
        ("name".to_string(), yaml::scalar(name)),
        ("description".to_string(), yaml::scalar(desc)),
    ];
    let (model, tier_note) = model_line(cfg);
    if let Some(kv) = model {
        pairs.push(kv);
    }
    if !cfg.tools.is_empty() {
        let native = tools::native_tools(&cfg.tools, Harness::Claude);
        pairs.push(("tools".to_string(), yaml::flow_list(&native)));
    }
    if !cfg.skills.is_empty() {
        pairs.push(("skills".to_string(), yaml::flow_list(&cfg.skills)));
    }
    Rendered {
        path: format!(".claude/agents/{id}.md"),
        contents: with_body(&yaml::frontmatter(&pairs), body),
        installability: Installability::Clean,
        notes: tier_note.into_iter().collect(),
    }
}

/// OpenCode `.opencode/agents/<id>.md` — `tools:` is deprecated, so the tool
/// allowance is lowered to the `permission` map (a listed tool ⇒ `allow`), and
/// canonical `write` folds into the `edit` key (OpenCode has no `write` key).
fn render_opencode(id: &str, desc: &str, cfg: &AgentConfig, body: &str) -> Rendered {
    let mut pairs: Vec<(String, String)> = vec![("description".to_string(), yaml::scalar(desc))];
    if let Some(mode) = &cfg.mode {
        pairs.push(("mode".to_string(), yaml::scalar(mode)));
    }
    // model: explicit wins; a tier alone is unresolved (noted).
    let mut notes: Vec<String> = Vec::new();
    match (&cfg.model, &cfg.model_tier) {
        (Some(m), _) => pairs.push(("model".to_string(), yaml::scalar(m))),
        (None, Some(tier)) => notes.push(format!(
            "model_tier '{tier}' is carried but not resolved by open-harness — set an explicit `agent.model` or resolve the tier before install"
        )),
        (None, None) => {}
    }
    if let Some(temp) = cfg.temperature {
        pairs.push(("temperature".to_string(), temp.to_string()));
    }
    let perm = opencode_permission_map(cfg);
    if !perm.is_empty() {
        let body_map: Vec<String> = perm.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        pairs.push((
            "permission".to_string(),
            format!("{{{}}}", body_map.join(", ")),
        ));
    }
    Rendered {
        path: format!(".opencode/agents/{id}.md"),
        contents: with_body(&yaml::frontmatter(&pairs), body),
        installability: Installability::Clean,
        notes,
    }
}

/// Build the OpenCode `permission` map from the agent's tools + explicit
/// permissions. A tool in `tools` with no explicit verdict is granted (`allow`);
/// `write` folds into `edit` (deny-wins if both are present).
fn opencode_permission_map(cfg: &AgentConfig) -> Vec<(String, String)> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut set = |canonical: &str, verdict: &str| {
        let key = opencode_perm_key(canonical);
        // Most-restrictive wins when two canonical tools fold onto one key.
        let rank = |v: &str| match v {
            "deny" => 2,
            "ask" => 1,
            _ => 0,
        };
        match map.get(&key) {
            Some(existing) if rank(existing) >= rank(verdict) => {}
            _ => {
                map.insert(key, verdict.to_string());
            }
        }
    };
    for t in &cfg.tools {
        set(t, "allow");
    }
    for (t, verdict) in &cfg.permissions {
        set(t, verdict);
    }
    map.into_iter().collect()
}

/// The OpenCode permission-map key for a canonical tool. Like the tool token but
/// `write` folds into `edit` (no standalone `write` permission key).
fn opencode_perm_key(canonical: &str) -> String {
    let token = tools::native_tool(canonical, Harness::OpenCode);
    if token == "write" {
        "edit".to_string()
    } else {
        token
    }
}

/// Copilot `.github/agents/<id>.agent.md` — a `tools` list plus Copilot's own
/// orchestration frontmatter (handoffs, sub-agents, invocation flags, MCP).
fn render_copilot(id: &str, name: &str, desc: &str, cfg: &AgentConfig, body: &str) -> Rendered {
    let mut pairs: Vec<(String, String)> = vec![
        ("name".to_string(), yaml::scalar(name)),
        ("description".to_string(), yaml::scalar(desc)),
    ];
    if !cfg.tools.is_empty() {
        let native = tools::native_tools(&cfg.tools, Harness::Copilot);
        pairs.push(("tools".to_string(), yaml::flow_list(&native)));
    }
    let (model, tier_note) = model_line(cfg);
    if let Some(kv) = model {
        pairs.push(kv);
    }
    if !cfg.handoffs.is_empty() {
        pairs.push(("handoffs".to_string(), yaml::flow_list(&cfg.handoffs)));
    }
    if !cfg.agents.is_empty() {
        pairs.push(("agents".to_string(), yaml::flow_list(&cfg.agents)));
    }
    if let Some(ui) = cfg.user_invocable {
        pairs.push(("user-invocable".to_string(), ui.to_string()));
    }
    if let Some(dmi) = cfg.disable_model_invocation {
        pairs.push(("disable-model-invocation".to_string(), dmi.to_string()));
    }
    if !cfg.mcp_servers.is_empty() {
        pairs.push(("mcp-servers".to_string(), yaml::flow_list(&cfg.mcp_servers)));
    }
    Rendered {
        path: format!(".github/agents/{id}.agent.md"),
        contents: with_body(&yaml::frontmatter(&pairs), body),
        installability: Installability::Clean,
        notes: tier_note.into_iter().collect(),
    }
}

/// Antigravity has no file-defined subagents; the documented workaround is a
/// workflow shim at `.agents/workflows/subagent-<id>.md`. Emitted **Degraded** so
/// the substitution is never silent.
fn render_antigravity(id: &str, desc: &str, body: &str) -> Rendered {
    let pairs = vec![("description".to_string(), yaml::scalar(desc))];
    Rendered {
        path: format!(".agents/workflows/subagent-{id}.md"),
        contents: with_body(&yaml::frontmatter(&pairs), body),
        installability: Installability::Degraded(
            "Antigravity has no file-defined subagents; installed as a workflow shim at .agents/workflows/subagent-<id>.md (invoke it as a workflow, not an auto-delegated subagent)".into(),
        ),
        notes: Vec::new(),
    }
}

/// Apply the capability's per-harness override to a rendered agent: replace the
/// path and merge extra frontmatter. A `tools` override replaces the tool list
/// verbatim (native tokens), so it only lands cleanly on the list-style targets.
fn apply_overrides(cap: &LoadedCapability, harness: Harness, r: &mut Rendered) {
    let ov = cap.harness_override(harness.id());
    if ov.path.is_none() && ov.tools.is_none() && ov.frontmatter.is_empty() {
        return;
    }
    // Re-derive the frontmatter/body split so overrides merge structurally.
    let (mut pairs, body) = split_frontmatter(&r.contents);
    if let Some(t) = &ov.tools {
        let key = if harness == Harness::OpenCode {
            "permission"
        } else {
            "tools"
        };
        if harness == Harness::OpenCode {
            // A raw tools override on OpenCode can't become a permission map
            // safely; keep the list under `tools` and note the mismatch.
            upsert(&mut pairs, "tools", &yaml::flow_list(t));
            r.notes.push(
                "tools override on OpenCode emitted as a `tools:` list (deprecated there); prefer an explicit `permission` override".into(),
            );
        } else {
            upsert(&mut pairs, key, &yaml::flow_list(t));
        }
    }
    yaml::apply_overrides(&mut pairs, &ov.frontmatter);
    r.contents = with_body(&yaml::frontmatter(&pairs), &body);
    if let Some(p) = ov.path {
        r.path = p;
    }
}

fn upsert(pairs: &mut Vec<(String, String)>, key: &str, value: &str) {
    match pairs.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value.to_string(),
        None => pairs.push((key.to_string(), value.to_string())),
    }
}

/// Split a `---\n…\n---\n\nbody` document back into ordered frontmatter pairs and
/// the body. Best-effort: a document without frontmatter yields no pairs and the
/// whole text as body. (Values are treated opaquely — we only re-key them.)
fn split_frontmatter(doc: &str) -> (Vec<(String, String)>, String) {
    let Some(rest) = doc.strip_prefix("---\n") else {
        return (Vec::new(), doc.to_string());
    };
    let Some(end) = rest.find("\n---\n") else {
        return (Vec::new(), doc.to_string());
    };
    let front = &rest[..end];
    let body = rest[end + "\n---\n".len()..]
        .trim_start_matches('\n')
        .to_string();
    let mut pairs = Vec::new();
    for line in front.lines() {
        if let Some((k, v)) = line.split_once(": ") {
            pairs.push((k.trim().to_string(), v.trim().to_string()));
        } else if let Some(k) = line.strip_suffix(':') {
            pairs.push((k.trim().to_string(), String::new()));
        }
    }
    (pairs, body)
}

fn with_body(frontmatter: &str, body: &str) -> String {
    let mut s = String::from(frontmatter);
    s.push('\n');
    s.push_str(body.trim_end());
    s.push('\n');
    s
}
