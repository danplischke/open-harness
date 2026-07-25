//! The skill kind — unify `SKILL.md` across vendors.
//!
//! Skills are the *easy* kind: the "Agent Skills" `SKILL.md` format (YAML
//! frontmatter + Markdown body) is already a near-standard, so unification is a
//! path + frontmatter transform with no execution-model problem. This kind is
//! purely generative — it emits a `File` artifact per supporting harness and has
//! no runtime.
//!
//! Supported today: Claude Code, Codex, Cursor, Windsurf (each has a skills
//! directory). Everything else is reported Unsupported by the matrix.

use crate::adapters::Harness;
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;
use serde::Deserialize;

pub struct SkillKind;

#[derive(Debug, Clone, Default, Deserialize)]
struct SkillConfig {
    #[serde(default)]
    body: String,
    #[serde(default)]
    body_file: Option<String>,
    #[serde(default)]
    allowed_tools: Vec<String>,
}

/// Where each harness expects `SKILL.md` folders. `None` = no skills support.
fn skill_dir(h: Harness) -> Option<&'static str> {
    match h {
        Harness::Claude => Some(".claude/skills"),
        Harness::Codex => Some(".agents/skills"),
        Harness::Cursor => Some(".cursor/skills"),
        Harness::Windsurf => Some(".windsurf/skills"),
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

        let contents = render_skill_md(
            &cap.manifest.name,
            &cap.manifest.description,
            &cfg.allowed_tools,
            &body,
        );
        let path = format!("{dir}/{}/SKILL.md", cap.manifest.id);

        KindPlan {
            installability: Installability::Clean,
            artifacts: vec![Artifact::File { path, contents }],
            notes: Vec::new(),
        }
    }
}

/// Render a normalized skill to `SKILL.md`. (Frontmatter values are emitted
/// as-is; a production version would YAML-quote to be safe with `:` and `#`.)
fn render_skill_md(name: &str, description: &str, allowed_tools: &[String], body: &str) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str(&format!("description: {description}\n"));
    if !allowed_tools.is_empty() {
        out.push_str(&format!("allowed-tools: {}\n", allowed_tools.join(", ")));
    }
    out.push_str("---\n\n");
    out.push_str(body.trim_end());
    out.push('\n');
    out
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

/// Parse a `SKILL.md` document into a normalized skill.
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
            let (k, v) = (k.trim(), v.trim());
            match k {
                "name" => skill.name = v.to_string(),
                "description" => skill.description = v.to_string(),
                "allowed-tools" => {
                    skill.allowed_tools = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                _ => {}
            }
        }
    }
    skill.body = body.trim_end().to_string();
    Ok(skill)
}
