//! Importing a harness-native plugin bundle.
//!
//! The honesty question is the whole point here. A Claude Code plugin is not
//! uniformly portable: its skills/commands/agents are exactly the shapes
//! open-harness defines, its `.mcp.json` rides the shared MCP standard, and its
//! `hooks/hooks.json` is opaque shell against Claude's own schema that cannot
//! travel at all. These tests pin that each of the three lands in the right
//! place, and that the third is refused elsewhere *with a reason* rather than
//! dropped or faked.

use open_harness::adapters::Harness;
use open_harness::kind::{kind_impl, Installability, KindId};
use open_harness::profile::{self, Profile};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-plugin-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir temp");
    p
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// The hook bundle a real plugin ships: Claude's own schema, opaque commands.
const HOOKS_JSON: &str = r#"{
  "hooks": {
    "SessionStart": [{ "matcher": "startup", "hooks": [
      { "type": "command", "command": "node $CLAUDE_PLUGIN_ROOT/scripts/start.js" }]}],
    "PostToolUse": [{ "matcher": "*", "hooks": [
      { "type": "command", "command": "node $CLAUDE_PLUGIN_ROOT/scripts/observe.js" }]}]
  }
}"#;

/// A plugin bundle with one of everything.
fn plugin_bundle(tag: &str) -> PathBuf {
    let root = tmp(tag);
    write(
        &root.join(".claude-plugin/plugin.json"),
        r#"{"name": "demo-plugin", "version": "2.3.0", "description": "a demo"}"#,
    );
    write(
        &root.join("skills/mem-search/SKILL.md"),
        "---\ndescription: search memory\n---\n\nHow to search.\n",
    );
    write(
        &root.join("skills/timeline/SKILL.md"),
        "---\ndescription: timeline\n---\n\nHow to timeline.\n",
    );
    write(
        &root.join("commands/review.md"),
        "---\ndescription: review a PR\n---\n\nReview {{args}}.\n",
    );
    write(
        &root.join("agents/db-expert.md"),
        "---\ndescription: database expert\n---\n\nYou are a DB expert.\n",
    );
    write(
        &root.join(".mcp.json"),
        r#"{"mcpServers": {"search": {"command": "node", "args": ["srv.js"]}}}"#,
    );
    write(&root.join("hooks/hooks.json"), HOOKS_JSON);
    root
}

fn import(root: &Path) -> open_harness::plugin::Imported {
    open_harness::plugin::import(root).expect("imports")
}

fn kinds_of(imported: &open_harness::plugin::Imported, kind: KindId) -> Vec<String> {
    let mut v: Vec<String> = imported
        .capabilities
        .iter()
        .filter(|c| c.manifest.kind == kind)
        .map(|c| c.manifest.id.clone())
        .collect();
    v.sort();
    v
}

// ---- what travels ----------------------------------------------------------

