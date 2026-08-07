//! The skill kind — unify `SKILL.md` across vendors.
//!
//! Skills are the *easy* kind: the "Agent Skills" `SKILL.md` format (YAML
//! frontmatter + Markdown body) is already a near-standard, so unification is a
//! path + frontmatter transform with no execution-model problem. This kind is
//! purely generative — it emits a `File` artifact per supporting harness and has
//! no runtime.
//!
//! ## Frontmatter
//!
//! The three core fields (`name`, `description`, `allowed-tools`) are always
//! emitted; `allowed-tools` maps canonical tool names to each harness's native
//! tokens ([`crate::tools`]). Beyond those, a `skill.frontmatter` object is
//! passed through verbatim on **every** harness — this carries the Agent Skills
//! spec fields (`license`, `metadata`, `compatibility`), the forward-compat spec
//! keys (`triggers`, `prerequisite-skills`, `related-skills`), and anything else
//! portable. Harness-*specific* frontmatter (e.g. Claude-only `argument-hint` /
//! `user-invocable`) belongs in the per-harness `overrides` block, so it lands on
//! Claude and is stripped elsewhere.
//!
//! ## Targets
//!
//! | harness | directory |
//! |---|---|
//! | Claude | `.claude/skills` |
//! | Codex / Antigravity | `.agents/skills` (shared — sync converges the copy) |
//! | Cursor | `.cursor/skills` |
//! | Windsurf | `.windsurf/skills` |
//! | Copilot | `.github/skills` |
//! | OpenCode | `.opencode/skills` |
//!
//! Gemini/Cline/Pi/Aider have no `SKILL.md` directory and are reported
//! Unsupported. (De-duplicating the identical file across a tool's fallback
//! discovery path — writing once and letting others find it — is a possible
//! follow-up; today each target gets its own copy, which is always correct.)

use crate::adapters::Harness;
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;
use crate::{tools, yaml};
use serde::Deserialize;
use serde_json::{Map, Value};

pub struct SkillKind;

#[derive(Debug, Clone, Default, Deserialize)]
struct SkillConfig {
    #[serde(default)]
    body: String,
    #[serde(default)]
    body_file: Option<String>,
    #[serde(default)]
    allowed_tools: Vec<String>,
    /// Portable frontmatter passthrough — emitted on every harness.
    #[serde(default)]
    frontmatter: Map<String, Value>,
}

/// Where each harness expects `SKILL.md` folders. `None` = no skills support.
fn skill_dir(h: Harness) -> Option<&'static str> {
    match h {
        Harness::Claude => Some(".claude/skills"),
        // Codex and Antigravity 2.0 share `.agents/skills` — a genuine same-path
        // collision between two *harnesses*; identical content, so the sync layer
        // converges them to one file.
        Harness::Codex | Harness::Antigravity => Some(".agents/skills"),
        Harness::Cursor => Some(".cursor/skills"),
        Harness::Windsurf => Some(".windsurf/skills"),
        Harness::Copilot => Some(".github/skills"),
        Harness::OpenCode => Some(".opencode/skills"),
        _ => None,
    }
}

impl Kind for SkillKind {
    fn id(&self) -> KindId {
        KindId::Skill
    }

    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan {
        let Some(dir) = skill_dir(harness) else {
            return KindPlan::unsupported(format!(
                "{} has no SKILL.md skills directory",
                harness.id()
            ));
        };

        let cfg: SkillConfig = cap
            .manifest
            .config
            .get("skill")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();

        let body = if let Some(bf) = &cfg.body_file {
            match std::fs::read_to_string(cap.dir.join(bf)) {
                Ok(s) => s,
                Err(e) => {
                    return KindPlan::unsupported(format!("cannot read body_file '{bf}': {e}"))
                }
            }
        } else if !cfg.body.trim().is_empty() {
            cfg.body.clone()
        } else {
            cap.manifest.description.clone()
        };

        let name = if cap.manifest.name.is_empty() {
            cap.manifest.id.as_str()
        } else {
            cap.manifest.name.as_str()
        };

        // Core frontmatter — YAML-quoted so a `:` or `#` in a value is safe.
        let mut pairs: Vec<(String, String)> = vec![
            ("name".to_string(), yaml::scalar(name)),
            (
                "description".to_string(),
                yaml::scalar(&cap.manifest.description),
            ),
        ];
        if !cfg.allowed_tools.is_empty() {
            let native = tools::native_tools(&cfg.allowed_tools, harness);
            pairs.push(("allowed-tools".to_string(), native.join(", ")));
        }
        // Portable passthrough (never clobber a core key).
        for (k, v) in &cfg.frontmatter {
            if matches!(k.as_str(), "name" | "description" | "allowed-tools") {
                continue;
            }
            pairs.push((k.clone(), yaml::render_value(v)));
        }

        // Per-harness overrides: a tools override replaces `allowed-tools`
        // verbatim; frontmatter overrides merge; a path override relocates.
        let ov = cap.harness_override(harness.id());
        if let Some(t) = &ov.tools {
            match pairs.iter_mut().find(|(k, _)| k == "allowed-tools") {
                Some(slot) => slot.1 = t.join(", "),
                None => pairs.push(("allowed-tools".to_string(), t.join(", "))),
            }
        }
        yaml::apply_overrides(&mut pairs, &ov.frontmatter);

        let path = ov
            .path
            .unwrap_or_else(|| format!("{dir}/{}/SKILL.md", cap.manifest.id));
        let contents = with_body(&yaml::frontmatter(&pairs), &body);

        KindPlan {
            installability: Installability::Clean,
            artifacts: vec![Artifact::File { path, contents }],
            notes: Vec::new(),
        }
    }
}

fn with_body(frontmatter: &str, body: &str) -> String {
    let mut s = String::from(frontmatter);
    s.push('\n');
    s.push_str(body.trim_end());
    s.push('\n');
    s
}

/// A skill parsed back out of a `SKILL.md` file — the inverse of `render`, used
/// for importing existing vendor skills into the normalized model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportedSkill {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub body: String,
}

/// Parse a `SKILL.md` document into a normalized skill. Handles plain and
/// double-quoted scalar values (so a description containing `:` round-trips) and
/// `allowed-tools` as either a comma list or a `[a, b]` flow list. Block scalars
/// and nested maps are out of scope (a line-based parser); unknown keys are kept
/// out of the core fields.
pub fn import_skill_md(text: &str) -> Result<ImportedSkill, String> {
    let text = text.trim_start();
    let after_open = text
        .strip_prefix("---")
        .ok_or("missing frontmatter opening '---'")?;
    let close = after_open
        .find("\n---")
        .ok_or("missing frontmatter closing '---'")?;
    let frontmatter = &after_open[..close];
    let body = after_open[close + "\n---".len()..]
        .trim_start_matches(['\n', '\r'])
        .to_string();

    let mut skill = ImportedSkill::default();
    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim(), unquote(v.trim()));
            match k {
                "name" => skill.name = v,
                "description" => skill.description = v,
                "allowed-tools" => skill.allowed_tools = parse_tool_list(&v),
                _ => {}
            }
        }
    }
    skill.body = body.trim_end().to_string();
    Ok(skill)
}

/// Unwrap a double-quoted YAML scalar (with basic escape handling); pass a plain
/// scalar through unchanged.
fn unquote(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => out.push(other),
                    None => {}
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        v.to_string()
    }
}

/// Parse an `allowed-tools` value: a `[a, b]` flow list or a bare comma list.
fn parse_tool_list(v: &str) -> Vec<String> {
    let inner = v
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(v);
    inner
        .split(',')
        .map(|s| unquote(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}
