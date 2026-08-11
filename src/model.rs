//! The canonical payload sent to a capability, and the canonical decision it
//! returns. These two shapes ARE the language-agnostic contract: a capability
//! is any executable that reads a `CanonicalPayload` as JSON on stdin and writes
//! a `Decision` as JSON on stdout. Nothing about it is Rust-, Python-, or
//! TypeScript-specific.

use crate::event::NormEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Full protocol id embedded in every payload. See `spec/hook-protocol.md`.
pub const PROTOCOL: &str = "open-harness/hook@1.1";
/// Short version token a capability manifest may declare via `protocol`.
pub const PROTOCOL_VERSION: &str = "hook@1.1";

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
    /// The harness session (or conversation) this event belongs to, where the
    /// native payload exposes one.
    ///
    /// Without it a capability that keeps per-session state has no key to group
    /// by: a transcript recorder cannot tie tool calls together, and a
    /// `post.session.end` capability cannot tell *which* session ended, so it
    /// cannot correlate with what it recorded at `pre.session.start`. The only
    /// alternative was reading `raw`, which writes the capability against one
    /// harness and forfeits the portability the contract exists to provide.
    ///
    /// `None` where the harness genuinely sends no identity — honest
    /// degradation, so a capability can decide whether it can proceed rather
    /// than being handed a fabricated key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The working directory the event happened in.
    ///
    /// Populated from the native payload where the harness supplies one, and
    /// otherwise from the dispatcher's own working directory — which is where
    /// the harness invoked the hook, so it is the project root for every
    /// shell-invoked harness. It stays `Option` because an in-process shim host
    /// (OpenCode, Pi) can be running somewhere else entirely, and a wrong root
    /// is worse than an absent one for anything keyed by project.
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

/// The major number of a `hook@MAJOR[.MINOR]` token, accepting either the short
/// form (`hook@1`) or the full id (`open-harness/hook@1.2`).
pub fn protocol_major(token: &str) -> Option<u32> {
    token.rsplit('@').next()?.split('.').next()?.parse().ok()
}

/// Version negotiation. A capability that declares no protocol is assumed
/// compatible; one that declares a different *major* is refused.
pub fn negotiate(capability_protocol: Option<&str>) -> Result<(), String> {
    let Some(theirs_token) = capability_protocol else {
        return Ok(());
    };
    let ours = protocol_major(PROTOCOL_VERSION).expect("our protocol version is well-formed");
    let theirs = protocol_major(theirs_token)
        .ok_or_else(|| format!("unparseable protocol version '{theirs_token}'"))?;
    if theirs == ours {
        Ok(())
    } else {
        Err(format!(
            "protocol mismatch: capability speaks '{theirs_token}' (major {theirs}), dispatcher is '{PROTOCOL_VERSION}' (major {ours})"
        ))
    }
}

/// Validate a Decision document against `hook@1`. Unknown fields are permitted
/// (forward-compat); wrong types and unknown verdicts are rejected.
pub fn validate_decision(v: &Value) -> Result<(), String> {
    let obj = v.as_object().ok_or("decision must be a JSON object")?;
    if let Some(d) = obj.get("decision") {
        let s = d.as_str().ok_or("`decision` must be a string")?;
        if !matches!(s, "allow" | "deny" | "modify") {
            return Err(format!(
                "`decision` must be one of allow|deny|modify, got '{s}'"
            ));
        }
    }
    if let Some(r) = obj.get("reason") {
        if !r.is_null() && !r.is_string() {
            return Err("`reason` must be a string".into());
        }
    }
    if let Some(c) = obj.get("context_append") {
        if !c.is_null() && !c.is_string() {
            return Err("`context_append` must be a string".into());
        }
    }
    Ok(())
}

/// Parse a capability's stdout into a validated Decision. Empty output means
/// allow. Validation runs before deserialization so errors are descriptive.
pub fn parse_decision(stdout: &str) -> Result<Decision, String> {
    // Tolerate a UTF-8 BOM (Windows PowerShell / `Out-File` prepend one) and any
    // CRLF whitespace before/after the JSON document.
    let trimmed = stdout.strip_prefix('\u{feff}').unwrap_or(stdout).trim();
    if trimmed.is_empty() {
        return Ok(Decision::allow());
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("decision was not valid JSON: {e} (got: {trimmed})"))?;
    validate_decision(&value)?;
    serde_json::from_value(value).map_err(|e| format!("decision did not match schema: {e}"))
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
