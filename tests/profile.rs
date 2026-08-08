//! Profile / sources / lockfile tests (#15).
//!
//! The resolver itself is platform-independent; the git-backed tests spawn the
//! system `git` against a throwaway *local* repo (no network), and are
//! `#[cfg(unix)]` only to avoid exercising temp-repo fixture wrangling on a
//! Windows runner we can't observe — git's behavior is identical either way.

use open_harness::adapters::Harness;
use open_harness::profile::{self, Lock, Profile};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-profile-{}-{tag}-{n}", std::process::id()));
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

fn profile_from(v: serde_json::Value) -> Profile {
    Profile::from_text(&v.to_string()).expect("valid profile")
}

/// A capability manifest as the YAML that authors actually write — not JSON in
/// a `.yaml` file, which YAML would accept and which would leave the block
/// syntax untested.
fn cap_yaml(id: &str, version: &str, kind: &str, extra: serde_json::Value) -> String {
    let mut m = json!({
        "id": id, "name": id, "description": "x", "version": version, "kind": kind,
    });
    if let (Some(mo), Some(eo)) = (m.as_object_mut(), extra.as_object()) {
        for (k, val) in eo {
            mo.insert(k.clone(), val.clone());
        }
    }
    open_harness::config::to_yaml(&m).expect("render manifest")
}

// ---- cross-platform: resolver, lockfile, warnings -------------------------

