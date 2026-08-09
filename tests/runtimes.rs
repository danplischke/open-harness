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
        provision: None,
    };
    assert!(runtime::missing_requirements(&r).is_none());
}

#[test]
fn an_absent_requirement_is_named() {
    let r = Runtime {
        requires: vec!["python3".to_string(), ABSENT.to_string()],
        provision: None,
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
        provision: None,
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

// ---- cross-platform requirement resolution ---------------------------------

#[test]
fn a_requirement_resolves_through_the_same_interpreter_remap_the_dispatcher_uses() {
    // On Windows the binary is `python.exe`, and `resolve_program` maps
    // `python3` → `python` before spawning. A literal PATH probe for `python3`
    // would report a missing runtime for a capability that runs perfectly —
    // `check` asking a different question than the dispatcher is the exact
    // inconsistency this probe exists to remove.
    assert!(
        runtime::resolve_requirement("python3").is_some(),
        "python3 resolves on every platform this suite runs on"
    );
    assert!(runtime::resolve_requirement("python").is_some());
    // A non-interpreter name still falls through to a plain lookup.
    assert!(runtime::resolve_requirement(ABSENT).is_none());
}

#[test]
fn a_python_capability_is_not_reported_missing_on_any_platform() {
    let r = Runtime {
        requires: vec!["python3".to_string()],
        provision: None,
    };
    assert!(
        runtime::missing_requirements(&r).is_none(),
        "a python3 requirement must be satisfiable wherever the dispatcher can run it"
    );
}

// ---- provisioning ----------------------------------------------------------

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-provision-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir temp");
    p
}

/// A capability whose provisioning command is a python one-liner, so the test
/// needs no package manager and runs identically on every platform.
fn provisioned_cap(dir: &std::path::Path, script: &str, requires: &[&str]) -> LoadedCapability {
    let manifest: Manifest = serde_json::from_value(json!({
        "id": "prov",
        "kind": "hook",
        "runtime": {
            "requires": requires,
            "provision": { "command": "python3", "args": ["-c", script] },
        },
        "run": { "command": "python3", "args": ["-c", "print('{}')"] },
        "events": [{ "phase": "pre", "subject": "tool", "tool_class": "any" }],
    }))
    .expect("valid manifest");
    LoadedCapability {
        manifest,
        dir: dir.to_path_buf(),
    }
}

#[test]
fn a_capability_without_provision_returns_nothing_to_run() {
    let cap = hook_needing("guard", &["python3"]);
    assert!(runtime::provision(&cap).is_none());
}

#[test]
fn provisioning_runs_in_the_capabilitys_own_directory() {
    // Otherwise `uv sync --script guard.py` or `npm ci` would resolve against
    // wherever `oh` was invoked from.
    let dir = tmp_dir("cwd");
    let cap = provisioned_cap(&dir, "open('made-here.txt', 'w').write('x')", &[]);
    let out = runtime::provision(&cap).expect("has a provision command");
    assert!(out.success, "the command ran: {}", out.output);
    assert!(
        dir.join("made-here.txt").is_file(),
        "the command ran in the capability's directory"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failing_provision_command_is_reported_with_its_output() {
    let dir = tmp_dir("fail");
    let cap = provisioned_cap(
        &dir,
        "import sys; sys.stderr.write('could not reach the index'); sys.exit(1)",
        &[],
    );
    let out = runtime::provision(&cap).unwrap();
    assert!(!out.success);
    assert!(
        out.output.contains("could not reach the index"),
        "the failure explains itself: {}",
        out.output
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_provisioner_that_exits_zero_without_satisfying_requires_is_not_a_success() {
    // The honest distinction: the command ran, the dependency is still absent.
    // Reporting that as ✓ would be the same silent lie this project refuses
    // everywhere else.
    let dir = tmp_dir("hollow");
    let cap = provisioned_cap(&dir, "pass", &[ABSENT]);
    let out = runtime::provision(&cap).unwrap();
    assert!(out.success, "the command itself exited 0");
    assert!(
        !out.requirements_met,
        "but the declared requirement is still unsatisfied"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_provisioner_binary_is_reported_not_panicked() {
    let dir = tmp_dir("no-binary");
    let manifest: Manifest = serde_json::from_value(json!({
        "id": "prov", "kind": "hook",
        "runtime": { "provision": { "command": ABSENT, "args": [] } },
    }))
    .unwrap();
    let cap = LoadedCapability {
        manifest,
        dir: dir.clone(),
    };
    let out = runtime::provision(&cap).unwrap();
    assert!(!out.success);
    assert!(out.output.contains(ABSENT), "{}", out.output);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn provision_round_trips_through_yaml() {
    let text = "requires: [uv]\nprovision:\n  command: uv\n  args: [sync, --script, guard.py]\n";
    let r: Runtime =
        open_harness::config::from_str(text, open_harness::config::Format::Yaml).unwrap();
    let spec = r.provision.as_ref().expect("parsed");
    assert_eq!(spec.display(), "uv sync --script guard.py");

    let back: Runtime = open_harness::config::from_str(
        &open_harness::config::to_yaml(&r).unwrap(),
        open_harness::config::Format::Yaml,
    )
    .unwrap();
    assert_eq!(back, r);
}

#[test]
fn a_misspelled_provision_key_is_a_loud_error() {
    let err = serde_json::from_value::<Runtime>(json!({
        "provision": { "cmd": "uv" }
    }))
    .unwrap_err()
    .to_string();
    assert!(err.contains("cmd"), "names the bad key: {err}");
}

// ---- the `oh install --runtimes` gates -------------------------------------
//
// This is the only place open-harness executes something a capability author
// chose but the wire protocol did not define, so the gates in front of it are
// the behaviour under test.

fn oh(args: &[&str], cwd: &std::path::Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run oh")
}

/// A capability tree whose provisioning command leaves a marker file behind, so
/// "did it actually run" is observable.
fn provisioning_tree(tag: &str) -> std::path::PathBuf {
    let wd = tmp_dir(tag);
    let cap = wd.join("caps/needs-deps");
    std::fs::create_dir_all(&cap).unwrap();
    std::fs::write(
        cap.join("capability.yaml"),
        "id: needs-deps\nkind: hook\nruntime:\n  requires: [python3]\n  provision:\n    \
         command: python3\n    args: [\"-c\", \"open('provisioned.txt','w').write('x')\"]\n\
         run:\n  command: python3\n  args: [\"-c\", \"print('{}')\"]\nevents:\n  \
         - phase: pre\n    subject: tool\n    tool_class: any\n",
    )
    .unwrap();
    wd
}

#[test]
fn install_requires_the_runtimes_flag() {
    let wd = provisioning_tree("needs-flag");
    let out = oh(&["install"], &wd);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--runtimes"));
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn install_shows_the_commands_and_stops_without_yes() {
    // `pip`/`npm` execute arbitrary code at install time, so the blast radius
    // should be visible before it happens, not after.
    let wd = provisioning_tree("dry");
    let out = oh(&["install", "--runtimes", "--capabilities", "caps"], &wd);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("needs-deps") && stdout.contains("python3 -c"),
        "shows the exact command: {stdout}"
    );
    assert!(
        stdout.contains("Re-run with --yes"),
        "and stops until confirmed: {stdout}"
    );
    assert!(
        !wd.join("caps/needs-deps/provisioned.txt").exists(),
        "nothing ran"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn install_runs_the_commands_with_yes() {
    let wd = provisioning_tree("yes");
    let out = oh(
        &["install", "--runtimes", "--capabilities", "caps", "--yes"],
        &wd,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        wd.join("caps/needs-deps/provisioned.txt").is_file(),
        "the provisioning command actually ran"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn install_refuses_under_deny_network() {
    // Provisioning fetches dependencies; honouring the flag by running anyway
    // would make it a lie.
    let wd = provisioning_tree("deny-net");
    let out = oh(
        &[
            "install",
            "--runtimes",
            "--capabilities",
            "caps",
            "--yes",
            "--deny-network",
        ],
        &wd,
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--deny-network refuses"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!wd.join("caps/needs-deps/provisioned.txt").exists());
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn install_skips_unsigned_capabilities_under_require_signed() {
    let wd = provisioning_tree("signed");
    let out = oh(
        &[
            "install",
            "--runtimes",
            "--capabilities",
            "caps",
            "--yes",
            "--require-signed",
        ],
        &wd,
    );
    assert!(out.status.success(), "skipping is not a failure");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("skipped") && combined.contains("needs-deps"),
        "says what it refused and why: {combined}"
    );
    assert!(
        !wd.join("caps/needs-deps/provisioned.txt").exists(),
        "an unsigned capability is not provisioned under --require-signed"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn sync_never_provisions() {
    // Installing config and executing a package manager are different acts and
    // do not share a verb.
    let wd = provisioning_tree("sync-never");
    std::fs::write(
        wd.join("open-harness.yaml"),
        "name: p\nharnesses: [claude-code]\nsources:\n  - local:\n      path: caps\n",
    )
    .unwrap();
    let out = oh(
        &[
            "sync",
            "--profile",
            "open-harness.yaml",
            "--into",
            "project",
        ],
        &wd,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !wd.join("caps/needs-deps/provisioned.txt").exists(),
        "sync must never run a provisioning command"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn install_reports_nothing_to_do_when_no_capability_declares_provision() {
    let wd = tmp_dir("nothing");
    let cap = wd.join("caps/plain");
    std::fs::create_dir_all(&cap).unwrap();
    std::fs::write(cap.join("SKILL.md"), "---\ndescription: d\n---\n\nbody\n").unwrap();
    let out = oh(&["install", "--runtimes", "--capabilities", "caps"], &wd);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("no capabilities declare"));
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn provision_run_and_requires_resolve_a_command_the_same_way() {
    // Three code paths spawn or probe the same interpreter name. If they
    // disagree — as they did, with `provision` spawning raw while `run` remapped
    // per-OS — a capability that runs fine fails to provision on Windows.
    use open_harness::manifest::RunSpec;

    let spec = RunSpec {
        command: "python3".to_string(),
        args: vec![],
        interpreter: None,
    };
    let (run_program, _) = runtime::resolve_program(&spec);
    let probed = runtime::resolve_requirement("python3").expect("python3 is present");
    assert_eq!(
        run_program, probed,
        "`run` and `requires` must resolve the same executable"
    );

    // And provisioning actually starts, which is the observable consequence.
    let dir = tmp_dir("same-resolution");
    let cap = provisioned_cap(&dir, "open('ran.txt','w').write('x')", &[]);
    let out = runtime::provision(&cap).unwrap();
    assert!(out.success, "provision resolved and ran: {}", out.output);
    assert!(dir.join("ran.txt").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the deadline must bound collection, not just the process --------------

/// A capability that exits promptly but leaves a **grandchild** holding the
/// inherited stdout pipe must fail on its own deadline, not hang the dispatcher.
///
/// The consequence if this is wrong: `read()` on that pipe never reaches EOF, so
/// joining the reader blocks forever and the agent wedges on a tool call — the
/// exact failure the timeout exists to prevent, reached through a path the
/// timeout did not cover (it bounded the child's *exit*, not the collection of
/// its output).
#[test]
#[cfg(unix)]
fn a_grandchild_holding_the_pipe_cannot_wedge_the_dispatcher() {
    let dir = std::env::temp_dir().join(format!("oh-stuck-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("fork.sh");
    std::fs::write(
        &script,
        "#!/usr/bin/env bash\nsleep 5 &\necho '{\"decision\":\"allow\"}'\nexit 0\n",
    )
    .unwrap();

    let cap = LoadedCapability {
        manifest: serde_json::from_value(json!({
            "id": "forker",
            "timeout_ms": 300,
            "run": { "command": "fork.sh", "interpreter": "bash" },
            "events": [{ "phase": "pre", "subject": "tool", "tool_class": "any" }],
        }))
        .unwrap(),
        dir: dir.clone(),
    };

    let started = std::time::Instant::now();
    let result = runtime::run_capability(&cap, &blocking_payload(), &RunLimits::default());
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "must give up on its own deadline, took {elapsed:?}"
    );
    match result {
        Err(e) => {
            assert_eq!(e.class(), "output-stuck", "got {e}");
            assert!(
                e.to_string().contains("stdout"),
                "the reason must name the stream: {e}"
            );
        }
        Ok(d) => panic!("expected a classified failure, got {d:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A stuck stream is still a failure to produce a decision, so the default
/// policy denies on a blocking event — a guard that cannot be heard from leaves
/// you unguarded.
#[test]
fn a_stuck_stream_fails_closed_on_a_blocking_event() {
    use open_harness::manifest::FailPolicy;
    let ev = NormEvent::tool(Phase::Pre, ToolClass::Shell);
    assert!(ev.blocking());
    assert!(FailPolicy::Auto.denies(ev.blocking()));
    assert_eq!(
        RunError::OutputStuck(300, "stdout".to_string()).class(),
        "output-stuck"
    );
}
