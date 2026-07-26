//! Stable embeddable API — the surface hosts (and language bindings) call.
//!
//! Design rules that keep it binding-friendly (uniffi / napi / WASM):
//!   - every type is a plain struct/enum of primitives, `String`, `Vec`, and
//!     `Option` — **no `serde_json::Value` crosses this boundary**;
//!   - anywhere arbitrary data is needed (a capability manifest, a harness's
//!     native hook payload, a decision) it is passed as a **JSON string** and
//!     parsed inside;
//!   - every fallible call returns `Result<_, ApiError>` rather than
//!     panicking / exiting (that is the CLI's job, not the library's).
//!
//! The internals in `dispatch`, `kind`, `kinds`, `manifest`, `model` are the
//! implementation; this module is the contract.

use crate::adapters::Harness;
use crate::event::NormEvent;
use crate::kind::{kind_impl, Artifact, Installability, KindId};
use crate::manifest::{LoadedCapability, Manifest};
use std::fmt;
use std::path::PathBuf;

/// Errors from the stable API. Kept as a small, flat enum so it maps to a
/// uniffi/napi error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    Manifest(String),
    Harness(String),
    Event(String),
    Json(String),
    Protocol(String),
    Io(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Manifest(m) => write!(f, "invalid manifest: {m}"),
            ApiError::Harness(m) => write!(f, "unknown harness: {m}"),
            ApiError::Event(m) => write!(f, "invalid event id: {m}"),
            ApiError::Json(m) => write!(f, "invalid JSON: {m}"),
            ApiError::Protocol(m) => write!(f, "protocol: {m}"),
            ApiError::Io(m) => write!(f, "io: {m}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// One thing to install to realize a capability on a harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanArtifact {
    /// `"file"` (path + contents) or `"registration"` (native config snippet in `contents`).
    pub kind: String,
    pub path: String,
    pub contents: String,
}

/// The result of planning a capability for one harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub harness: String,
    /// `"clean"` | `"degraded"` | `"unsupported"`.
    pub installability: String,
    /// Degradation / unsupported reason; empty when clean.
    pub detail: String,
    pub artifacts: Vec<PlanArtifact>,
    pub notes: Vec<String>,
}

/// The result of running the hook dispatcher for one native event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub ran: Vec<String>,
    pub skipped: Vec<String>,
    pub errored: Vec<String>,
}

/// The frozen wire-protocol version, e.g. `"hook@1"`.
pub fn protocol_version() -> String {
    crate::model::PROTOCOL_VERSION.to_string()
}

/// Every harness id open-harness knows about.
pub fn harnesses() -> Vec<String> {
    crate::adapters::ALL
        .iter()
        .map(|h| h.id().to_string())
        .collect()
}

/// The capability kinds currently implemented.
pub fn kinds() -> Vec<String> {
    [KindId::Hook, KindId::Skill, KindId::Rule, KindId::Tool]
        .iter()
        .map(|k| k.as_str().to_string())
        .collect()
}

/// Plan a capability (given as its `capability.json` text) for one harness.
/// `dir` is the directory used to resolve relative `body_file` paths.
pub fn plan(manifest_json: &str, dir: &str, harness: &str) -> Result<Plan, ApiError> {
    let cap = load(manifest_json, dir)?;
    let h = harness_of(harness)?;
    Ok(to_plan(h, kind_impl(cap.manifest.kind).plan(&cap, h)))
}

/// Plan a capability across every harness (the data behind `check`).
pub fn plan_all(manifest_json: &str, dir: &str) -> Result<Vec<Plan>, ApiError> {
    let cap = load(manifest_json, dir)?;
    let kind = kind_impl(cap.manifest.kind);
    Ok(crate::adapters::ALL
        .iter()
        .map(|&h| to_plan(h, kind.plan(&cap, h)))
        .collect())
}

/// Run the hook dispatcher: a harness's native hook payload (JSON string, or
/// empty for none) in, its native response out.
pub fn dispatch(
    harness: &str,
    event_id: &str,
    native_stdin_json: &str,
    capabilities_dir: &str,
) -> Result<DispatchResult, ApiError> {
    let h = harness_of(harness)?;
    let ev: NormEvent = event_id.parse().map_err(ApiError::Event)?;
    let native = if native_stdin_json.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(native_stdin_json).map_err(|e| ApiError::Json(e.to_string()))?
    };
    let caps =
        crate::manifest::discover(std::path::Path::new(capabilities_dir)).map_err(ApiError::Io)?;
    let out = crate::dispatch::dispatch(h, &ev, &caps, &native);
    Ok(DispatchResult {
        exit_code: out.response.exit_code,
        stdout: out.response.stdout,
        stderr: out.response.stderr,
        ran: out.ran,
        skipped: out.skipped,
        errored: out.errored,
    })
}

/// Validate a decision document against `hook@1`. Ok(()) or a descriptive error.
pub fn validate_decision(decision_json: &str) -> Result<(), ApiError> {
    crate::model::parse_decision(decision_json)
        .map(|_| ())
        .map_err(ApiError::Protocol)
}

/// Negotiate a capability's declared protocol version against the dispatcher's.
pub fn negotiate(capability_protocol: Option<String>) -> Result<(), ApiError> {
    crate::model::negotiate(capability_protocol.as_deref()).map_err(ApiError::Protocol)
}

// ---- internal helpers -----------------------------------------------------

fn load(manifest_json: &str, dir: &str) -> Result<LoadedCapability, ApiError> {
    let manifest: Manifest =
        serde_json::from_str(manifest_json).map_err(|e| ApiError::Manifest(e.to_string()))?;
    Ok(LoadedCapability {
        manifest,
        dir: PathBuf::from(dir),
    })
}

fn harness_of(id: &str) -> Result<Harness, ApiError> {
    Harness::from_id(id).ok_or_else(|| ApiError::Harness(id.to_string()))
}

fn to_plan(h: Harness, p: crate::kind::KindPlan) -> Plan {
    let (installability, detail) = match p.installability {
        Installability::Clean => ("clean".to_string(), String::new()),
        Installability::Degraded(d) => ("degraded".to_string(), d),
        Installability::Unsupported(r) => ("unsupported".to_string(), r),
    };
    let artifacts = p
        .artifacts
        .into_iter()
        .map(|a| match a {
            Artifact::File { path, contents } => PlanArtifact {
                kind: "file".to_string(),
                path,
                contents,
            },
            Artifact::Registration { text } => PlanArtifact {
                kind: "registration".to_string(),
                path: String::new(),
                contents: text,
            },
        })
        .collect();
    Plan {
        harness: h.id().to_string(),
        installability,
        detail,
        artifacts,
        notes: p.notes,
    }
}
