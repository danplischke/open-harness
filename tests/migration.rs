//! The JSON → YAML migration story, end to end through the CLI.
//!
//! `tests/config.rs` covers the emitter and `config::migrate_file`; this file
//! covers what a *user with an existing checkout* actually hits: `oh migrate`
//! walking a tree, `oh init` not clobbering a legacy profile, the trust store's
//! legacy path, and — the one with real consequences — a project synced by the
//! previous version converging instead of orphaning every file it managed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

fn oh(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run oh")
}

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-migration-{}-{tag}-{n}", std::process::id()));
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

// ---- oh migrate ------------------------------------------------------------

/// A tree as it looked before the switch: JSON manifests, a JSON profile, and a
/// hand-written `.json` that is a *harness's* file and must not be touched.
fn legacy_tree(tag: &str) -> PathBuf {
    let wd = tmp(tag);
    write(
        &wd.join("caps/guard/capability.json"),
        r#"{"id": "guard", "kind": "skill", "skill": {"body": "hi"}}"#,
    );
    write(
        &wd.join("caps/other/capability.json"),
        r#"{"id": "other", "kind": "rule", "rule": {"body": "r"}}"#,
    );
    write(
        &wd.join("open-harness.json"),
        r#"{"name": "p", "harnesses": ["claude-code"], "sources": [{"local": {"path": "caps"}}]}"#,
    );
    // Not ours: a harness's own config, and an unrelated project file.
    write(
        &wd.join("project/.claude/settings.json"),
        r#"{"hooks": {}}"#,
    );
    write(&wd.join("package.json"), r#"{"name": "unrelated"}"#);
    wd
}

#[test]
fn migrate_converts_our_configs_and_leaves_everyone_elses_alone() {
    let wd = legacy_tree("convert");
    let out = oh(&["migrate"], &wd);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    for gone in [
        "caps/guard/capability.json",
        "caps/other/capability.json",
        "open-harness.json",
    ] {
        assert!(
            !wd.join(gone).exists(),
            "{gone} should have been migrated away"
        );
    }
    for present in [
        "caps/guard/capability.yaml",
        "caps/other/capability.yaml",
        "open-harness.yaml",
    ] {
        assert!(wd.join(present).exists(), "{present} should exist");
    }
    // A harness's own config, and an unrelated file, are none of our business.
    assert!(wd.join("project/.claude/settings.json").exists());
    assert!(wd.join("package.json").exists());
    assert_eq!(
        std::fs::read_to_string(wd.join("package.json")).unwrap(),
        r#"{"name": "unrelated"}"#,
        "an unrelated package.json is byte-for-byte untouched"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn migrate_dry_run_changes_nothing() {
    let wd = legacy_tree("dry");
    let out = oh(&["migrate", "--dry-run"], &wd);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("would migrate"),
        "previews the work: {stdout}"
    );
    assert!(
        wd.join("open-harness.json").exists() && !wd.join("open-harness.yaml").exists(),
        "--dry-run must not write"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn migrate_is_idempotent() {
    let wd = legacy_tree("idem");
    assert!(oh(&["migrate"], &wd).status.success());
    let after_first = std::fs::read_to_string(wd.join("open-harness.yaml")).unwrap();

    let out = oh(&["migrate"], &wd);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no JSON config files to migrate"),
        "a second run finds nothing left in JSON"
    );
    assert_eq!(
        std::fs::read_to_string(wd.join("open-harness.yaml")).unwrap(),
        after_first,
        "and does not churn the file it already converted"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn migrate_leaves_an_already_yaml_file_with_a_migratable_name_alone() {
    // `open-harness.lock` and `capability.sig` carry no extension, so they stay
    // candidates by name forever. Re-running must inspect and skip the YAML
    // ones rather than rewrite them.
    let wd = tmp("idem-extensionless");
    let lock = wd.join("open-harness.lock");
    write(&lock, "version: 1\nprofile: p\nsources: []\n");
    let before = std::fs::read_to_string(&lock).unwrap();

    let out = oh(&["migrate"], &wd);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("0 file(s) migrated"),
        "the candidate is inspected and skipped: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), before);
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn migrate_warns_that_signatures_no_longer_verify() {
    // The filename is part of the content digest, so migrating is a one-way
    // door for anything already signed. Saying so is the whole point.
    let wd = legacy_tree("sig-note");
    let out = oh(&["migrate"], &wd);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("re-signed"),
        "migrate must say signatures are invalidated"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn a_migrated_tree_still_resolves() {
    // The real acceptance criterion: migration produces a working setup, not
    // just differently-shaped files.
    let wd = legacy_tree("resolves");
    assert!(oh(&["migrate"], &wd).status.success());

    let out = oh(&["resolve", "--profile", "open-harness.yaml"], &wd);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("guard") && stdout.contains("other"),
        "both migrated capabilities resolve: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn migrate_skips_hidden_directories_but_not_the_managed_one() {
    let wd = tmp("hidden");
    // `.git` is not ours; `.open-harness` holds the sync lockfile and is.
    write(
        &wd.join(".git/capability.json"),
        r#"{"id": "x", "kind": "skill"}"#,
    );
    write(
        &wd.join("project/.open-harness/sync.lock.json"),
        r#"{"version": 1, "managed": []}"#,
    );
    assert!(oh(&["migrate"], &wd).status.success());

    assert!(
        wd.join(".git/capability.json").exists(),
        "`.git` is left alone"
    );
    assert!(
        wd.join("project/.open-harness/sync.lock.yaml").exists(),
        "the managed directory is migrated"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- oh init ---------------------------------------------------------------

#[test]
fn init_writes_a_yaml_profile_that_loads() {
    let wd = tmp("init");
    let out = oh(&["init"], &wd);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = std::fs::read_to_string(wd.join("open-harness.yaml")).unwrap();
    assert!(
        text.starts_with("name: default\n"),
        "block YAML, not JSON: {text}"
    );
    open_harness::profile::Profile::from_text(&text).expect("the profile it writes must load");
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn init_refuses_when_a_legacy_json_profile_is_present() {
    // Writing `open-harness.yaml` beside a `open-harness.json` that is still
    // being read would shadow it silently.
    let wd = tmp("init-legacy");
    write(
        &wd.join("open-harness.json"),
        r#"{"name": "mine", "sources": []}"#,
    );

    let out = oh(&["init"], &wd);
    assert!(!out.status.success(), "init must refuse");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("open-harness.json"),
        "and name the file that is in the way"
    );
    assert!(!wd.join("open-harness.yaml").exists());
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- the trust store's legacy path ----------------------------------------

#[test]
fn trust_keeps_using_a_legacy_json_store_instead_of_shadowing_it() {
    // A fresh `trust.yaml` written beside a `trust.json` full of pinned keys
    // would silently drop every one of them.
    let wd = tmp("trust-legacy");
    write(
        &wd.join("caps/web/capability.yaml"),
        "id: web\nkind: skill\n",
    );
    // An author already pinned, recorded in the *legacy* store spelling.
    let old_key = open_harness::trust::generate_keyfile("alice").unwrap();
    write(
        &wd.join("trust.json"),
        &serde_json::to_string(&serde_json::json!({
            "keys": [{ "public_key": old_key.public_key, "label": "alice" }]
        }))
        .unwrap(),
    );

    // A second author signs this capability, so `oh trust` has a key to pin.
    let new_key = open_harness::trust::generate_keyfile("bob").unwrap();
    let cap = wd.join("caps/web");
    open_harness::trust::sign(&cap, &new_key)
        .unwrap()
        .write(&cap)
        .unwrap();

    let out = oh(&["trust", "--capability", "caps/web"], &wd);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        wd.join("trust.json").exists() && !wd.join("trust.yaml").exists(),
        "the existing store is used, not shadowed by a new one"
    );
    let reloaded = open_harness::trust::TrustStore::load(&wd.join("trust.json")).unwrap();
    assert!(
        reloaded.contains(&new_key.public_key),
        "the newly trusted key lands in the existing store"
    );
    assert!(
        reloaded.contains(&old_key.public_key),
        "and the previously pinned key is still there"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- the sync lockfile ------------------------------------------------------

#[test]
fn a_project_synced_before_the_switch_converges_and_prunes() {
    // The consequence if this is wrong: every file the old sync managed is
    // orphaned — never pruned, never updated, silently left behind forever.
    let wd = tmp("sync-legacy");
    write(
        &wd.join("caps/greet/SKILL.md"),
        "---\ndescription: g\n---\n\nbody\n",
    );
    write(
        &wd.join("open-harness.yaml"),
        "name: p\nharnesses: [claude-code]\nsources:\n  - local:\n      path: caps\n",
    );
    // A project synced by the previous version: a JSON lockfile claiming a file
    // whose capability no longer exists.
    write(&wd.join("project/.claude/skills/gone/SKILL.md"), "stale\n");
    // The fingerprint is what marks the file as open-harness's to prune, so the
    // fixture carries the real one — exactly what the pre-YAML sync recorded.
    write(
        &wd.join("project/.open-harness/sync.lock.json"),
        &format!(
            r#"{{"version":1,"managed":[{{"path":".claude/skills/gone/SKILL.md",
            "harnesses":["claude-code"],"sources":["gone"],"fingerprint":"{}"}}]}}"#,
            open_harness::sync::fingerprint("stale\n")
        ),
    );

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
        !wd.join("project/.claude/skills/gone/SKILL.md").exists(),
        "the legacy lockfile's managed file is pruned, not orphaned"
    );
    assert!(wd.join("project/.claude/skills/greet/SKILL.md").exists());
    assert!(
        wd.join("project/.open-harness/sync.lock.yaml").exists(),
        "the lockfile is now YAML"
    );
    assert!(
        !wd.join("project/.open-harness/sync.lock.json").exists(),
        "and the superseded JSON one is removed, so it cannot be read again"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn uninstall_removes_a_legacy_sync_lockfile_too() {
    let wd = tmp("uninstall-legacy");
    write(
        &wd.join("open-harness.yaml"),
        "name: p\nharnesses: [claude-code]\nsources: []\n",
    );
    write(&wd.join("project/.claude/skills/gone/SKILL.md"), "stale\n");
    write(
        &wd.join("project/.open-harness/sync.lock.json"),
        &format!(
            r#"{{"version":1,"managed":[{{"path":".claude/skills/gone/SKILL.md",
            "harnesses":["claude-code"],"sources":["gone"],"fingerprint":"{}"}}]}}"#,
            open_harness::sync::fingerprint("stale\n")
        ),
    );

    let out = oh(&["sync", "--into", "project", "--uninstall"], &wd);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!wd.join("project/.claude/skills/gone/SKILL.md").exists());
    assert!(
        !wd.join("project/.open-harness/sync.lock.json").exists(),
        "uninstall leaves no lockfile behind in either spelling"
    );
    let _ = std::fs::remove_dir_all(&wd);
}
