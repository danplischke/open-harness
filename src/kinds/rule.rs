//! The rules kind — map rule metadata across harnesses.
//!
//! The same concept — "apply this guidance, conditionally" — is spelled five
//! different ways. This kind normalizes an **activation** (always / glob /
//! agent-requested / manual) plus a body, and renders each harness's native rule
//! file. Only Cursor and Windsurf support all four activations; on Copilot,
//! Cline, and Claude the model-decision and manual modes have no target, so they
//! are emitted **Degraded** with a loud note rather than silently changed.
//!
//! | activation | Cursor `.mdc` | Copilot `.instructions.md` | Windsurf | Cline / Claude |
//! |---|---|---|---|---|
//! | always | `alwaysApply: true` | `applyTo: "**"` | `trigger: always_on` | (no `paths`) |
//! | glob | `alwaysApply:false`+`globs:` (csv) | `applyTo:` (csv) | `trigger:glob`+`globs:` | `paths:` (yaml array) |
//! | agent-requested | `alwaysApply:false`+`description` | degraded→always | `trigger:model_decision` | degraded→always |
//! | manual | `alwaysApply:false` (bare) | degraded (no applyTo) | `trigger:manual` | degraded |
//!
//! Harnesses without a structured rules directory (Gemini, OpenCode, Pi, Aider)
//! are reported Unsupported — their path is AGENTS.md, a separate instructions
//! kind (follow-up).

use crate::adapters::Harness;
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;
use serde::Deserialize;

pub struct RuleKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    #[default]
    Always,
    Glob,
    AgentRequested,
    Manual,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RuleConfig {
    #[serde(default)]
    activation: Activation,
    #[serde(default)]
    globs: Vec<String>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    body_file: Option<String>,
}

struct Rendered {
    path: String,
    contents: String,
    installability: Installability,
}

