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
    assert!(files.iter().any(|f| f.ends_with("capability.yaml")));
    assert!(files.iter().any(|f| f.ends_with("hook.py")));

    // The manifest loads and plans on Claude.
    let cap = LoadedCapability::load(&root.join("guard/capability.yaml")).unwrap();
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
    // Python and Node are peers (plain scripts); TypeScript emits a typed
    // `hook.ts` that Node runs directly via type stripping — each binds the
    // right entrypoint + interpreter.
    for (lang, script, command) in [
        (Lang::Python, "hook.py", "python3"),
        (Lang::Node, "hook.mjs", "node"),
        (Lang::TypeScript, "hook.ts", "node"),
        (Lang::Bash, "hook.sh", "sh"),
    ] {
        let id = format!("h-{lang:?}").to_lowercase();
        let files = scaffold::scaffold(KindId::Hook, lang, &id, &root).unwrap();
        assert!(
            files.iter().any(|f| f.ends_with(script)),
            "{id} writes {script}"
        );
        let cap = LoadedCapability::load(&root.join(&id).join("capability.yaml")).unwrap();
        assert_eq!(cap.manifest.kind, KindId::Hook);
        let run = cap.manifest.run.as_ref().expect("has a run entrypoint");
        assert_eq!(run.command, command, "{id} runs via {command}");
        assert_eq!(run.args, vec![script.to_string()]);
    }

    // `--lang` aliases resolve node/js to Node and typescript/ts to TypeScript.
    assert_eq!(Lang::parse("js"), Some(Lang::Node));
    assert_eq!(Lang::parse("node"), Some(Lang::Node));
    assert_eq!(Lang::parse("ts"), Some(Lang::TypeScript));
    assert_eq!(Lang::parse("typescript"), Some(Lang::TypeScript));
    assert!(Lang::parse("cobol").is_none());

    // A generative kind scaffolds to a valid, installable manifest.
    scaffold::scaffold(KindId::Permission, Lang::Python, "policy", &root).unwrap();
    let cap = LoadedCapability::load(&root.join("policy/capability.yaml")).unwrap();
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
        "capability.yaml",
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
    let cap = LoadedCapability::load(&root.join("guard/capability.yaml")).unwrap();
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
        "must not clobber an existing capability.yaml"
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

// ---- the example corpus as documentation ----------------------------------

/// `capabilities/` doubles as the by-example documentation for the authoring
/// formats, so which form each example uses is a real decision, not an
/// accident.
///
/// A document kind can be authored two ways — a single `SKILL.md`/`RULE.md`/…
/// whose frontmatter *is* the manifest, or a `capability.yaml` pointing at a
/// `body_file`. For those kinds the two are equally expressive (the only
/// manifest field `from_doc` will not lift is `permissions`, which a document
/// kind has no use for), so the single-file form is the ergonomic default and
/// the corpus should read that way.
///
/// It briefly did not: `command` and `rule` existed *only* in the manifest
/// form, which made the whole directory look like open-harness wanted YAML for
/// everything. These two gates keep both forms represented without letting
/// either become the only face of a kind.
fn corpus() -> Vec<LoadedCapability> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capabilities");
    discover(&dir).expect("the example corpus must load")
}

fn is_single_file(cap: &LoadedCapability) -> bool {
    !cap.dir
        .join(open_harness::manifest::MANIFEST_NAME)
        .is_file()
}

/// The kinds that have a single-file document form at all. Hooks, tools and
/// permissions are structured config with no body, so they have no document
/// form and are irrelevant to this gate.
const DOCUMENT_KINDS: [KindId; 5] = [
    KindId::Skill,
    KindId::Agent,
    KindId::Command,
    KindId::Rule,
    KindId::Instructions,
];

#[test]
fn no_document_kind_is_demonstrated_only_as_a_manifest() {
    let caps = corpus();
    for kind in DOCUMENT_KINDS {
        let of_kind: Vec<&LoadedCapability> =
            caps.iter().filter(|c| c.manifest.kind == kind).collect();
        if of_kind.is_empty() {
            continue;
        }
        assert!(
            of_kind.iter().any(|c| is_single_file(c)),
            "`{}` is only shown in the capability.yaml form ({:?}) — a reader learns the \
             heavier authoring path is the only one. Author at least one as a single {}.md",
            kind.as_str(),
            of_kind.iter().map(|c| &c.manifest.id).collect::<Vec<_>>(),
            kind.as_str().to_uppercase()
        );
    }
}

