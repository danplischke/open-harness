//! The dispatcher: the single native entrypoint each harness calls. It decodes
//! the native payload, runs every capability bound to the event (each as a
//! subprocess speaking canonical JSON over stdio — hence language-agnostic),
//! merges their decisions, and re-encodes for the harness.

use crate::adapters::Harness;
use crate::event::NormEvent;
use crate::manifest::LoadedCapability;
use crate::model::{merge, CanonicalPayload, Decision, NativeResponse, Verdict};
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

pub struct DispatchOutcome {
    pub response: NativeResponse,
    pub ran: Vec<String>,
    pub skipped: Vec<String>,
    pub errored: Vec<String>,
}

pub fn dispatch(
    harness: Harness,
    ev: &NormEvent,
    caps: &[LoadedCapability],
    native_stdin: &Value,
) -> DispatchOutcome {
    let payload = harness.decode(native_stdin, ev);
    let mut decisions = Vec::new();
    let mut ran = Vec::new();
    let mut skipped = Vec::new();
    let mut errored = Vec::new();

    for cap in caps {
        if !cap.binds(ev) {
            skipped.push(cap.manifest.id.clone());
            continue;
        }
        match run_capability(cap, &payload) {
            Ok(d) => {
                decisions.push(d);
                ran.push(cap.manifest.id.clone());
            }
            Err(e) => {
                errored.push(format!("{}: {e}", cap.manifest.id));
                // Fail-closed on blocking events: a guard that crashes must not
                // silently allow the action. On non-blocking events, degrade to
                // allow (there is nothing to veto).
                if ev.blocking() {
                    decisions.push(Decision {
                        decision: Verdict::Deny,
                        reason: format!("capability `{}` failed: {e}", cap.manifest.id),
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

fn run_capability(cap: &LoadedCapability, payload: &CanonicalPayload) -> Result<Decision, String> {
    // Refuse an incompatible protocol before spawning anything.
    crate::model::negotiate(cap.manifest.protocol.as_deref())?;

    let input = serde_json::to_string(payload).map_err(|e| e.to_string())?;
    let mut child = Command::new(&cap.manifest.run.command)
        .args(&cap.manifest.run.args)
        .current_dir(&cap.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", cap.manifest.run.command))?;

    child
        .stdin
        .take()
        .ok_or("no stdin handle")?
        .write_all(input.as_bytes())
        .map_err(|e| format!("write stdin: {e}"))?;

    let out = child.wait_with_output().map_err(|e| format!("wait: {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Validates against hook@1 before returning; malformed output fails closed
    // on blocking events via the caller's error handling.
    crate::model::parse_decision(&stdout)
}
