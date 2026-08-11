//! The native kind — config that only **one** harness can host, carried
//! verbatim and refused everywhere else.
//!
//! Every other kind exists because a concept is shared and only its *spelling*
//! differs, so open-harness can compile one definition into eleven dialects.
//! This kind is the opposite: it carries something with no cross-harness
//! meaning at all.
//!
//! It exists because importing a vendor's plugin (see [`crate::plugin`]) turns
//! up exactly that. A Claude Code plugin's `hooks/hooks.json` is not a
//! `hook@1` capability — it is opaque shell wired to Claude's native hook
//! schema, hunting Claude's own plugin cache at runtime. Nothing can make it
//! portable. The honest options were to drop it silently, to pretend it
//! compiles, or to carry it as what it is and say so on the other ten
//! harnesses. This is the third.
//!
//! ```yaml
//! kind: native
//! native:
//!   harness: claude-code
//!   reason: Claude Code hook bundle — opaque shell against Claude's own schema
//!   artifacts:
//!     - path: .claude/hooks/claude-mem.json
//!       body: "{ … }"
//! ```
//!
//! On `harness` the artifacts are written as-is. On every other harness the
//! plan is `Unsupported`, carrying `reason` — so `oh check` and `oh matrix`
//! show a blocked cell with an explanation instead of a silent absence.

use crate::adapters::Harness;
use crate::kind::{Artifact, Installability, Kind, KindId, KindPlan};
use crate::manifest::LoadedCapability;
use serde::Deserialize;

pub struct NativeKind;

/// `deny_unknown_fields` so a misspelled key is a loud error rather than a
/// silently defaulted one. A mistyped `activation`/`argument_hint`/`verdict`
/// does not just lose a setting — it substitutes the *default*, which for a
/// glob-scoped rule means applying it to every file instead of a few.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeConfig {
    /// The one harness that can host this. Anything else is Unsupported.
    #[serde(default)]
    harness: String,
    /// Why it is not portable, shown on every other harness. A default is
    /// provided, but an importer should say something specific.
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    artifacts: Vec<NativeArtifact>,
    /// A native registration snippet to wire in by hand, for config that has no
    /// file of its own. Reported as a manual step by `sync`, never written.
    #[serde(default)]
    registration: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct NativeArtifact {
    path: String,
    #[serde(default)]
    body: String,
    /// Read the body from a file in the capability directory instead — how an
    /// importer carries a bundle without inlining it into the manifest.
    #[serde(default)]
    body_file: Option<String>,
}

impl Kind for NativeKind {
    fn id(&self) -> KindId {
        KindId::Native
    }

    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan {
        let cfg: NativeConfig = match crate::kind::kind_config(cap, "native") {
            Ok(c) => c,
            Err(e) => return KindPlan::unsupported(e),
        };

        // An unnamed or unknown target harness is a broken capability, not a
        // capability that happens to be unsupported here — say which.
        let Some(target) = Harness::from_id(&cfg.harness) else {
            return KindPlan::unsupported(if cfg.harness.is_empty() {
                "native capability declares no `harness`".to_string()
            } else {
                format!("native capability names unknown harness '{}'", cfg.harness)
            });
        };

        if target != harness {
            return KindPlan::unsupported(cfg.reason.clone().unwrap_or_else(|| {
                format!(
                    "native to {} — this config has no equivalent on {}",
                    target.id(),
                    harness.id()
                )
            }));
        }

        let mut artifacts = Vec::new();
        for a in &cfg.artifacts {
            let body = match &a.body_file {
                Some(f) => match std::fs::read_to_string(cap.dir.join(f)) {
                    Ok(s) => s,
                    Err(e) => {
                        return KindPlan::unsupported(format!("cannot read body_file '{f}': {e}"))
                    }
                },
                None => a.body.clone(),
            };
            artifacts.push(Artifact::File {
                path: a.path.clone(),
                contents: body,
            });
        }
        if let Some(text) = &cfg.registration {
            artifacts.push(Artifact::Registration { text: text.clone() });
        }

        if artifacts.is_empty() {
            return KindPlan::unsupported("native capability declares no artifacts".to_string());
        }

        KindPlan {
            installability: Installability::Degraded(format!(
                "native to {} — carried verbatim, not portable to any other harness",
                target.id()
            )),
            artifacts,
            notes: vec![format!(
                "this capability was imported in {}'s own format; open-harness \
                 does not interpret it",
                target.id()
            )],
        }
    }
}
