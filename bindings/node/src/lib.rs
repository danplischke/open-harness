//! Node/TS bindings for open-harness (#14) — a napi-rs native addon over the
//! stable core API (`open_harness::api`).
//!
//! The Node/TS ecosystem hosts most harness plugin shims (OpenCode, Pi), so the
//! core is exposed as a **native** module rather than WASM: the core is
//! filesystem- and subprocess-oriented (`dispatch` spawns capabilities, `plan`
//! and `verify` read files), none of which works in `wasm32-unknown-unknown`.
//!
//! Complex results cross as JSON strings — the same string boundary the rest of
//! the API uses — so the addon needs no per-type binding glue.

use napi_derive::napi;
use open_harness::api;

fn err<E: std::fmt::Display>(e: E) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

/// Every harness id open-harness knows about.
#[napi]
pub fn harnesses() -> Vec<String> {
    api::harnesses()
}

/// The capability kinds currently implemented.
#[napi]
pub fn kinds() -> Vec<String> {
    api::kinds()
}

/// The frozen wire-protocol version, e.g. `"hook@1"`.
#[napi(js_name = "protocolVersion")]
pub fn protocol_version() -> String {
    api::protocol_version()
}

/// Plan a capability (its `capability.json` text) for one harness → JSON `Plan`.
#[napi]
pub fn plan(manifest_json: String, dir: String, harness: String) -> napi::Result<String> {
    let p = api::plan(&manifest_json, &dir, &harness).map_err(err)?;
    serde_json::to_string(&p).map_err(err)
}

/// Plan a capability across every harness → JSON `Plan[]`.
#[napi(js_name = "planAll")]
pub fn plan_all(manifest_json: String, dir: String) -> napi::Result<String> {
    let ps = api::plan_all(&manifest_json, &dir).map_err(err)?;
    serde_json::to_string(&ps).map_err(err)
}

/// Run the hook dispatcher (spawns capabilities over the stdio contract) →
/// JSON `DispatchResult`. This is what an in-process OpenCode/Pi plugin calls
/// to enforce hooks without shelling out to `oh-dispatch`.
#[napi]
pub fn dispatch(
    harness: String,
    event_id: String,
    native_stdin_json: String,
    capabilities_dir: String,
) -> napi::Result<String> {
    let r = api::dispatch(&harness, &event_id, &native_stdin_json, &capabilities_dir).map_err(err)?;
    serde_json::to_string(&r).map_err(err)
}

/// `dispatch` with explicit runtime limits (`0` = runtime default).
#[napi(js_name = "dispatchWithLimits")]
pub fn dispatch_with_limits(
    harness: String,
    event_id: String,
    native_stdin_json: String,
    capabilities_dir: String,
    timeout_ms: i64,
    max_output_bytes: i64,
) -> napi::Result<String> {
    let r = api::dispatch_with_limits(
        &harness,
        &event_id,
        &native_stdin_json,
        &capabilities_dir,
        timeout_ms.max(0) as u64,
        max_output_bytes.max(0) as u64,
    )
    .map_err(err)?;
    serde_json::to_string(&r).map_err(err)
}

/// Validate a decision document against `hook@1`. Throws on invalid.
#[napi(js_name = "validateDecision")]
pub fn validate_decision(decision_json: String) -> napi::Result<()> {
    api::validate_decision(&decision_json).map_err(err)
}

/// Verify a capability directory against an optional trust store (JSON string)
/// → JSON `VerifyResult`.
#[napi]
pub fn verify(dir: String, trust_json: Option<String>) -> napi::Result<String> {
    let r = api::verify(&dir, trust_json.as_deref()).map_err(err)?;
    serde_json::to_string(&r).map_err(err)
}
