//! `runtime.requires`: declaring what a capability's entrypoint needs on the
//! host, and finding out *before* a missing tool becomes a denied tool call.
//!
//! The failure this exists to prevent: a hook whose interpreter or helper tool
//! is absent exits non-zero with a language traceback, and on a blocking event
//! the default failure policy turns that into a **deny** — every `Bash` call the
//! agent makes is refused, and the reason shown to the user is a stack trace.
//! The verdict is still a deny (a guard that cannot run leaves you unguarded);
//! what changes is that the diagnosis names the tool, and that `check` and
//! `doctor` say so first.

use open_harness::adapters::Harness;
use open_harness::event::{NormEvent, Phase, ToolClass};
use open_harness::manifest::{LoadedCapability, Manifest, Runtime};
use open_harness::runtime::{self, RunError, RunLimits};
use serde_json::json;

fn cap_from(manifest: serde_json::Value) -> LoadedCapability {
    let manifest: Manifest = serde_json::from_value(manifest).expect("valid manifest");
    LoadedCapability {
        manifest,
        dir: std::path::PathBuf::from("."),
    }
}

/// A hook that would succeed if it ever ran, so any failure is the runtime gate.
fn hook_needing(id: &str, requires: &[&str]) -> LoadedCapability {
    cap_from(json!({
        "id": id,
        "runtime": { "requires": requires },
        "run": { "command": "python3", "args": ["-c", "print('{\"decision\":\"allow\"}')"] },
        "events": [{ "phase": "pre", "subject": "tool", "tool_class": "any" }],
    }))
}

/// An executable name no host will have.
const ABSENT: &str = "definitely-not-a-real-tool-9d3f";

/// The canonical payload for a blocking shell-tool event, built the way the
/// dispatcher builds it — through the harness adapter, not by hand.
fn blocking_payload() -> open_harness::model::CanonicalPayload {
    Harness::Claude.decode(
        &json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } }),
        &NormEvent::tool(Phase::Pre, ToolClass::Shell),
    )
}

// ---- the manifest block ----------------------------------------------------

#[test]
fn requires_defaults_to_empty_so_existing_manifests_are_unaffected() {
    let cap = cap_from(json!({ "id": "x", "kind": "skill" }));
    assert!(cap.manifest.runtime.is_empty());
    assert!(runtime::missing_requirements(&cap.manifest.runtime).is_none());
}

#[test]
fn a_misspelled_runtime_key_is_a_loud_error() {
    // `require:` silently checking nothing is exactly the failure mode this
    // block exists to remove, so the block refuses unknown fields.
    let err = serde_json::from_value::<Manifest>(json!({
        "id": "x", "runtime": { "require": ["uv"] }
    }))
    .unwrap_err()
    .to_string();
    assert!(err.contains("require"), "names the bad key: {err}");
}

#[test]
fn a_single_file_capability_can_declare_a_runtime() {
    // Document kinds are authored as frontmatter, so `runtime` has to be lifted
    // into the manifest header rather than landing in the kind's config.
    let dir = std::env::temp_dir().join(format!("oh-runtime-doc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("gen")).unwrap();
    std::fs::write(
        dir.join("gen/SKILL.md"),
        "---\ndescription: d\nruntime:\n  requires: [uv]\n---\n\nbody\n",
    )
    .unwrap();

    let caps = open_harness::manifest::discover(&dir).unwrap();
    assert_eq!(caps[0].manifest.runtime.requires, vec!["uv".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the probe -------------------------------------------------------------

#[test]
fn a_satisfied_requirement_reports_nothing_missing() {
    // `python3` is a CI-installed prerequisite of this suite, so it is present
    // by construction wherever these tests run.
    let r = Runtime {
        requires: vec!["python3".to_string()],
    };
    assert!(runtime::missing_requirements(&r).is_none());
}

#[test]
fn an_absent_requirement_is_named() {
    let r = Runtime {
        requires: vec!["python3".to_string(), ABSENT.to_string()],
    };
    let gap = runtime::missing_requirements(&r).expect("should report a gap");
    assert!(gap.contains(ABSENT), "names what is missing: {gap}");
    assert!(
        !gap.contains("python3"),
        "and does not name what is present: {gap}"
    );
}

#[test]
fn find_executable_agrees_with_the_probe() {
    // `doctor`, `check` and the dispatcher must ask the same question, or they
    // can disagree about whether a capability will run.
    assert!(runtime::find_executable("python3").is_some());
    assert!(runtime::find_executable(ABSENT).is_none());
}

// ---- install hints ---------------------------------------------------------

#[test]
fn known_tools_carry_an_install_hint() {
    for tool in ["uv", "node", "npm", "python3", "deno", "bun", "ruby", "git"] {
        let hint = runtime::install_hint(tool)
            .unwrap_or_else(|| panic!("{tool} should have an install hint"));
        assert!(!hint.trim().is_empty(), "{tool}'s hint is blank");
    }
}

#[test]
fn an_unknown_tool_gets_no_hint_rather_than_a_wrong_one() {
    assert!(runtime::install_hint(ABSENT).is_none());
    assert!(runtime::install_hint("some-in-house-cli").is_none());
}

#[test]
fn a_missing_known_tool_is_reported_with_its_hint() {
    let r = Runtime {
        requires: vec!["deno".to_string()],
    };
    // Only meaningful when deno is genuinely absent, which it is on CI.
    if runtime::find_executable("deno").is_none() {
        let gap = runtime::missing_requirements(&r).unwrap();
        assert!(gap.contains("install:"), "carries the hint: {gap}");
    }
}

// ---- dispatch --------------------------------------------------------------

#[test]
fn a_missing_runtime_is_its_own_failure_class_not_a_spawn_or_exit_error() {
    // The point of the class: "your toolchain is broken" and "the guard decided
    // to deny" must not look the same to whoever reads the error.
    let cap = hook_needing("guard", &[ABSENT]);
    let payload = blocking_payload();
    let err = runtime::run_capability(&cap, &payload, &RunLimits::default())
        .expect_err("should refuse to run");

    assert_eq!(err.class(), "runtime-missing");
    assert!(matches!(err, RunError::RuntimeMissing(_)));
    assert!(
        err.to_string().contains(ABSENT),
        "the message names the tool: {err}"
    );
}

#[test]
fn a_satisfied_runtime_does_not_block_the_capability() {
    let cap = hook_needing("guard", &["python3"]);
    let payload = blocking_payload();
    let decision = runtime::run_capability(&cap, &payload, &RunLimits::default())
        .expect("a satisfied requirement must not get in the way");
    assert_eq!(decision.decision, open_harness::model::Verdict::Allow);
}

#[test]
fn a_missing_runtime_still_fails_closed_on_a_blocking_event() {
    // Deliberately unchanged: a guard that cannot run leaves you unguarded, so
    // the verdict stays a deny. Only the diagnosis improves.
    let caps = vec![hook_needing("guard", &[ABSENT])];
    let out = open_harness::dispatch(
        Harness::Claude,
        &NormEvent::tool(Phase::Pre, ToolClass::Shell),
        &caps,
        &json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } }),
    );
    assert_eq!(out.response.exit_code, 2, "blocking event still denies");
    assert!(
        out.errored.iter().any(|e| e.contains("runtime-missing")),
        "and the reason is classified, not a traceback: {:?}",
        out.errored
    );
}