#[test]
fn local_only_profile_resolves_and_locks() {
    let wd = tmp("local");
    write(
        &wd.join("caps/greet/capability.yaml"),
        &cap_yaml(
            "greet",
            "1.2.0",
            "skill",
            json!({ "skill": { "body": "hi" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));

    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert_eq!(r.capabilities.len(), 1);
    assert_eq!(r.harnesses, vec![Harness::Claude]);
    assert_eq!(r.lock.sources.len(), 1);
    let src = &r.lock.sources[0];
    assert_eq!(src.kind, "local");
    assert!(src.rev.is_none(), "local sources have no pinned rev");
    assert_eq!(src.capabilities[0].id, "greet");
    assert_eq!(src.capabilities[0].version, "1.2.0");
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn resolve_is_deterministic_for_local() {
    let wd = tmp("determ");
    write(
        &wd.join("caps/a/capability.yaml"),
        &cap_yaml("a", "0.1.0", "rule", json!({ "rule": { "body": "x" } })),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["cursor"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let a = profile::resolve(&profile, &wd, None).unwrap().lock;
    let b = profile::resolve(&profile, &wd, None).unwrap().lock;
    assert_eq!(a, b, "the same inputs must produce the same lock");
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn lock_round_trips_through_yaml() {
    let wd = tmp("lockrt");
    write(
        &wd.join("caps/a/capability.yaml"),
        &cap_yaml("a", "2.0.0", "skill", json!({ "skill": { "body": "y" } })),
    );
    let profile = profile_from(json!({
        "name": "rt", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let lock = profile::resolve(&profile, &wd, None).unwrap().lock;
    let round = Lock::from_text(&lock.to_yaml()).unwrap();
    assert_eq!(lock, round);
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn registry_source_is_reported_not_silently_skipped() {
    let wd = tmp("registry");
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "registry": { "name": "some-pack", "version": "1.0.0" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(r.capabilities.is_empty());
    assert!(
        r.warnings.iter().any(|w| w.contains("registry")),
        "a registry source must be loudly reported, not silently dropped: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn registry_source_resolves_through_a_local_index() {
    let wd = tmp("registry-resolve");
    // A capability living under packs/, reached only via the registry index.
    write(
        &wd.join("packs/greeter/capability.yaml"),
        &cap_yaml(
            "greeter",
            "2.1.0",
            "skill",
            json!({ "skill": { "body": "hi" } }),
        ),
    );
    // The index maps the pack name → that real (local) source.
    write(
        &wd.join("registry.json"),
        &json!({
            "capabilities": [
                { "name": "greeter-pack", "version": "2.1.0",
                  "source": { "local": { "path": "packs" } } }
            ]
        })
        .to_string(),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "registry": { "index": "registry.json", "name": "greeter-pack", "version": "2.1.0" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    let ids: Vec<&str> = r
        .capabilities
        .iter()
        .map(|c| c.manifest.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["greeter"],
        "the registry resolved the pack's capability: {ids:?}"
    );
    let src = &r.lock.sources[0];
    assert_eq!(src.kind, "registry", "provenance is recorded as registry");
    assert!(
        src.origin.contains("greeter-pack"),
        "the resolved name is pinned: {}",
        src.origin
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn registry_missing_entry_is_warned_not_dropped() {
    let wd = tmp("registry-miss");
    write(
        &wd.join("registry.json"),
        &json!({ "capabilities": [] }).to_string(),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "registry": { "index": "registry.json", "name": "nope" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(r.capabilities.is_empty());
    assert!(
        r.warnings.iter().any(|w| w.contains("nope")),
        "a missing registry entry is loudly reported: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn missing_dependency_is_warned() {
    let wd = tmp("deps");
    write(
        &wd.join("caps/a/capability.yaml"),
        &cap_yaml(
            "a",
            "0.1.0",
            "skill",
            json!({ "dependencies": ["nope"], "skill": { "body": "z" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(
        r.warnings.iter().any(|w| w.contains("nope")),
        "an unmet dependency must be warned: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn dependencies_order_topologically() {
    let wd = tmp("topo");
    // Source order is a, b, c (discover sorts by id); the graph a→b→c must
    // reorder them so each dependency precedes its dependent.
    write(
        &wd.join("caps/a/capability.yaml"),
        &cap_yaml(
            "a",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["b"], "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/b/capability.yaml"),
        &cap_yaml(
            "b",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["c"], "skill": { "body": "b" } }),
        ),
    );
    write(
        &wd.join("caps/c/capability.yaml"),
        &cap_yaml("c", "1.0.0", "skill", json!({ "skill": { "body": "c" } })),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    let ids: Vec<&str> = r
        .capabilities
        .iter()
        .map(|c| c.manifest.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["c", "b", "a"],
        "a dependency is composed before its dependents"
    );
    assert!(
        r.warnings.is_empty(),
        "a satisfied graph warns about nothing: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn dependency_cycle_is_warned_not_hung() {
    let wd = tmp("cycle");
    write(
        &wd.join("caps/a/capability.yaml"),
        &cap_yaml(
            "a",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["b"], "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/b/capability.yaml"),
        &cap_yaml(
            "b",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["a"], "skill": { "body": "b" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert_eq!(r.capabilities.len(), 2, "a cycle drops nothing");
    assert!(
        r.warnings.iter().any(|w| w.contains("cycle")),
        "the cycle is reported, not hung on: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn resolved_dependencies_are_recorded_in_the_lock() {
    let wd = tmp("lockdeps");
    write(
        &wd.join("caps/a/capability.yaml"),
        &cap_yaml(
            "a",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["b"], "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/b/capability.yaml"),
        &cap_yaml("b", "1.0.0", "skill", json!({ "skill": { "body": "b" } })),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let lock = profile::resolve(&profile, &wd, None).unwrap().lock;
    let a = lock.sources[0]
        .capabilities
        .iter()
        .find(|c| c.id == "a")
        .unwrap();
    assert_eq!(
        a.dependencies,
        std::collections::BTreeMap::from([("b".to_string(), "1.0.0".to_string())]),
        "the lock pins the resolved graph: name AND the version it resolved to"
    );
    assert_eq!(
        lock,
        Lock::from_text(&lock.to_yaml()).unwrap(),
        "the graph survives a lock round-trip"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn unknown_harness_is_rejected() {
    let wd = tmp("badharness");
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["not-a-real-harness"],
        "sources": [],
    }));
    assert!(profile::resolve(&profile, &wd, None).is_err());
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- git-backed personal-repo sync (unix-gated fixture) -------------------

#[cfg(unix)]
mod git_backed {
    #![allow(unused_imports)]
    use super::*;
    use std::process::Command;

    pub(super) fn git(args: &[&str], cwd: &Path) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git available")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    pub(super) fn init_repo_with(cap_rel: &str, cap: &str) -> (PathBuf, String) {
        let repo = tmp("gitrepo");
        write(&repo.join(cap_rel), cap);
        git(&["init", "-q"], &repo);
        git(&["config", "user.email", "t@e.st"], &repo);
        git(&["config", "user.name", "T"], &repo);
        git(&["add", "-A"], &repo);
        git(&["commit", "-qm", "caps"], &repo);
        let sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        (repo, sha)
    }

    /// Acceptance: a profile composed from a git source + a local source, with a
    /// reproducible lockfile that pins the git commit.
    #[test]
    fn profile_composes_git_and_local_with_pinned_lock() {
        let (repo, sha) = init_repo_with(
            "caps/conv/capability.yaml",
            &cap_yaml(
                "conv",
                "0.4.1",
                "rule",
                json!({ "rule": { "body": "tabs" } }),
            ),
        );
        let wd = tmp("compose");
        write(
            &wd.join("local/greet/capability.yaml"),
            &cap_yaml(
                "greet",
                "1.0.0",
                "skill",
                json!({ "skill": { "body": "hi" } }),
            ),
        );
        let profile = profile_from(json!({
            "name": "mix", "harnesses": ["claude-code"],
            "sources": [
                { "local": { "path": "local" } },
                { "git": { "url": repo.to_string_lossy(), "rev": "HEAD", "subdir": "caps" } },
            ],
        }));

        let r = profile::resolve(&profile, &wd, None).unwrap();
        let ids: Vec<&str> = r
            .capabilities
            .iter()
            .map(|c| c.manifest.id.as_str())
            .collect();
        assert!(
            ids.contains(&"greet") && ids.contains(&"conv"),
            "both sources compose: {ids:?}"
        );

        let git_src = r.lock.sources.iter().find(|s| s.kind == "git").unwrap();
        assert_eq!(
            git_src.rev.as_deref(),
            Some(sha.as_str()),
            "git commit is pinned"
        );
        assert_eq!(git_src.capabilities[0].id, "conv");
        assert_eq!(git_src.capabilities[0].version, "0.4.1");

        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Reproducibility: resolving with an existing lock pins the recorded commit
    /// even after the upstream repo advances.
    #[test]
    fn lock_pins_reproducibly_across_new_commits() {
        let (repo, sha1) = init_repo_with(
            "caps/x/capability.yaml",
            &cap_yaml("x", "1.0.0", "skill", json!({ "skill": { "body": "v1" } })),
        );
        let wd = tmp("repro");
        let profile = profile_from(json!({
            "name": "r", "harnesses": ["claude-code"],
            "sources": [{ "git": { "url": repo.to_string_lossy(), "rev": "HEAD", "subdir": "caps" } }],
        }));

        let lock1 = profile::resolve(&profile, &wd, None).unwrap().lock;
        assert_eq!(lock1.sources[0].rev.as_deref(), Some(sha1.as_str()));

        // Advance the upstream repo.
        write(
            &repo.join("caps/y/capability.yaml"),
            &cap_yaml("y", "1.0.0", "rule", json!({ "rule": { "body": "new" } })),
        );
        git(&["add", "-A"], &repo);
        git(&["commit", "-qm", "more"], &repo);

        // Re-resolve WITH the old lock: it must stay pinned to sha1, not advance.
        let r2 = profile::resolve(&profile, &wd, Some(&lock1)).unwrap();
        assert_eq!(
            r2.lock.sources[0].rev.as_deref(),
            Some(sha1.as_str()),
            "an existing lock pins the commit; the new upstream commit is ignored"
        );
        let ids: Vec<&str> = r2
            .capabilities
            .iter()
            .map(|c| c.manifest.id.as_str())
            .collect();
        assert!(
            ids.contains(&"x") && !ids.contains(&"y"),
            "pinned tree only: {ids:?}"
        );

        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Regression: a git source must actually advance when the branch does.
    ///
    /// `fetch` updates `origin/<branch>` but never fast-forwards the cache's
    /// local branch of the same name, so `checkout <branch>` used to resolve to
    /// whatever commit was HEAD at first clone — permanently. Every git source
    /// was frozen at the moment it was first cloned, and no amount of
    /// re-resolving would pick up upstream work.
    #[test]
    fn a_branch_source_picks_up_new_upstream_commits() {
        let (repo, sha1) = init_repo_with(
            "caps/x/capability.yaml",
            &cap_yaml("x", "1.0.0", "skill", json!({ "skill": { "body": "v1" } })),
        );
        let branch = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let wd = tmp("advance");
        let profile = profile_from(json!({
            "name": "a", "harnesses": ["claude-code"],
            "sources": [{ "git": {
                "url": repo.to_string_lossy(), "rev": branch, "subdir": "caps" } }],
        }));

        // First resolve populates the cache clone.
        let lock1 = profile::resolve(&profile, &wd, None).unwrap().lock;
        assert_eq!(lock1.sources[0].rev.as_deref(), Some(sha1.as_str()));

        write(
            &repo.join("caps/y/capability.yaml"),
            &cap_yaml("y", "1.0.0", "rule", json!({ "rule": { "body": "new" } })),
        );
        git(&["add", "-A"], &repo);
        git(&["commit", "-qm", "more"], &repo);
        let sha2 = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        // Re-resolve with NO lock: the branch must now be at the new commit.
        let r2 = profile::resolve(&profile, &wd, None).unwrap();
        assert_eq!(
            r2.lock.sources[0].rev.as_deref(),
            Some(sha2.as_str()),
            "an unlocked branch source follows the branch"
        );
        let ids: Vec<&str> = r2
            .capabilities
            .iter()
            .map(|c| c.manifest.id.as_str())
            .collect();
        assert!(ids.contains(&"y"), "the new capability is visible: {ids:?}");

        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A git source lends its capabilities a `<owner>/<repo>` namespace, so two
    /// authors' identically-named capabilities are distinct identities.
    #[test]
    fn a_git_source_namespaces_its_capabilities() {
        let (repo, _) = init_repo_with(
            "caps/mem-search/capability.yaml",
            &cap_yaml(
                "mem-search",
                "1.0.0",
                "skill",
                json!({ "skill": { "body": "theirs" } }),
            ),
        );
        let wd = tmp("ns");
        let profile = profile_from(json!({
            "name": "n", "harnesses": ["claude-code"],
            "sources": [{ "git": {
                "url": repo.to_string_lossy(), "rev": "HEAD", "subdir": "caps" } }],
        }));
        let r = profile::resolve(&profile, &wd, None).unwrap();

        let cap = &r.capabilities[0];
        assert_eq!(cap.manifest.id, "mem-search", "the bare id is the alias");
        let qualified = cap.qualified_name();
        assert!(
            qualified.ends_with("/mem-search") && qualified != "mem-search",
            "a git source namespaces the capability: {qualified}"
        );
        assert_eq!(r.lock.sources[0].capabilities[0].name, qualified);

        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&repo);
    }
}

// ---- lock digest + namespacing (0.2) --------------------------------------

/// Regression: the lock's per-capability digest must track *content*.
///
/// It used to be a hash of the capability's `capability.json` bytes, falling
/// back to the bare id when that file was absent — which is the normal case for
/// single-file capabilities, and precisely the shape you get when importing
/// someone else's repo. A skill's whole body could change and the lock would
/// not move, so the lockfile pinned nothing that mattered.
#[test]
fn the_lock_digest_tracks_single_file_capability_content() {
    let wd = tmp("digest");
    let skill = wd.join("caps/demo/SKILL.md");
    write(&skill, "---\ndescription: first\n---\n\nbody v1\n");

    let profile = profile_from(json!({
        "name": "d", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let first = profile::resolve(&profile, &wd, None).unwrap().lock.sources[0].capabilities[0]
        .digest
        .clone();
    assert!(first.starts_with("sha256:"), "a real tree digest: {first}");

    write(
        &skill,
        "---\ndescription: second\n---\n\nbody v2 totally changed\n",
    );
    let second = profile::resolve(&profile, &wd, None).unwrap().lock.sources[0].capabilities[0]
        .digest
        .clone();

    assert_ne!(first, second, "changing the body must change the digest");
    let _ = std::fs::remove_dir_all(&wd);
}

/// A local source is your own tree, so it lends no namespace and bare ids stay
/// bare — single-project authoring is unchanged by qualified names.
#[test]
fn a_local_source_leaves_ids_unqualified() {
    let wd = tmp("bare");
    write(
        &wd.join("caps/greet/capability.yaml"),
        &cap_yaml(
            "greet",
            "1.0.0",
            "skill",
            json!({ "skill": { "body": "hi" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "b", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert_eq!(r.capabilities[0].qualified_name(), "greet");
    assert_eq!(r.lock.sources[0].capabilities[0].name, "greet");
    let _ = std::fs::remove_dir_all(&wd);
}

/// An author's explicit `namespace:` outranks whatever the source implies.
#[test]
fn an_explicit_namespace_wins_over_the_source() {
    let wd = tmp("explicit-ns");
    write(
        &wd.join("caps/greet/capability.yaml"),
        &cap_yaml(
            "greet",
            "1.0.0",
            "skill",
            json!({ "namespace": "acme", "skill": { "body": "hi" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "e", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert_eq!(r.capabilities[0].qualified_name(), "acme/greet");
    let _ = std::fs::remove_dir_all(&wd);
}

/// Two namespaces may both provide `mem-search` — they are distinct identities
/// and both compose — but the emitted filenames use the bare id, so the clash
/// is surfaced at resolve time rather than discovered on disk.
#[test]
fn colliding_bare_ids_compose_but_are_reported() {
    let wd = tmp("collide");
    write(
        &wd.join("a/mem-search/capability.yaml"),
        &cap_yaml(
            "mem-search",
            "1.0.0",
            "skill",
            json!({ "namespace": "alice", "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("b/mem-search/capability.yaml"),
        &cap_yaml(
            "mem-search",
            "2.0.0",
            "skill",
            json!({ "namespace": "bob", "skill": { "body": "b" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "c", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "a" } }, { "local": { "path": "b" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();

    let names: Vec<String> = r.capabilities.iter().map(|c| c.qualified_name()).collect();
    assert!(
        names.contains(&"alice/mem-search".to_string())
            && names.contains(&"bob/mem-search".to_string()),
        "different namespaces are different capabilities: {names:?}"
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("bare id 'mem-search'")),
        "the on-disk path clash is reported: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- version requirements + relations (0.3 / 0.5) -------------------------

/// A satisfied requirement resolves quietly and pins the resolved version.
#[test]
fn a_satisfied_version_requirement_resolves_cleanly() {
    let wd = tmp("req-ok");
    write(
        &wd.join("caps/app/capability.yaml"),
        &cap_yaml(
            "app",
            "1.0.0",
            "skill",
            json!({ "dependencies": { "lib": "^1.2" }, "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/lib/capability.yaml"),
        &cap_yaml("lib", "1.5.0", "skill", json!({ "skill": { "body": "l" } })),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(
        !r.warnings.iter().any(|w| w.contains("requires")),
        "a satisfied requirement is silent: {:?}",
        r.warnings
    );
    let app = r.lock.sources[0]
        .capabilities
        .iter()
        .find(|c| c.id == "app")
        .unwrap();
    assert_eq!(
        app.dependencies.get("lib").map(String::as_str),
        Some("1.5.0")
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// An unsatisfied requirement names what was asked for and what is there.
#[test]
fn an_unsatisfied_version_requirement_is_reported_with_both_versions() {
    let wd = tmp("req-bad");
    write(
        &wd.join("caps/app/capability.yaml"),
        &cap_yaml(
            "app",
            "1.0.0",
            "skill",
            json!({ "dependencies": { "lib": "^2" }, "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/lib/capability.yaml"),
        &cap_yaml("lib", "1.5.0", "skill", json!({ "skill": { "body": "l" } })),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    let warning = r
        .warnings
        .iter()
        .find(|w| w.contains("requires 'lib'"))
        .unwrap_or_else(|| panic!("expected a requirement warning: {:?}", r.warnings));
    assert!(
        warning.contains("^2") && warning.contains("1.5.0"),
        "says what was asked and what is composed: {warning}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// Sources are a preference order, but a requirement can veto the preferred
/// candidate and push selection to a later source that satisfies it.
#[test]
fn a_requirement_vetoes_the_preferred_candidate() {
    let wd = tmp("veto");
    // The app requires lib ^2; the first source only offers 1.0.0.
    write(
        &wd.join("first/lib/capability.yaml"),
        &cap_yaml(
            "lib",
            "1.0.0",
            "skill",
            json!({ "skill": { "body": "old" } }),
        ),
    );
    write(
        &wd.join("second/lib/capability.yaml"),
        &cap_yaml(
            "lib",
            "2.3.0",
            "skill",
            json!({ "skill": { "body": "new" } }),
        ),
    );
    write(
        &wd.join("first/app/capability.yaml"),
        &cap_yaml(
            "app",
            "1.0.0",
            "skill",
            json!({ "dependencies": { "lib": "^2" }, "skill": { "body": "a" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "first" } }, { "local": { "path": "second" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    let lib = r
        .capabilities
        .iter()
        .find(|c| c.manifest.id == "lib")
        .unwrap();
    assert_eq!(
        lib.manifest.version, "2.3.0",
        "the later source wins because the earlier one is vetoed"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// When nothing on offer satisfies every dependent, say who asked for what —
/// the part of a solver people actually need.
#[test]
fn an_unsatisfiable_conflict_names_every_requirer() {
    let wd = tmp("conflict");
    write(
        &wd.join("caps/lib/capability.yaml"),
        &cap_yaml("lib", "1.0.0", "skill", json!({ "skill": { "body": "l" } })),
    );
    for (id, want) in [("app-a", "^1"), ("app-b", ">=2")] {
        write(
            &wd.join(format!("caps/{id}/capability.yaml")),
            &cap_yaml(
                id,
                "1.0.0",
                "skill",
                json!({ "dependencies": { "lib": want }, "skill": { "body": id } }),
            ),
        );
    }
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    let conflict = r
        .warnings
        .iter()
        .find(|w| w.starts_with("version conflict on 'lib'"))
        .unwrap_or_else(|| panic!("expected a conflict report: {:?}", r.warnings));
    assert!(
        conflict.contains("app-b requires >=2"),
        "names the requirer and its requirement: {conflict}"
    );
    assert!(
        conflict.contains("offers 1.0.0"),
        "names what was actually available: {conflict}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// Capabilities in one repo keep referring to each other by bare id, even
/// though the source namespaces them.
#[test]
fn a_sibling_dependency_resolves_by_bare_id_within_its_namespace() {
    let wd = tmp("sibling");
    write(
        &wd.join("caps/app/capability.yaml"),
        &cap_yaml(
            "app",
            "1.0.0",
            "skill",
            json!({ "namespace": "acme", "dependencies": ["lib"], "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/lib/capability.yaml"),
        &cap_yaml(
            "lib",
            "1.0.0",
            "skill",
            json!({ "namespace": "acme", "skill": { "body": "l" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(
        !r.warnings.iter().any(|w| w.contains("no source provides")),
        "a bare sibling id resolves inside its own namespace: {:?}",
        r.warnings
    );
    let app = r.lock.sources[0]
        .capabilities
        .iter()
        .find(|c| c.id == "app")
        .unwrap();
    assert!(
        app.dependencies.contains_key("acme/lib"),
        "and is pinned by its qualified name: {:?}",
        app.dependencies
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// An ambiguous bare id resolves to nothing rather than guessing between two
/// strangers' capabilities — the failure qualified names exist to prevent.
#[test]
fn an_ambiguous_bare_dependency_is_unmet_not_guessed() {
    let wd = tmp("ambiguous");
    for ns in ["alice", "bob"] {
        write(
            &wd.join(format!("caps/{ns}-lib/capability.yaml")),
            &cap_yaml(
                "lib",
                "1.0.0",
                "skill",
                json!({ "namespace": ns, "skill": { "body": ns } }),
            ),
        );
    }
    write(
        &wd.join("caps/app/capability.yaml"),
        &cap_yaml(
            "app",
            "1.0.0",
            "skill",
            json!({ "namespace": "carol", "dependencies": ["lib"], "skill": { "body": "a" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("requires 'lib'") && w.contains("no source provides")),
        "an ambiguous bare id is reported unmet: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// `suggests` is advice: an absent target is a note, and it never creates an
/// ordering edge (so it can never manufacture a cycle).
#[test]
fn a_suggestion_is_a_note_not_a_failure() {
    let wd = tmp("suggests");
    write(
        &wd.join("caps/app/capability.yaml"),
        &cap_yaml(
            "app",
            "1.0.0",
            "skill",
            json!({
                "dependencies": { "nice-to-have": { "relation": "suggests" } },
                "skill": { "body": "a" },
            }),
        ),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    let note = r
        .warnings
        .iter()
        .find(|w| w.contains("nice-to-have"))
        .unwrap_or_else(|| panic!("expected a note: {:?}", r.warnings));
    assert!(
        note.contains("suggests") && note.contains("optional"),
        "reported as optional, not as a failure: {note}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// `conflicts` matters more here than in a normal package manager: same-path
/// config merges deny-wins, so two contradictory policies silently produce the
/// strictest union.
#[test]
fn a_declared_conflict_between_two_composed_capabilities_is_reported() {
    let wd = tmp("conflicts");
    write(
        &wd.join("caps/strict/capability.yaml"),
        &cap_yaml(
            "strict",
            "1.0.0",
            "skill",
            json!({
                "dependencies": { "lax": { "relation": "conflicts" } },
                "skill": { "body": "s" },
            }),
        ),
    );
    write(
        &wd.join("caps/lax/capability.yaml"),
        &cap_yaml("lax", "1.0.0", "skill", json!({ "skill": { "body": "l" } })),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("conflict with 'lax'") && w.contains("deny-wins")),
        "a live conflict is reported with why it matters: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// A fork that `replaces` the original satisfies dependencies aimed at it.
#[test]
fn a_replacement_satisfies_dependencies_on_what_it_replaced() {
    let wd = tmp("replaces");
    write(
        &wd.join("caps/fork/capability.yaml"),
        &cap_yaml(
            "fork",
            "2.0.0",
            "skill",
            json!({
                "dependencies": { "acme/original": { "relation": "replaces" } },
                "skill": { "body": "f" },
            }),
        ),
    );
    write(
        &wd.join("caps/app/capability.yaml"),
        &cap_yaml(
            "app",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["acme/original"], "skill": { "body": "a" } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(
        !r.warnings.iter().any(|w| w.contains("no source provides")),
        "the replacement stands in for the original: {:?}",
        r.warnings
    );
    let app = r.lock.sources[0]
        .capabilities
        .iter()
        .find(|c| c.id == "app")
        .unwrap();
    assert!(
        app.dependencies.contains_key("fork"),
        "and the lock pins what it actually resolved to: {:?}",
        app.dependencies
    );
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- transitive acquisition (0.4) -----------------------------------------

mod transitive {
    use super::*;
    use open_harness::trust;

    /// A capability whose `requires` edge names where to fetch the dependency.
    fn app_pointing_at(dir: &Path) -> String {
        cap_yaml(
            "app",
            "1.0.0",
            "skill",
            json!({
                "dependencies": {
                    "lib": { "version": "*", "source": { "local": { "path": dir.to_string_lossy() } } }
                },
                "skill": { "body": "a" },
            }),
        )
    }

    /// A standalone directory holding one capability, optionally signed.
    fn lib_source(tag: &str, sign_with: Option<&trust::Keyfile>) -> PathBuf {
        let dir = tmp(tag);
        let cap = dir.join("lib");
        write(
            &cap.join("capability.yaml"),
            &cap_yaml("lib", "1.0.0", "skill", json!({ "skill": { "body": "l" } })),
        );
        if let Some(key) = sign_with {
            trust::sign(&cap, key).unwrap().write(&cap).unwrap();
        }
        dir
    }

    fn profile_with(wd: &Path, lib: &Path, extra: serde_json::Value) -> Profile {
        write(&wd.join("caps/app/capability.yaml"), &app_pointing_at(lib));
        let mut v = json!({
            "name": "t", "harnesses": ["claude-code"],
            "sources": [{ "local": { "path": "caps" } }],
        });
        let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) else {
            unreachable!()
        };
        for (k, val) in e {
            o.insert(k.clone(), val.clone());
        }
        profile_from(v)
    }

    /// Flat is the default: a declared source is never fetched behind your back.
    #[test]
    fn flat_resolution_never_fetches_a_declared_dependency_source() {
        let lib = lib_source("flat-lib", None);
        let wd = tmp("flat");
        let profile = profile_with(&wd, &lib, json!({}));

        let r = profile::resolve(&profile, &wd, None).unwrap();
        let ids: Vec<&str> = r
            .capabilities
            .iter()
            .map(|c| c.manifest.id.as_str())
            .collect();
        assert_eq!(ids, vec!["app"], "nothing was acquired: {ids:?}");
        assert!(
            r.warnings.iter().any(|w| w.contains("no source provides")),
            "the dependency is simply reported unmet: {:?}",
            r.warnings
        );
        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&lib);
    }

    /// Opting in without pinning a key acquires nothing — an empty trust store
    /// trusts nobody, and that is the correct default rather than a bug.
    #[test]
    fn transitive_refuses_unsigned_capabilities_by_default() {
        let lib = lib_source("unsigned-lib", None);
        let wd = tmp("unsigned");
        let profile = profile_with(&wd, &lib, json!({ "resolution": "transitive" }));

        let r = profile::resolve(&profile, &wd, None).unwrap();
        let ids: Vec<&str> = r
            .capabilities
            .iter()
            .map(|c| c.manifest.id.as_str())
            .collect();
        assert_eq!(ids, vec!["app"], "the unsigned capability is refused");
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("refused transitively acquired 'lib'")),
            "and the refusal says so: {:?}",
            r.warnings
        );
        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&lib);
    }

    /// A signature by a key in the trust store is what the default policy wants.
    #[test]
    fn transitive_admits_a_capability_signed_by_a_trusted_key() {
        let key = trust::generate_keyfile("alice").unwrap();
        let lib = lib_source("trusted-lib", Some(&key));
        let wd = tmp("trusted");

        let mut store = trust::TrustStore::default();
        store.add(&key.public_key, "alice");
        store.write(&wd.join("trust.yaml")).unwrap();

        let profile = profile_with(
            &wd,
            &lib,
            json!({ "resolution": "transitive", "trust": "trust.yaml" }),
        );
        let r = profile::resolve(&profile, &wd, None).unwrap();
        let ids: Vec<&str> = r
            .capabilities
            .iter()
            .map(|c| c.manifest.id.as_str())
            .collect();
        assert!(
            ids.contains(&"lib"),
            "a trusted signature is admitted: {ids:?} / {:?}",
            r.warnings
        );
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("acquired") && w.contains("transitively")),
            "and the acquisition is reported, never silent: {:?}",
            r.warnings
        );
        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&lib);
    }

    /// `tofu` admits an unknown *signer*, but the content is still verified.
    #[test]
    fn tofu_admits_an_unknown_signer_but_still_refuses_unsigned() {
        let key = trust::generate_keyfile("stranger").unwrap();
        let signed = lib_source("tofu-signed", Some(&key));
        let wd = tmp("tofu");
        let profile = profile_with(
            &wd,
            &signed,
            json!({ "resolution": "transitive", "transitive_trust": "tofu" }),
        );
        let r = profile::resolve(&profile, &wd, None).unwrap();
        assert!(
            r.capabilities.iter().any(|c| c.manifest.id == "lib"),
            "an unknown but valid signer passes tofu: {:?}",
            r.warnings
        );
        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&signed);

        // The same policy still refuses something carrying no signature at all.
        let unsigned = lib_source("tofu-unsigned", None);
        let wd2 = tmp("tofu2");
        let profile = profile_with(
            &wd2,
            &unsigned,
            json!({ "resolution": "transitive", "transitive_trust": "tofu" }),
        );
        let r = profile::resolve(&profile, &wd2, None).unwrap();
        assert!(
            !r.capabilities.iter().any(|c| c.manifest.id == "lib"),
            "tofu is not 'anything goes'"
        );
        let _ = std::fs::remove_dir_all(&wd2);
        let _ = std::fs::remove_dir_all(&unsigned);
    }

    /// Tampering is caught before the signer is even considered.
    #[test]
    fn transitive_refuses_a_capability_whose_content_was_tampered_with() {
        let key = trust::generate_keyfile("alice").unwrap();
        let lib = lib_source("tampered-lib", Some(&key));
        // Change the content after signing: the recomputed digest no longer
        // matches, so the signature is over something else.
        write(&lib.join("lib/extra.md"), "smuggled payload");

        let wd = tmp("tampered");
        let mut store = trust::TrustStore::default();
        store.add(&key.public_key, "alice");
        store.write(&wd.join("trust.yaml")).unwrap();

        let profile = profile_with(
            &wd,
            &lib,
            json!({ "resolution": "transitive", "trust": "trust.yaml" }),
        );
        let r = profile::resolve(&profile, &wd, None).unwrap();
        assert!(
            !r.capabilities.iter().any(|c| c.manifest.id == "lib"),
            "tampered content is refused even from a trusted key"
        );
        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&lib);
    }

    /// `any` is the escape hatch, and it says what it let through.
    #[test]
    fn transitive_trust_any_admits_unsigned_and_reports_it() {
        let lib = lib_source("any-lib", None);
        let wd = tmp("any");
        let profile = profile_with(
            &wd,
            &lib,
            json!({ "resolution": "transitive", "transitive_trust": "any" }),
        );
        let r = profile::resolve(&profile, &wd, None).unwrap();
        assert!(
            r.capabilities.iter().any(|c| c.manifest.id == "lib"),
            "any admits unsigned code: {:?}",
            r.warnings
        );
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("acquired") && w.contains("transitively")),
            "every acquisition is reported: {:?}",
            r.warnings
        );
        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&lib);
    }

    /// An acquired source never outranks one the profile named.
    #[test]
    fn declared_sources_keep_preference_over_acquired_ones() {
        let lib = lib_source("pref-lib", None);
        let wd = tmp("pref");
        // The profile's own source also provides `lib`, at a different version.
        write(
            &wd.join("caps/lib/capability.yaml"),
            &cap_yaml(
                "lib",
                "9.9.9",
                "skill",
                json!({ "skill": { "body": "mine" } }),
            ),
        );
        let profile = profile_with(
            &wd,
            &lib,
            json!({ "resolution": "transitive", "transitive_trust": "any" }),
        );
        let r = profile::resolve(&profile, &wd, None).unwrap();
        let lib_cap = r
            .capabilities
            .iter()
            .find(|c| c.manifest.id == "lib")
            .unwrap();
        assert_eq!(
            lib_cap.manifest.version, "9.9.9",
            "the profile's own source wins"
        );
        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&lib);
    }
}

/// "Present" is not "usable here": a dependency the composed set satisfies may
/// still be Unsupported on some of the harnesses the profile targets.
#[test]
fn a_dependency_unsupported_on_a_target_harness_is_reported() {
    let wd = tmp("per-harness");
    // A rule (Cline hosts rules) depending on a permission policy (Cline has no
    // committed allowlist, so the permission kind is Unsupported there).
    write(
        &wd.join("caps/app/capability.yaml"),
        &cap_yaml(
            "app",
            "1.0.0",
            "rule",
            json!({ "dependencies": ["policy"], "rule": { "body": "r", "globs": ["src/**"] } }),
        ),
    );
    write(
        &wd.join("caps/policy/capability.yaml"),
        &cap_yaml(
            "policy",
            "1.0.0",
            "permission",
            json!({ "permission": {
                "default": "ask",
                "rules": [{ "tool": "bash", "pattern": "git *", "verdict": "allow" }],
            } }),
        ),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code", "cline"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();

    let gap = r
        .warnings
        .iter()
        .find(|w| w.contains("Unsupported on"))
        .unwrap_or_else(|| panic!("expected a per-harness gap: {:?}", r.warnings));
    assert!(
        gap.contains("cline") && !gap.contains("claude-code"),
        "names only the harness that cannot host it: {gap}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// A lock keeps a branch pinned across upstream commits — that is what a lock is
/// for — but it must not also override the *profile*. Editing `rev:` has to win
/// over the pin, or the lock would silently serve the old revision forever.
#[cfg(unix)]
#[test]
fn changing_the_requested_rev_invalidates_the_pin() {
    use git_backed::*;

    let (repo, sha1) = init_repo_with(
        "caps/x/capability.yaml",
        &cap_yaml("x", "1.0.0", "skill", json!({ "skill": { "body": "v1" } })),
    );
    git(&["tag", "v1"], &repo);

    write(
        &repo.join("caps/x/capability.yaml"),
        &cap_yaml("x", "2.0.0", "skill", json!({ "skill": { "body": "v2" } })),
    );
    git(&["add", "-A"], &repo);
    git(&["commit", "-qm", "v2"], &repo);
    git(&["tag", "v2"], &repo);

    let wd = tmp("retag");
    let at = |rev: &str| {
        profile_from(json!({
            "name": "r", "harnesses": ["claude-code"],
            "sources": [{ "git": {
                "url": repo.to_string_lossy(), "rev": rev, "subdir": "caps" } }],
        }))
    };

    let lock1 = profile::resolve(&at("v1"), &wd, None).unwrap().lock;
    assert_eq!(lock1.sources[0].rev.as_deref(), Some(sha1.as_str()));
    assert_eq!(lock1.sources[0].requested.as_deref(), Some("v1"));

    // Same profile, same lock: the pin holds.
    let same = profile::resolve(&at("v1"), &wd, Some(&lock1)).unwrap();
    assert_eq!(same.lock.sources[0].rev.as_deref(), Some(sha1.as_str()));

    // Profile now asks for v2: the pin must give way.
    let moved = profile::resolve(&at("v2"), &wd, Some(&lock1)).unwrap();
    assert_ne!(
        moved.lock.sources[0].rev.as_deref(),
        Some(sha1.as_str()),
        "editing `rev:` overrides the lock's pin"
    );
    assert_eq!(moved.capabilities[0].manifest.version, "2.0.0");

    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&repo);
}
