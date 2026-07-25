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
        }
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
        other => Box::new(Unimplemented(other)),
    }
}

/// Placeholder for kinds not yet built, so `check`/`emit` degrade gracefully
/// instead of panicking.
struct Unimplemented(KindId);
impl Kind for Unimplemented {
    fn id(&self) -> KindId {
        self.0
    }
    fn plan(&self, _cap: &LoadedCapability, _harness: Harness) -> KindPlan {
        KindPlan::unsupported(format!(
            "capability kind '{}' is not implemented yet",
            self.0.as_str()
        ))
    }
}