#[test]
fn skills_commands_and_agents_import_as_themselves() {
    // These are already the shapes open-harness defines, so importing is a
    // load, not a translation.
    let root = plugin_bundle("docs");
    let imported = import(&root);

    assert_eq!(
        kinds_of(&imported, KindId::Skill),
        vec!["mem-search", "timeline"]
    );
    assert_eq!(kinds_of(&imported, KindId::Command), vec!["review"]);
    assert_eq!(kinds_of(&imported, KindId::Agent), vec!["db-expert"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_imported_skill_still_fans_out_to_every_harness_that_has_skills() {
    // The payoff: a plugin's skills stop being Claude-only the moment they are
    // imported.
    let root = plugin_bundle("fanout");
    let imported = import(&root);
    let skill = imported
        .capabilities
        .iter()
        .find(|c| c.manifest.id == "mem-search")
        .unwrap();

    for h in [Harness::Claude, Harness::Cursor, Harness::OpenCode] {
        let plan = kind_impl(KindId::Skill).plan(skill, h);
        assert!(
            !matches!(plan.installability, Installability::Unsupported(_)),
            "an imported skill should install on {}",
            h.id()
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_mcp_server_imports_as_a_portable_tool_with_a_caveat() {
    let root = plugin_bundle("mcp");
    let imported = import(&root);
    assert_eq!(kinds_of(&imported, KindId::Tool), vec!["search"]);
    assert!(
        imported
            .notes
            .iter()
            .any(|n| n.contains("search") && n.contains("may resolve against")),
        "the install-layout caveat is stated, not pretended away: {:?}",
        imported.notes
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_plugins_version_becomes_the_capabilities_version() {
    let root = plugin_bundle("version");
    let imported = import(&root);
    assert!(
        imported
            .capabilities
            .iter()
            .all(|c| c.manifest.version == "2.3.0"),
        "every capability inherits the plugin's version"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_document_that_declares_its_own_version_keeps_it() {
    let root = plugin_bundle("own-version");
    write(
        &root.join("skills/pinned/SKILL.md"),
        "---\ndescription: pinned\nversion: 9.9.9\n---\n\nbody\n",
    );
    let imported = import(&root);
    let pinned = imported
        .capabilities
        .iter()
        .find(|c| c.manifest.id == "pinned")
        .unwrap();
    assert_eq!(pinned.manifest.version, "9.9.9");
    let _ = std::fs::remove_dir_all(&root);
}

// ---- what does not travel --------------------------------------------------

#[test]
fn hooks_import_as_a_native_capability_carried_verbatim() {
    let root = plugin_bundle("hooks");
    let imported = import(&root);

    let native = imported
        .capabilities
        .iter()
        .find(|c| c.manifest.kind == KindId::Native)
        .expect("the hook bundle imports as native");
    assert_eq!(native.manifest.id, "demo-plugin-hooks");

    let plan = kind_impl(KindId::Native).plan(native, Harness::Claude);
    let contents = match plan.artifacts.first() {
        Some(open_harness::kind::Artifact::File { path, contents }) => {
            assert_eq!(path, ".claude/hooks/demo-plugin.json");
            contents.clone()
        }
        other => panic!("expected a file artifact, got {other:?}"),
    };
    assert_eq!(
        contents, HOOKS_JSON,
        "the bundle is carried byte-for-byte — open-harness does not interpret it"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_native_hook_bundle_is_unsupported_elsewhere_with_a_reason() {
    // Dropping it silently would be the easy lie; claiming it compiles would be
    // the worse one.
    let root = plugin_bundle("native-elsewhere");
    let imported = import(&root);
    let native = imported
        .capabilities
        .iter()
        .find(|c| c.manifest.kind == KindId::Native)
        .unwrap();

    for h in [
        Harness::Cursor,
        Harness::OpenCode,
        Harness::Codex,
        Harness::Aider,
    ] {
        match kind_impl(KindId::Native).plan(native, h).installability {
            Installability::Unsupported(reason) => assert!(
                reason.contains("hook@1") || reason.contains("Claude"),
                "{} should say why: {reason}",
                h.id()
            ),
            other => panic!("{} should be Unsupported, got {other:?}", h.id()),
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_import_reports_which_hooks_were_not_portable() {
    let root = plugin_bundle("hook-notes");
    let imported = import(&root);
    assert!(
        imported
            .notes
            .iter()
            .any(|n| n.contains("SessionStart") && n.contains("Unsupported on every other harness")),
        "names the events and their fate: {:?}",
        imported.notes
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---- locating the plugin ---------------------------------------------------

#[test]
fn a_marketplace_with_one_plugin_resolves_without_being_named() {
    let repo = tmp("market-one");
    write(
        &repo.join(".claude-plugin/marketplace.json"),
        r#"{"name": "vendor", "plugins": [{"name": "only", "source": "./plugin"}]}"#,
    );
    write(
        &repo.join("plugin/.claude-plugin/plugin.json"),
        r#"{"name": "only", "version": "1.0.0"}"#,
    );
    let root = open_harness::plugin::find_plugin_root(&repo, None).unwrap();
    assert_eq!(root, repo.join("plugin"));
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_marketplace_with_several_plugins_demands_a_name_and_lists_them() {
    // Guessing which of somebody's plugins you meant is not a silent decision.
    let repo = tmp("market-many");
    write(
        &repo.join(".claude-plugin/marketplace.json"),
        r#"{"plugins": [{"name": "alpha", "source": "./a"}, {"name": "beta", "source": "./b"}]}"#,
    );
    let err = open_harness::plugin::find_plugin_root(&repo, None).unwrap_err();
    assert!(
        err.contains("alpha") && err.contains("beta") && err.contains("name the one you want"),
        "lists the choices and the fix: {err}"
    );

    let picked = open_harness::plugin::find_plugin_root(&repo, Some("beta")).unwrap();
    assert_eq!(picked, repo.join("b"));
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn asking_for_a_plugin_the_marketplace_does_not_publish_lists_what_it_does() {
    let repo = tmp("market-miss");
    write(
        &repo.join(".claude-plugin/marketplace.json"),
        r#"{"plugins": [{"name": "alpha", "source": "./a"}]}"#,
    );
    let err = open_harness::plugin::find_plugin_root(&repo, Some("nope")).unwrap_err();
    assert!(err.contains("nope") && err.contains("alpha"), "{err}");
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_directory_that_is_not_a_plugin_says_so() {
    let dir = tmp("not-a-plugin");
    write(&dir.join("README.md"), "just a repo\n");
    let err = open_harness::plugin::find_plugin_root(&dir, None).unwrap_err();
    assert!(err.contains("not a plugin bundle"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_bundle_is_reported_rather_than_silently_yielding_nothing() {
    let root = tmp("empty-bundle");
    write(
        &root.join(".claude-plugin/plugin.json"),
        r#"{"name": "hollow", "version": "1.0.0"}"#,
    );
    let imported = import(&root);
    assert!(imported.capabilities.is_empty());
    assert!(
        imported
            .notes
            .iter()
            .any(|n| n.contains("nothing open-harness can import")),
        "an empty import says so: {:?}",
        imported.notes
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---- as a profile source ---------------------------------------------------

#[test]
fn a_plugin_source_composes_and_namespaces_by_the_plugins_own_name() {
    // The plugin's published name is a better identity than `<owner>/<repo>`,
    // which is only where it happens to be hosted.
    let bundle = plugin_bundle("as-source");
    let wd = tmp("as-source-wd");
    let p = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "plugin": { "url": bundle.to_string_lossy() } }],
        })
        .to_string(),
    )
    .unwrap();

    let r = profile::resolve(&p, &wd, None).unwrap();
    assert_eq!(
        r.capabilities.len(),
        6,
        "2 skills + command + agent + tool + native"
    );
    assert!(
        r.capabilities
            .iter()
            .all(|c| c.qualified_name().starts_with("demo-plugin/")),
        "namespaced by the plugin's own name: {:?}",
        r.capabilities
            .iter()
            .map(|c| c.qualified_name())
            .collect::<Vec<_>>()
    );
    assert_eq!(r.lock.sources[0].kind, "plugin");
    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&bundle);
}

#[test]
fn select_applies_to_a_plugin_source() {
    // Taking a plugin's skills without its native hook bundle is the obvious
    // want, and `kinds` already expresses it.
    let bundle = plugin_bundle("select");
    let wd = tmp("select-wd");
    let p = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "plugin": {
                "url": bundle.to_string_lossy(),
                "select": { "kinds": ["skill"] },
            } }],
        })
        .to_string(),
    )
    .unwrap();

    let r = profile::resolve(&p, &wd, None).unwrap();
    let mut ids: Vec<String> = r
        .capabilities
        .iter()
        .map(|c| c.manifest.id.clone())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["mem-search", "timeline"]);
    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&bundle);
}

#[test]
fn the_import_notes_surface_as_resolver_warnings() {
    // Which parts of a plugin travelled is exactly what a user needs told, so
    // the notes are not swallowed by the importer.
    let bundle = plugin_bundle("warnings");
    let wd = tmp("warnings-wd");
    let p = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "plugin": { "url": bundle.to_string_lossy() } }],
        })
        .to_string(),
    )
    .unwrap();

    let r = profile::resolve(&p, &wd, None).unwrap();
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("plugin 'demo-plugin'") && w.contains("hook")),
        "the hook caveat reaches the user: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&bundle);
}

// ---- the native kind on its own --------------------------------------------

fn native_cap(config: serde_json::Value) -> open_harness::manifest::LoadedCapability {
    let manifest: open_harness::manifest::Manifest = serde_json::from_value(json!({
        "id": "x", "kind": "native", "native": config
    }))
    .unwrap();
    open_harness::manifest::LoadedCapability {
        manifest,
        dir: PathBuf::from("."),
    }
}

#[test]
fn a_native_capability_naming_no_harness_is_blocked_not_silently_installed() {
    let cap = native_cap(json!({ "artifacts": [{ "path": "x", "body": "y" }] }));
    match kind_impl(KindId::Native)
        .plan(&cap, Harness::Claude)
        .installability
    {
        Installability::Unsupported(r) => assert!(r.contains("no `harness`"), "{r}"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn a_native_capability_naming_an_unknown_harness_says_which() {
    let cap = native_cap(json!({ "harness": "emacs", "artifacts": [{ "path": "x" }] }));
    match kind_impl(KindId::Native)
        .plan(&cap, Harness::Claude)
        .installability
    {
        Installability::Unsupported(r) => assert!(r.contains("emacs"), "{r}"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn a_native_capability_with_no_artifacts_is_blocked() {
    let cap = native_cap(json!({ "harness": "claude-code" }));
    match kind_impl(KindId::Native)
        .plan(&cap, Harness::Claude)
        .installability
    {
        Installability::Unsupported(r) => assert!(r.contains("no artifacts"), "{r}"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn native_is_degraded_even_on_its_own_harness() {
    // It installs, but calling it "clean" would imply a portability it does not
    // have — every other harness gets nothing.
    let cap = native_cap(json!({
        "harness": "claude-code",
        "artifacts": [{ "path": ".claude/x.json", "body": "{}" }],
    }));
    match kind_impl(KindId::Native)
        .plan(&cap, Harness::Claude)
        .installability
    {
        Installability::Degraded(d) => assert!(d.contains("not portable"), "{d}"),
        other => panic!("expected Degraded, got {other:?}"),
    }
}

#[test]
fn native_cannot_be_scaffolded() {
    // It is what importing produces, not a template for writing unportable
    // config by hand.
    let dir = tmp("no-scaffold");
    let err = open_harness::scaffold::scaffold(
        KindId::Native,
        open_harness::scaffold::Lang::Python,
        "mine",
        &dir,
    )
    .unwrap_err();
    assert!(err.contains("importing a plugin"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- adopting a plugin: the path a person actually meets first --------------
//
// The import machinery above was correct long before any of this worked. What
// did not work was reaching it: `oh add` accepted a `plugin:` string and wrote a
// broken profile, `oh try` read a bundle as a plain directory and produced a
// confidently wrong answer, and two capabilities that merely shared a name were
// treated as duplicates. These pin the adoption path.

use open_harness::profile::Source;

#[test]
fn a_plugin_has_a_spec_form_so_add_and_try_can_take_one() {
    // Inference cannot tell a capability repo from a plugin bundle by URL alone
    // — the same repository could be either — so the prefix is what decides,
    // exactly as `git+` does.
    let Source::Plugin(p) =
        Source::parse_spec("plugin+https://github.com/ruvnet/ruflo@main#name=ruflo-core").unwrap()
    else {
        panic!("plugin+ must infer a plugin source");
    };
    assert_eq!(p.url, "https://github.com/ruvnet/ruflo");
    assert_eq!(p.rev.as_deref(), Some("main"));
    assert_eq!(p.name.as_deref(), Some("ruflo-core"));

    // `plugin:` is the other spelling people type, and a local checkout needs
    // no rev — the same rule the mapping form already had.
    let Source::Plugin(local) = Source::parse_spec("plugin:../checkouts/claude-mem").unwrap()
    else {
        panic!("plugin: must infer a plugin source");
    };
    assert_eq!(local.url, "../checkouts/claude-mem");
    assert!(local.rev.is_none(), "no @rev means a local path, not git");
}

#[test]
fn a_plugin_spec_refuses_what_it_cannot_read_instead_of_guessing() {
    for bad in [
        "plugin+",
        "plugin+https://example.com/repo@",
        "plugin+https://example.com/repo#nonsense=1",
    ] {
        let err = Source::parse_spec(bad).unwrap_err();
        assert!(!err.is_empty(), "{bad} should be refused");
    }
    // The fragment error names the keys that *are* accepted.
    let err = Source::parse_spec("plugin+https://example.com/r#nonsense=1").unwrap_err();
    assert!(
        err.contains("name=") && err.contains("subdirectory="),
        "{err}"
    );
}

#[test]
fn a_plugin_bundle_is_refused_as_a_local_directory_rather_than_misread() {
    // Both are "a directory with files in it", so scanning a bundle as a local
    // source *succeeds* — and quietly produces a wrong answer: skills load as
    // bare single-file capabilities with no version and no namespace, and the
    // `.mcp.json` and `hooks/hooks.json` are not seen at all. A plausible
    // partial reading is the worst failure this project can have.
    let bundle = plugin_bundle("misread");
    let wd = tmp("misread-wd");
    let p = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [bundle.to_string_lossy()],
        })
        .to_string(),
    )
    .unwrap();

    let err = match profile::resolve(&p, &wd, None) {
        Err(e) => e,
        Ok(_) => panic!("a plugin bundle must not resolve as a local directory"),
    };
    assert!(err.contains("Claude plugin bundle"), "{err}");
    assert!(
        err.contains("plugin:"),
        "the error must carry the fix, not just the diagnosis: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&bundle);
}

#[test]
fn a_marketplace_directory_is_refused_with_the_name_fragment_in_the_fix() {
    let repo = tmp("misread-market");
    write(
        &repo.join(".claude-plugin/marketplace.json"),
        r#"{"plugins": [{"name": "alpha", "source": "./a"}]}"#,
    );
    let wd = tmp("misread-market-wd");
    let p = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [repo.to_string_lossy()],
        })
        .to_string(),
    )
    .unwrap();
    let err = match profile::resolve(&p, &wd, None) {
        Err(e) => e,
        Ok(_) => panic!("a marketplace must not resolve as a local directory"),
    };
    assert!(err.contains("marketplace"), "{err}");
    assert!(
        err.contains("#name="),
        "a marketplace needs the plugin named, and the fix should say so: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&repo);
}

/// A bundle shipping one name as *both* a skill and a command — which real
/// plugins do routinely.
fn plugin_with_a_shared_name(tag: &str) -> PathBuf {
    let root = tmp(tag);
    write(
        &root.join(".claude-plugin/plugin.json"),
        r#"{"name": "dual", "version": "1.0.0"}"#,
    );
    write(
        &root.join("skills/status/SKILL.md"),
        "---\ndescription: the status skill\n---\n\nHow to read status.\n",
    );
    write(
        &root.join("commands/status.md"),
        "---\ndescription: the status command\n---\n\nPrint status.\n",
    );
    root
}

#[test]
fn a_skill_and_a_command_with_one_name_both_survive() {
    // Identity is (name, kind). Keyed on the name alone, one of these was
    // dropped as a duplicate of the other — a silent loss of half a plugin's
    // surface, reported as "shadowed by an earlier source" when there was only
    // ever one source.
    let bundle = plugin_with_a_shared_name("dual");
    let wd = tmp("dual-wd");
    let p = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "plugin": { "url": bundle.to_string_lossy() } }],
        })
        .to_string(),
    )
    .unwrap();

    let r = profile::resolve(&p, &wd, None).unwrap();
    let mut got: Vec<String> = r
        .capabilities
        .iter()
        .map(|c| format!("{}:{}", c.manifest.kind.as_str(), c.manifest.id))
        .collect();
    got.sort();
    assert_eq!(got, vec!["command:status", "skill:status"]);
    assert!(
        !r.warnings.iter().any(|w| w.contains("shadowed")),
        "neither shadows the other: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&bundle);
}

#[test]
fn the_bare_id_clash_warning_only_fires_within_a_kind() {
    // The warning tells you to rename one of them. Firing it across kinds meant
    // proposing a fix for a problem the user did not have: a skill emits to
    // `skills/<id>/SKILL.md` and a command to `commands/<id>.md`, so there is
    // no shared filename to clash over.
    let bundle = plugin_with_a_shared_name("dual-warn");
    let wd = tmp("dual-warn-wd");
    let p = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "plugin": { "url": bundle.to_string_lossy() } }],
        })
        .to_string(),
    )
    .unwrap();
    let r = profile::resolve(&p, &wd, None).unwrap();
    assert!(
        !r.warnings.iter().any(|w| w.contains("bare id")),
        "different kinds are not a filename clash: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&bundle);
}

#[test]
fn a_plugin_can_be_reached_as_a_transitive_dependency() {
    // The sharp edge worth pinning: the plugin's *name* is the namespace, not a
    // capability, so a dependency must name something inside it. Depending on
    // "dual" acquires the bundle and still reports the requirement unmet.
    let bundle = plugin_with_a_shared_name("dep");
    let wd = tmp("dep-wd");
    let manifest = format!(
        "id: needs-it\n\
         name: Needs It\n\
         description: depends on a capability inside a plugin\n\
         runtime:\n  requires: [python3]\n\
         run:\n  command: python3\n  args: [x.py]\n\
         events:\n  - phase: pre\n    subject: tool\n    tool_class: any\n\
         dependencies:\n\
         \x20 status:\n\
         \x20   version: \"*\"\n\
         \x20   relation: requires\n\
         \x20   source:\n\
         \x20     plugin:\n\
         \x20       url: {}\n",
        bundle.to_string_lossy()
    );
    write(&wd.join("caps/needs-it/capability.yaml"), &manifest);
    write(&wd.join("caps/needs-it/x.py"), "print(1)\n");
    let p = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "resolution": "transitive", "transitive_trust": "any",
            "sources": ["caps"],
        })
        .to_string(),
    )
    .unwrap();

    let r = profile::resolve(&p, &wd, None).unwrap();
    let ids: Vec<&str> = r
        .capabilities
        .iter()
        .map(|c| c.manifest.id.as_str())
        .collect();
    assert!(ids.contains(&"status"), "the plugin was acquired: {ids:?}");
    assert!(
        !r.warnings
            .iter()
            .any(|w| w.contains("requires 'status'") && w.contains("no source provides")),
        "naming a capability inside the plugin satisfies the dependency: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&bundle);
}
