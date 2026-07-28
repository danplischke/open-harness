//! The dispatcher: the single native entrypoint each harness calls. It decodes
//! the native payload, runs every capability bound to the event (each as a
//! subprocess speaking canonical JSON over stdio — hence language-agnostic),
//! merges their decisions, and re-encodes for the harness.
//!
//! Execution is hardened by `runtime.rs` (timeout, output cap, error taxonomy);
//! this module owns fan-out, per-binding **failure policy** resolution, and the
//! deterministic merge. Independent capabilities run **concurrently** (each is
//! its own subprocess), but results are collected in manifest order so the merge
//! is deterministic regardless of completion order.

use crate::adapters::Harness;
use crate::event::NormEvent;
use crate::kind::KindId;
use crate::manifest::LoadedCapability;
use crate::model::{merge, Decision, NativeResponse, Verdict};
use crate::runtime::{run_capability, RunError, RunLimits};
use serde_json::Value;

pub struct DispatchOutcome {
    pub response: NativeResponse,
    pub ran: Vec<String>,
    pub skipped: Vec<String>,
    pub errored: Vec<String>,
}

/// Dispatch with the default runtime limits.
pub fn dispatch(
    harness: Harness,
    ev: &NormEvent,
    caps: &[LoadedCapability],
    native_stdin: &Value,
) -> DispatchOutcome {
    dispatch_with_limits(harness, ev, caps, native_stdin, &RunLimits::default())
}

/// Dispatch with explicit runtime limits (timeout / output cap).
pub fn dispatch_with_limits(
    harness: Harness,
    ev: &NormEvent,
    caps: &[LoadedCapability],
    native_stdin: &Value,
    limits: &RunLimits,
) -> DispatchOutcome {
    let payload = harness.decode(native_stdin, ev);

    // Fan out: run every bound hook capability concurrently, but keep the
    // results index-aligned with `caps` so the downstream merge is order-stable.
    let results: Vec<Option<Result<Decision, RunError>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = caps
            .iter()
            .map(|cap| {
                let runnable = cap.manifest.kind == KindId::Hook && cap.binds(ev);
                let payload = &payload;
                scope.spawn(move || {
                    if runnable {
                        Some(run_capability(cap, payload, limits))
                    } else {
                        None
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or(Some(Err(RunError::Io("capability thread panicked".into()))))
            })
            .collect()
    });

    let blocking = ev.blocking();
    let mut decisions = Vec::new();
    let mut ran = Vec::new();
    let mut skipped = Vec::new();
    let mut errored = Vec::new();

    for (cap, result) in caps.iter().zip(results) {
        let id = &cap.manifest.id;
        match result {
            None => skipped.push(id.clone()),
            Some(Ok(d)) => {
                decisions.push(d);
                ran.push(id.clone());
            }
            Some(Err(e)) => {
                let policy = cap.fail_policy(ev);
                let denies = policy.denies(blocking);
                errored.push(format!(
                    "{id}: [{}] {e} -> {} ({})",
                    e.class(),
                    if denies { "deny" } else { "allow" },
                    policy.as_str()
                ));
                if denies {
                    decisions.push(Decision {
                        decision: Verdict::Deny,
                        reason: format!("capability `{id}` failed ({}): {e}", e.class()),
                        context_append: None,
                        modified_input: None,
                    });
                }
            }
        }
    }

    let merged = merge(&decisions);
    let response = harness.encode(&merged, ev);
    DispatchOutcome {
        response,
        ran,
        skipped,
        errored,
    }
}
