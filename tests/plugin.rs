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
