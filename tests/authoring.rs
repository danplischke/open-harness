//! Authoring (`oh scaffold`) + generated-matrix tests (#18).

use open_harness::adapters::{Harness, ALL};
use open_harness::event::NormEvent;
use open_harness::kind::{kind_impl, Installability, KindId};
use open_harness::manifest::LoadedCapability;
use open_harness::scaffold::{self, Lang};
use open_harness::{discover, matrix};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-scaffold-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn installable(plan: &open_harness::kind::KindPlan) -> bool {
    matches!(
        plan.installability,
        Installability::Clean | Installability::Degraded(_)
    )
}

/// Acceptance: `oh scaffold` produces a runnable capability — the scaffolded
/// Python hook runs through the dispatcher and yields a decision.
#[test]
fn scaffold_hook_produces_a_runnable_capability() {
    let root = tmp("hook");
    let files = scaffold::scaffold(KindId::Hook, Lang::Python, "guard", &root).unwrap();
    assert!(files.iter().any(|f| f.ends_with("capability.json")));
    assert!(files.iter().any(|f| f.ends_with("hook.py")));

    // The manifest loads and plans on Claude.
    let cap = LoadedCapability::load(&root.join("guard/capability.json")).unwrap();
    assert_eq!(cap.manifest.kind, KindId::Hook);
    assert!(installable(
        &kind_impl(cap.manifest.kind).plan(&cap, Harness::Claude)
    ));

    // And it actually runs: dispatch it and confirm it allows (exit 0) + ran.
    let caps = discover(&root).unwrap();
    let ev: NormEvent = "pre.tool.shell".parse().unwrap();
    let native = json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } });
    let out = open_harness::dispatch(Harness::Claude, &ev, &caps, &native);
    assert_eq!(out.response.exit_code, 0, "a fresh hook allows by default");
    assert!(
        out.ran.iter().any(|id| id == "guard"),
        "the hook ran: {:?}",
        out.ran
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scaffold_supports_each_language_and_generative_kinds() {
    let root = tmp("langs");
    for lang in [Lang::Python, Lang::TypeScript, Lang::Bash] {
        let id = format!("h-{lang:?}").to_lowercase();
        scaffold::scaffold(KindId::Hook, lang, &id, &root).unwrap();
        let cap = LoadedCapability::load(&root.join(&id).join("capability.json")).unwrap();
        assert_eq!(cap.manifest.kind, KindId::Hook);
        assert!(cap.manifest.run.is_some(), "{id} has a run entrypoint");
    }
    // A generative kind scaffolds to a valid, installable manifest.
    scaffold::scaffold(KindId::Permission, Lang::Python, "policy", &root).unwrap();
    let cap = LoadedCapability::load(&root.join("policy/capability.json")).unwrap();
    assert_eq!(cap.manifest.kind, KindId::Permission);
    assert!(installable(
        &kind_impl(cap.manifest.kind).plan(&cap, Harness::Claude)
    ));

    let _ = std::fs::remove_dir_all(&root);
}

/// `oh scaffold --project` writes a TypeScript **npm package**: a valid
/// capability manifest plus the `package.json` / `tsconfig.json` / `src/hook.ts`
/// / `test.mjs` scaffolding that drives it through the core in-process via
/// `@open-harness/node`.
#[test]
fn scaffold_project_produces_a_typescript_npm_package() {
    let root = tmp("project");
    let files = scaffold::scaffold_ts_project("guard", &root).unwrap();
    for expected in [
        "package.json",
        "tsconfig.json",
        "capability.json",
        "src/hook.ts",
        "test.mjs",
        "README.md",
        ".gitignore",
    ] {
        assert!(
            files.iter().any(|f| f.ends_with(expected)),
            "scaffold writes {expected}"
        );
    }

    // The manifest is a real, loadable Hook that plans cleanly on a harness.
    let cap = LoadedCapability::load(&root.join("guard/capability.json")).unwrap();
    assert_eq!(cap.manifest.kind, KindId::Hook);
    let run = cap.manifest.run.as_ref().expect("has a run entrypoint");
    assert_eq!(run.command, "node");
    assert_eq!(run.args, vec!["dist/hook.js".to_string()]);
    assert!(installable(
        &kind_impl(cap.manifest.kind).plan(&cap, Harness::Claude)
    ));

    // package.json is valid and does NOT declare the unpublished addon as a dep
    // (that would 404 on `npm install`); the toolchain deps are present.
    let pkg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("guard/package.json")).unwrap())
            .unwrap();
    assert_eq!(pkg["name"], "guard");
    assert_eq!(pkg["type"], "module");
    assert!(pkg["scripts"]["build"].is_string() && pkg["scripts"]["test"].is_string());
    let dev = &pkg["devDependencies"];
    assert!(dev.get("typescript").is_some(), "toolchain dep present");
    assert!(
        dev.get("@open-harness/node").is_none(),
        "the unpublished addon must not be a hard dependency"
    );

    // tsconfig.json parses; the capability source imports the addon's types.
    serde_json::from_str::<serde_json::Value>(
        &std::fs::read_to_string(root.join("guard/tsconfig.json")).unwrap(),
    )
    .expect("tsconfig is valid JSON");
    let hook = std::fs::read_to_string(root.join("guard/src/hook.ts")).unwrap();
    assert!(hook.contains("@open-harness/node") && hook.contains("Decision"));

    // Refuses to clobber an existing package.
    assert!(scaffold::scaffold_ts_project("guard", &root).is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scaffold_refuses_to_overwrite_and_rejects_bad_ids() {
    let root = tmp("guard-rails");
    scaffold::scaffold(KindId::Hook, Lang::Bash, "g", &root).unwrap();
    assert!(
        scaffold::scaffold(KindId::Hook, Lang::Bash, "g", &root).is_err(),
        "must not clobber an existing capability.json"
    );
    assert!(scaffold::scaffold(KindId::Hook, Lang::Bash, "bad id!", &root).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

/// The support matrix is generated from the adapters — deterministic and
/// covering every harness (so the docs table can't be hand-maintained).
#[test]
fn support_matrix_markdown_is_generated_from_adapters() {
    let a = matrix::markdown();
    assert_eq!(a, matrix::markdown(), "generation is deterministic");
    for h in ALL {
        assert!(a.contains(h.id()), "matrix covers {}", h.id());
    }
    assert!(a.contains("native") && a.contains("fanout×") && a.contains('—'));
    // One row per representative event.
    for ev in matrix::representative_events() {
        assert!(a.contains(&ev.id()), "matrix has a row for {}", ev.id());
    }
}
