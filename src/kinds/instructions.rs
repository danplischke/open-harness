//! The instructions kind — one canonical project instruction file → each
//! harness's always-on memory file (gap 4).
//!
//! This is the always-on "read this first" project brief, distinct from the
//! `rule` kind (which is *conditional*, glob- or model-scoped guidance). Most
//! agents now read the cross-tool [`AGENTS.md`](https://agents.md) standard; a
//! few keep a vendor-specific name:
//!
//! | harness | file |
//! |---|---|
//! | Claude | `CLAUDE.md` |
//! | Copilot | `.github/copilot-instructions.md` |
//! | Gemini / Antigravity | `GEMINI.md` |
//! | Codex / OpenCode / Cursor / Windsurf / Cline / Aider | `AGENTS.md` |
//! | Pi | Unsupported (minimal core, no committed instruction file) |
//!
//! Several harnesses share `AGENTS.md`; because the body is identical the sync
//! layer converges them to a single idempotent file. The file is plain Markdown
//! (no frontmatter) — an instruction brief, not a scoped rule.

use crate::adapters::Harness;
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;
use serde::Deserialize;

pub struct InstructionsKind;

/// `deny_unknown_fields` so a misspelled key is a loud error rather than a
/// silently defaulted one. A mistyped `activation`/`argument_hint`/`verdict`
/// does not just lose a setting — it substitutes the *default*, which for a
/// glob-scoped rule means applying it to every file instead of a few.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstructionsConfig {
    #[serde(default)]
    body: String,
    #[serde(default)]
    body_file: Option<String>,
}

/// The always-on instruction file each harness reads. `None` = no committed
/// instruction file on this harness.
fn instructions_path(h: Harness) -> Option<&'static str> {
    match h {
        Harness::Claude => Some("CLAUDE.md"),
        Harness::Copilot => Some(".github/copilot-instructions.md"),
        // GEMINI.md wins over AGENTS.md on Google's tools when both are present.
        Harness::Gemini | Harness::Antigravity => Some("GEMINI.md"),
        // The AGENTS.md cross-tool standard.
        Harness::Codex
        | Harness::OpenCode
        | Harness::Cursor
        | Harness::Windsurf
        | Harness::Cline
        | Harness::Aider => Some("AGENTS.md"),
        Harness::Pi => None,
    }
}

impl Kind for InstructionsKind {
    fn id(&self) -> KindId {
        KindId::Instructions
    }

    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan {
        let Some(path) = instructions_path(harness) else {
            return KindPlan::unsupported(format!(
                "{} has no committed always-on instruction file",
                harness.id()
            ));
        };

        let cfg: InstructionsConfig = match crate::kind::kind_config(cap, "instructions") {
            Ok(c) => c,
            Err(e) => return KindPlan::unsupported(e),
        };

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

        let mut contents = body.trim_end().to_string();
        contents.push('\n');

        KindPlan {
            installability: Installability::Clean,
            artifacts: vec![Artifact::File {
                path: path.to_string(),
                contents,
            }],
            notes: Vec::new(),
        }
    }
}
