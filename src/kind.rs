//! Capability *kind* abstraction.
//!
//! The spike treated "capability" and "hook" as synonyms. To add skills, rules,
//! commands, tools, and permissions without special-casing each in the core, a
//! capability declares a `kind`, and each kind knows how to answer one question
//! for a target harness: **can this be installed, and what artifacts realize it?**
//!
//! Hooks additionally have a *runtime* (the dispatcher in `dispatch.rs`); other
//! kinds are purely generative (they emit files). Adding a new kind means adding
//! a `Kind` impl and one arm in [`kind_impl`] — the dispatcher core is untouched.

use crate::adapters::Harness;
use crate::manifest::LoadedCapability;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum KindId {
    #[default]
    Hook,
    Skill,
    Rule,
    Command,
    Tool,
    Permission,
    Agent,
    Instructions,
}

impl KindId {
    pub fn as_str(&self) -> &'static str {
        match self {
            KindId::Hook => "hook",
            KindId::Skill => "skill",
            KindId::Rule => "rule",
            KindId::Command => "command",
            KindId::Tool => "tool",
            KindId::Permission => "permission",
            KindId::Agent => "agent",
            KindId::Instructions => "instructions",
        }
    }

    pub fn parse(s: &str) -> Option<KindId> {
        Some(match s {
            "hook" => KindId::Hook,
            "skill" => KindId::Skill,
            "rule" => KindId::Rule,
            "command" => KindId::Command,
            "tool" => KindId::Tool,
            "permission" => KindId::Permission,
            "agent" => KindId::Agent,
            "instructions" => KindId::Instructions,
            _ => return None,
        })
    }
}

/// Whether a capability of a given kind can be installed on a given harness.
#[derive(Debug, Clone)]
pub enum Installability {
    /// One-to-one, no loss.
    Clean,
    /// Installable but with caveats (fan-out to several native events, an
    /// optional binding skipped, a best-effort permission mapping, ...).
    Degraded(String),
    /// No native target. Carries the reason for the matrix / install warning.
    Unsupported(String),
}

/// A concrete thing to install to realize a capability on a harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifact {
    /// A file to write into the project (skills, rules, commands).
    File { path: String, contents: String },
    /// A native config snippet to register (hooks).
    Registration { text: String },
}

#[derive(Debug, Clone)]
pub struct KindPlan {
    pub installability: Installability,
    pub artifacts: Vec<Artifact>,
    pub notes: Vec<String>,
}

impl KindPlan {
    pub fn unsupported(reason: impl Into<String>) -> Self {
        KindPlan {
            installability: Installability::Unsupported(reason.into()),
            artifacts: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// Parse a kind's config block (`config["<key>"]`) into its typed shape.
///
/// Distinguishes an **absent** block (→ the default, legitimately empty) from a
/// block that is **present but malformed** (→ a hard error carrying the reason).
/// The malformed case must never be silently defaulted: doing so voids the
/// capability's real content — body *and* every config field — and emits a
/// plausible-but-wrong artifact with exit 0, the exact silent honest-degradation
/// failure this project forbids. Callers surface the `Err` as an `Unsupported`
/// plan so `oh check` / `oh sync` report the capability as blocked, with the
/// serde error, instead of shipping garbage.
pub fn kind_config<T>(cap: &LoadedCapability, key: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned + Default,
{
    match cap.manifest.config.get(key) {
        Some(v) => {
            serde_json::from_value(v.clone()).map_err(|e| format!("invalid `{key}` config: {e}"))
        }
        None => Ok(T::default()),
    }
}

/// The one operation every capability kind must implement.
pub trait Kind {
    fn id(&self) -> KindId;
    /// For a capability of this kind on a target harness: installability + the
    /// artifacts that realize it.
    fn plan(&self, cap: &LoadedCapability, harness: Harness) -> KindPlan;
}

/// The single registration point mapping a `KindId` to its implementation.
/// Adding a kind adds an arm here and nothing in the dispatcher core.
pub fn kind_impl(id: KindId) -> Box<dyn Kind> {
    match id {
        KindId::Hook => Box::new(crate::kinds::hook::HookKind),
        KindId::Skill => Box::new(crate::kinds::skill::SkillKind),
        KindId::Rule => Box::new(crate::kinds::rule::RuleKind),
        KindId::Tool => Box::new(crate::kinds::tool::ToolKind),
        KindId::Command => Box::new(crate::kinds::command::CommandKind),
        KindId::Permission => Box::new(crate::kinds::permission::PermissionKind),
        KindId::Agent => Box::new(crate::kinds::agent::AgentKind),
        KindId::Instructions => Box::new(crate::kinds::instructions::InstructionsKind),
    }
}
