//! The canonical payload sent to a capability, and the canonical decision it
//! returns. These two shapes ARE the language-agnostic contract: a capability
//! is any executable that reads a `CanonicalPayload` as JSON on stdin and writes
//! a `Decision` as JSON on stdout. Nothing about it is Rust-, Python-, or
//! TypeScript-specific.

use crate::event::NormEvent;
use serde::{Deserialize, Serialize};

pub const PROTOCOL: &str = "open-harness/hook@0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub input: serde_json::Value,
}

/// What the dispatcher hands to each capability. `raw` is the untouched native
/// payload — an escape hatch so a capability can reach fields the normalized
/// model doesn't (yet) cover, at the cost of harness portability.
#[derive(Debug, Clone, Serialize)]
pub struct CanonicalPayload {
    pub protocol: &'static str,
    pub harness: String,
    pub event: NormEvent,
    /// Whether a `deny` from the capability will actually be honored here.
    pub blocking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    #[default]
    Allow,
    Deny,
    Modify,
}

/// The canonical result. A capability that does nothing returns `{"decision":"allow"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    #[serde(default)]
    pub decision: Verdict,
    #[serde(default)]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_append: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_input: Option<serde_json::Value>,
}

impl Decision {
    pub fn allow() -> Self {
        Decision {
            decision: Verdict::Allow,
            reason: String::new(),
            context_append: None,
            modified_input: None,
        }
    }
}

/// Merge many capability decisions into one, with deny-wins semantics. This is
/// the composition mediation the harnesses themselves handle inconsistently:
/// because the dispatcher owns it, ordering and conflict resolution are
/// deterministic regardless of the host harness.
pub fn merge(decisions: &[Decision]) -> Decision {
    let mut out = Decision::allow();
    let mut reasons = Vec::new();
    let mut contexts = Vec::new();
    for d in decisions {
        match d.decision {
            Verdict::Deny => {
                out.decision = Verdict::Deny;
                if !d.reason.is_empty() {
                    reasons.push(d.reason.clone());
                }
            }
            Verdict::Modify => {
                // First modifier wins; a real impl would thread modified_input
                // through subsequent capabilities. Flagged in FEASIBILITY.md.
                if out.decision != Verdict::Deny {
                    out.decision = Verdict::Modify;
                    if out.modified_input.is_none() {
                        out.modified_input = d.modified_input.clone();
                    }
                    if !d.reason.is_empty() {
                        reasons.push(d.reason.clone());
                    }
                }
            }
            Verdict::Allow => {}
        }
        if let Some(c) = &d.context_append {
            contexts.push(c.clone());
        }
    }
    out.reason = reasons.join("; ");
    if !contexts.is_empty() {
        out.context_append = Some(contexts.join("\n"));
    }
    out
}

/// What a harness's native hook mechanism expects back. Different harnesses read
/// different channels (exit code vs stdout JSON vs stderr text), so all three
/// are carried and each adapter fills the ones its harness reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl NativeResponse {
    pub fn ok() -> Self {
        NativeResponse {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        }
    }
}