impl Kind for RuleKind {
    fn id(&self) -> KindId {
        KindId::Rule
    }

    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan {
        let cfg: RuleConfig = match crate::kind::kind_config(cap, "rule") {
            Ok(c) => c,
            Err(e) => return KindPlan::unsupported(e),
        };

        let body = match resolve_body(cap, &cfg) {
            Ok(b) => b,
            Err(e) => return KindPlan::unsupported(e),
        };

        let id = &cap.manifest.id;
        let desc = cap.manifest.description.as_str();
        let g = &cfg.globs;
        let a = cfg.activation;

        let r = match harness {
            Harness::Cursor => render_cursor(id, desc, a, g, &body),
            Harness::Copilot => render_copilot(id, a, g, &body),
            Harness::Windsurf => render_windsurf(id, desc, a, g, &body),
            Harness::Cline => render_paths(".clinerules", id, a, g, &body),
            Harness::Claude => render_paths(".claude/rules", id, a, g, &body),
            _ => {
                return KindPlan::unsupported(format!(
                    "{} has no structured rules directory (rules there go through AGENTS.md — a separate instructions kind)",
                    harness.id()
                ))
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

fn resolve_body(cap: &LoadedCapability, cfg: &RuleConfig) -> Result<String, String> {
    if let Some(bf) = &cfg.body_file {
        std::fs::read_to_string(cap.dir.join(bf))
            .map_err(|e| format!("cannot read body_file '{bf}': {e}"))
    } else if !cfg.body.trim().is_empty() {
        Ok(cfg.body.clone())
    } else {
        Ok(cap.manifest.description.clone())
    }
}

fn with_body(mut frontmatter: String, body: &str) -> String {
    frontmatter.push_str(body.trim_end());
    frontmatter.push('\n');
    frontmatter
}

/// Cursor `.cursor/rules/<id>.mdc` — supports all four activations natively.
fn render_cursor(id: &str, desc: &str, a: Activation, globs: &[String], body: &str) -> Rendered {
    let (always_apply, glob_csv, include_desc, inst) = match a {
        Activation::Always => (true, String::new(), true, Installability::Clean),
        Activation::Glob if globs.is_empty() => (
            true,
            String::new(),
            true,
            Installability::Degraded("glob activation with no globs; emitted as always".into()),
        ),
        Activation::Glob => (false, globs.join(","), true, Installability::Clean),
        Activation::AgentRequested => (false, String::new(), true, Installability::Clean),
        // manual = alwaysApply:false, no globs, no description
        Activation::Manual => (false, String::new(), false, Installability::Clean),
    };
    let mut fm = String::from("---\n");
    if include_desc && !desc.is_empty() {
        fm.push_str(&format!("description: {desc}\n"));
    }
    fm.push_str(&format!("globs: {glob_csv}\n"));
    fm.push_str(&format!("alwaysApply: {always_apply}\n"));
    fm.push_str("---\n\n");
    Rendered {
        path: format!(".cursor/rules/{id}.mdc"),
        contents: with_body(fm, body),
        installability: inst,
    }
}

/// Copilot `.github/instructions/<id>.instructions.md` — only `applyTo` globbing.
fn render_copilot(id: &str, a: Activation, globs: &[String], body: &str) -> Rendered {
    let (apply_to, inst): (Option<String>, Installability) = match a {
        Activation::Always => (Some("**".into()), Installability::Clean),
        Activation::Glob if globs.is_empty() => (
            Some("**".into()),
            Installability::Degraded("glob activation with no globs; applied to all files".into()),
        ),
        Activation::Glob => (Some(globs.join(",")), Installability::Clean),
        Activation::AgentRequested => (
            Some("**".into()),
            Installability::Degraded(
                "model-decision activation unsupported on Copilot; applied to all files".into(),
            ),
        ),
        Activation::Manual => (
            None,
            Installability::Degraded(
                "manual activation unsupported on Copilot; emitted without applyTo — attach in chat manually".into(),
            ),
        ),
    };
    let fm = match apply_to {
        Some(a) => format!("---\napplyTo: {a}\n---\n\n"),
        None => String::new(),
    };
    Rendered {
        path: format!(".github/instructions/{id}.instructions.md"),
        contents: with_body(fm, body),
        installability: inst,
    }
}

/// Windsurf `.windsurf/rules/<id>.md` — `trigger` covers all four activations.
fn render_windsurf(id: &str, desc: &str, a: Activation, globs: &[String], body: &str) -> Rendered {
    let mut fm = String::from("---\n");
    let inst = match a {
        Activation::Always => {
            fm.push_str("trigger: always_on\n");
            Installability::Clean
        }
        Activation::Glob if globs.is_empty() => {
            fm.push_str("trigger: always_on\n");
            Installability::Degraded("glob activation with no globs; emitted as always_on".into())
        }
        Activation::Glob => {
            fm.push_str("trigger: glob\n");
            fm.push_str(&format!("globs: {}\n", globs.join(",")));
            Installability::Clean
        }
        Activation::AgentRequested => {
            fm.push_str("trigger: model_decision\n");
            if !desc.is_empty() {
                fm.push_str(&format!("description: {desc}\n"));
            }
            Installability::Clean
        }
        Activation::Manual => {
            fm.push_str("trigger: manual\n");
            Installability::Clean
        }
    };
    fm.push_str("---\n\n");
    Rendered {
        path: format!(".windsurf/rules/{id}.md"),
        contents: with_body(fm, body),
        installability: inst,
    }
}

/// Cline `.clinerules/` and Claude `.claude/rules/` — a `paths:` YAML array for
/// glob activation; no frontmatter for always. Agent-requested/manual degrade.
fn render_paths(dir: &str, id: &str, a: Activation, globs: &[String], body: &str) -> Rendered {
    let (fm, inst) = match a {
        Activation::Always => (String::new(), Installability::Clean),
        Activation::Glob if globs.is_empty() => (
            String::new(),
            Installability::Degraded("glob activation with no globs; emitted as always".into()),
        ),
        Activation::Glob => (yaml_paths(globs), Installability::Clean),
        Activation::AgentRequested => (
            String::new(),
            Installability::Degraded(
                "model-decision activation unsupported here; applied always".into(),
            ),
        ),
        Activation::Manual => (
            String::new(),
            Installability::Degraded(
                "manual activation unsupported here; applied always (toggle in the harness UI)"
                    .into(),
            ),
        ),
    };
    Rendered {
        path: format!("{dir}/{id}.md"),
        contents: with_body(fm, body),
        installability: inst,
    }
}

fn yaml_paths(globs: &[String]) -> String {
    let mut s = String::from("---\npaths:\n");
    for g in globs {
        s.push_str(&format!("  - \"{g}\"\n"));
    }
    s.push_str("---\n\n");
    s
}