/// ...and the manifest form must not disappear either: `body_file`, per-harness
/// `overrides` and a generated manifest are all real, and an example set that
/// never shows them teaches that they do not exist.
#[test]
fn the_manifest_form_is_still_demonstrated_for_a_document_kind() {
    let caps = corpus();
    let manifest_authored: Vec<&str> = caps
        .iter()
        .filter(|c| DOCUMENT_KINDS.contains(&c.manifest.kind) && !is_single_file(c))
        .map(|c| c.manifest.id.as_str())
        .collect();
    assert!(
        !manifest_authored.is_empty(),
        "no document-kind capability is authored as a capability.yaml — the manifest form, \
         `body_file` and per-harness `overrides` are now undocumented by example"
    );
    // And it must be a real body_file, not an inline body pretending.
    let uses_body_file = caps.iter().any(|c| {
        !is_single_file(c)
            && DOCUMENT_KINDS.contains(&c.manifest.kind)
            && c.manifest
                .config
                .get(c.manifest.kind.as_str())
                .and_then(|v| v.get("body_file"))
                .is_some()
    });
    assert!(
        uses_body_file,
        "the manifest-authored example(s) {manifest_authored:?} do not use `body_file`, so \
         nothing in the corpus shows keeping a long body out of the manifest"
    );
}

/// A `body_file` that is also a recognized document name (`RULE.md` next to a
/// `capability.yaml`) is readable two ways, and `discover` has to pick — it
/// picks the manifest. Avoiding the ambiguity outright is clearer than relying
/// on that tie-break.
#[test]
fn a_body_file_is_not_named_like_a_single_file_capability() {
    for cap in corpus() {
        if is_single_file(&cap) {
            continue;
        }
        let Some(bf) = cap
            .manifest
            .config
            .get(cap.manifest.kind.as_str())
            .and_then(|v| v.get("body_file"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        assert!(
            !matches!(
                bf,
                "SKILL.md" | "AGENT.md" | "COMMAND.md" | "RULE.md" | "INSTRUCTIONS.md"
            ),
            "`{}` uses `{bf}` as a body_file, which is also a single-file capability name — \
             the directory now reads as both forms at once. Name the body something else",
            cap.manifest.id
        );
    }
}

/// The matrix rows are derived from the event space, not listed.
///
/// They used to be a hand-picked slice, so an event the adapters fully
/// supported could be absent from the published grid without tripping the CI
/// drift gate — that gate only ever compared the rendered table against the
/// same fixed list. `post.session.end` was exactly that: bindable, installable
/// on four harnesses, and missing from the grid. Under-reporting real support
/// is the honest-degradation rule pointing the wrong way.
#[test]
fn every_event_a_harness_can_host_has_a_matrix_row() {
    let rows = matrix::representative_events();
    let missing: Vec<String> = matrix::all_events()
        .into_iter()
        .filter(|ev| {
            ALL.iter().any(|h| {
                !matches!(
                    h.support(ev),
                    open_harness::adapters::Support::Unsupported(_)
                )
            })
        })
        .filter(|ev| !rows.contains(ev))
        .map(|ev| ev.id())
        .collect();
    assert!(
        missing.is_empty(),
        "these events are supported by at least one harness but absent from the grid: {missing:?}"
    );
    // The specific regression.
    assert!(
        rows.iter().any(|e| e.id() == "post.session.end"),
        "post.session.end must be a row: {:?}",
        rows.iter().map(|e| e.id()).collect::<Vec<_>>()
    );
}

/// ...and the converse: a row no harness can host is a statement about the
/// event model, not about the harnesses, and does not belong in a support grid.
#[test]
fn the_matrix_has_no_row_that_every_harness_refuses() {
    for ev in matrix::representative_events() {
        assert!(
            ALL.iter().any(|h| !matches!(
                h.support(&ev),
                open_harness::adapters::Support::Unsupported(_)
            )),
            "`{}` is a row but no harness hosts it",
            ev.id()
        );
    }
}

/// A harness caveat has to be about the capability in front of you. Codex's is
/// about which *tool* calls reach `PreToolUse`; attached to a session-teardown
/// capability it says something true about the harness and false about the
/// binding.
#[test]
fn a_tool_only_platform_note_does_not_leak_onto_other_events() {
    use open_harness::event::{Boundary, NormEvent, Phase, SubjectKind};
    let session_end = NormEvent {
        phase: Phase::Post,
        subject: SubjectKind::Session,
        tool_class: None,
        boundary: Some(Boundary::End),
        task_kind: None,
    };
    let tool = NormEvent::tool(Phase::Pre, open_harness::event::ToolClass::Any);

    assert!(
        Harness::Codex.platform_note(&[session_end]).is_none(),
        "the PreToolUse caveat must not attach to a session binding"
    );
    assert!(
        Harness::Codex
            .platform_note(&[tool])
            .is_some_and(|n| n.contains("PreToolUse")),
        "it must still attach where it applies"
    );
    // A whole-harness limit applies to every binding, tool or not.
    assert!(
        Harness::Cline.platform_note(&[session_end]).is_some(),
        "Cline's platform limit is a property of the harness, not the event"
    );
}
