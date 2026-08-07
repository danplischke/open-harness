//! The commands kind — normalize slash-commands across harnesses.
//!
//! Every harness turns a markdown (or TOML) file into a `/name` command, but the
//! format, location, and argument syntax diverge. This kind normalizes a command
//! (a prompt body using `{{args}}` / `{{argN}}` placeholders + optional hint /
//! model / agent) and renders each harness's native file.
//!
//! | harness | file | args |
//! |---|---|---|
//! | Claude | `.claude/commands/<id>.md` (YAML front) | `$ARGUMENTS` / `$N` |
//! | Codex | `~/.codex/prompts/<id>.md` (global) | `$ARGUMENTS` / `$N` |
//! | OpenCode | `.opencode/commands/<id>.md` | `$ARGUMENTS` / `$N` |
//! | Cursor | `.cursor/commands/<id>.md` | `$ARGUMENTS` / `$N` |
//! | Copilot | `.github/prompts/<id>.prompt.md` | `$ARGUMENTS` / `$N` |
//! | Windsurf | `.windsurf/workflows/<id>.md` | `$ARGUMENTS` / `$N` |
//! | Cline | `.clinerules/workflows/<id>.md` | `$ARGUMENTS` / `$N` |
//! | Gemini | `.gemini/commands/<id>.toml` (**TOML**) | `{{args}}` (no positional) |
//! | Pi | `.pi/prompts/<id>.md` | `{{args}}` (named vars) |
//!
//! Gemini has no positional arguments, so a body using `{{argN}}` there is
//! emitted **Degraded** with a note. Aider has no custom commands (Unsupported).

use crate::adapters::Harness;
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;
use serde::Deserialize;

pub struct CommandKind;

#[derive(Debug, Clone, Default, Deserialize)]
struct CommandConfig {
    #[serde(default)]
    argument_hint: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    agent: Option<String>,
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

impl Kind for CommandKind {
    fn id(&self) -> KindId {
        KindId::Command
    }

    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan {
        let cfg: CommandConfig = cap
            .manifest
            .config
            .get("command")
            .cloned()
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();

        let body = match resolve_body(cap, &cfg) {
            Ok(b) => b,
            Err(e) => return KindPlan::unsupported(e),
        };

        let id = cap.manifest.id.as_str();
        let desc = cap.manifest.description.as_str();
        let hint = cfg.argument_hint.as_deref().unwrap_or("");
        let model = cfg.model.as_deref().unwrap_or("");
        let agent = cfg.agent.as_deref().unwrap_or("");
        let dollar = to_dollar(&body);

        let r = match harness {
            Harness::Claude => Rendered {
                path: format!(".claude/commands/{id}.md"),
                contents: md(
                    fm(&[
                        ("description", desc),
                        ("argument-hint", hint),
                        ("model", model),
                    ]),
                    &dollar,
                ),
                installability: Installability::Clean,
            },
            Harness::Codex => Rendered {
                path: format!("~/.codex/prompts/{id}.md"),
                contents: md(
                    fm(&[("description", desc), ("argument-hint", hint)]),
                    &dollar,
                ),
                installability: Installability::Degraded(
                    "Codex custom prompts are global (~/.codex/prompts), not per-project".into(),
                ),
            },
            Harness::OpenCode => Rendered {
                path: format!(".opencode/commands/{id}.md"),
                contents: md(
                    fm(&[("description", desc), ("agent", agent), ("model", model)]),
                    &dollar,
                ),
                installability: Installability::Clean,
            },
            Harness::Cursor => Rendered {
                path: format!(".cursor/commands/{id}.md"),
                contents: md(fm(&[("description", desc)]), &dollar),
                installability: Installability::Clean,
            },
            Harness::Copilot => Rendered {
                path: format!(".github/prompts/{id}.prompt.md"),
                contents: md(
                    fm(&[("description", desc), ("model", model), ("agent", agent)]),
                    &dollar,
                ),
                installability: Installability::Clean,
            },
            Harness::Windsurf => Rendered {
                path: format!(".windsurf/workflows/{id}.md"),
                contents: md(fm(&[("description", desc)]), &dollar),
                installability: Installability::Clean,
            },
            Harness::Cline => Rendered {
                path: format!(".clinerules/workflows/{id}.md"),
                contents: md(fm(&[("description", desc)]), &dollar),
                installability: Installability::Clean,
            },
            Harness::Gemini => {
                // Gemini is TOML and has no positional args (only {{args}}).
                let inst = if has_positional(&body) {
                    Installability::Degraded(
                        "Gemini has no positional args; {{argN}} won't be substituted — use {{args}}"
                            .into(),
                    )
                } else {
                    Installability::Clean
                };
                Rendered {
                    path: format!(".gemini/commands/{id}.toml"),
                    contents: gemini_toml(desc, &body),
                    installability: inst,
                }
            }
            Harness::Pi => Rendered {
                // Pi prompt templates use named {{vars}}; keep the brace body.
                path: format!(".pi/prompts/{id}.md"),
                contents: md(String::new(), &body),
                installability: Installability::Clean,
            },
            // Antigravity commands are workflows: `.agents/workflows/<id>.md`.
            Harness::Antigravity => Rendered {
                path: format!(".agents/workflows/{id}.md"),
                contents: md(fm(&[("description", desc)]), &dollar),
                installability: Installability::Clean,
            },
            Harness::Aider => {
                return KindPlan::unsupported("Aider has no custom commands (built-ins only)");
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

fn resolve_body(cap: &LoadedCapability, cfg: &CommandConfig) -> Result<String, String> {
    if let Some(bf) = &cfg.body_file {
        std::fs::read_to_string(cap.dir.join(bf))
            .map_err(|e| format!("cannot read body_file '{bf}': {e}"))
    } else if !cfg.body.trim().is_empty() {
        Ok(cfg.body.clone())
    } else {
        Ok(cap.manifest.description.clone())
    }
}

/// Canonical `{{args}}` / `{{argN}}` → shell-style `$ARGUMENTS` / `$N`.
fn to_dollar(body: &str) -> String {
    let mut out = body.replace("{{args}}", "$ARGUMENTS");
    for n in 1..=9 {
        out = out.replace(&format!("{{{{arg{n}}}}}"), &format!("${n}"));
    }
    out
}

fn has_positional(body: &str) -> bool {
    (1..=9).any(|n| body.contains(&format!("{{{{arg{n}}}}}")))
}

/// Emit a YAML frontmatter block, skipping empty values.
fn fm(pairs: &[(&str, &str)]) -> String {
    let mut s = String::from("---\n");
    for (k, v) in pairs {
        if !v.is_empty() {
            s.push_str(&format!("{k}: {v}\n"));
        }
    }
    s.push_str("---\n\n");
    s
}

fn md(front: String, body: &str) -> String {
    let mut s = front;
    s.push_str(body.trim_end());
    s.push('\n');
    s
}

fn gemini_toml(desc: &str, body_brace: &str) -> String {
    format!(
        "description = {}\nprompt = \"\"\"\n{}\n\"\"\"\n",
        toml_str(desc),
        body_brace.trim_end()
    )
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
