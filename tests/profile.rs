//! Profile / sources / lockfile tests (#15).
//!
//! The resolver itself is platform-independent; the git-backed tests spawn the
//! system `git` against a throwaway *local* repo (no network), and are
//! `#[cfg(unix)]` only to avoid exercising temp-repo fixture wrangling on a
//! Windows runner we can't observe — git's behavior is identical either way.

use open_harness::adapters::Harness;
use open_harness::profile::{self, Lock, Profile};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

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
    Profile::from_json(&v.to_string()).expect("valid profile")
}

/// Serve `body` as `application/json` on an ephemeral loopback port, returning
/// the index URL. Exercises the real HTTP transport with no network egress, so
/// it runs on every platform (the same `TcpListener` trick as the MCP tests).
fn serve_index(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf); // drain the request head
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/registry.json")
}

/// A `file://` URL for `path` (absolute paths only, as the loader expects).
fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}") // Windows drive path: file:///C:/…
    }
}

fn cap_json(id: &str, version: &str, kind: &str, extra: serde_json::Value) -> String {
    let mut m = json!({
        "id": id, "name": id, "description": "x", "version": version, "kind": kind,
    });
    if let (Some(mo), Some(eo)) = (m.as_object_mut(), extra.as_object()) {
        for (k, val) in eo {
            mo.insert(k.clone(), val.clone());
        }
    }
    m.to_string()
}

// ---- cross-platform: resolver, lockfile, warnings -------------------------

#[test]
fn local_only_profile_resolves_and_locks() {
    let wd = tmp("local");
    write(
        &wd.join("caps/greet/capability.json"),
        &cap_json(
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
        &wd.join("caps/a/capability.json"),
        &cap_json("a", "0.1.0", "rule", json!({ "rule": { "body": "x" } })),
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
fn lock_round_trips_through_json() {
    let wd = tmp("lockrt");
    write(
        &wd.join("caps/a/capability.json"),
        &cap_json("a", "2.0.0", "skill", json!({ "skill": { "body": "y" } })),
    );
    let profile = profile_from(json!({
        "name": "rt", "harnesses": ["claude-code"],
        "sources": [{ "local": { "path": "caps" } }],
    }));
    let lock = profile::resolve(&profile, &wd, None).unwrap().lock;
    let round = Lock::from_json(&lock.to_json()).unwrap();
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
        &wd.join("packs/greeter/capability.json"),
        &cap_json(
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

// ---- registry index: local path, file:// URL, or fetched over http --------

#[test]
fn registry_index_resolves_through_a_file_url() {
    let wd = tmp("registry-file-url");
    write(
        &wd.join("packs/greeter/capability.json"),
        &cap_json(
            "greeter",
            "2.1.0",
            "skill",
            json!({ "skill": { "body": "hi" } }),
        ),
    );
    let index = wd.join("registry.json");
    write(
        &index,
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
        "sources": [{ "registry": { "index": file_url(&index), "name": "greeter-pack" } }],
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
        "a file:// index resolves like a bare path: {ids:?} ({:?})",
        r.warnings
    );
    assert_eq!(r.lock.sources[0].kind, "registry");
    let _ = std::fs::remove_dir_all(&wd);
}

/// The index is fetched over HTTP and parsed — proven by the resolver finding
/// the named entry. A *remote* index may not select a path on this machine, so
/// that entry is refused loudly rather than read.
#[test]
fn remote_index_is_fetched_and_a_local_entry_is_refused() {
    let wd = tmp("registry-http-local");
    let url = serve_index(
        json!({
            "capabilities": [
                { "name": "greeter-pack", "source": { "local": { "path": "packs" } } }
            ]
        })
        .to_string(),
    );
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "registry": { "index": url, "name": "greeter-pack" } }],
    }));

    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(r.capabilities.is_empty());
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("refused") && w.contains("greeter-pack")),
        "a remote index must not select local paths: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn unreachable_remote_index_is_warned_not_silently_skipped() {
    let wd = tmp("registry-http-down");
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        // Port 1: nothing listens, so the fetch fails fast.
        "sources": [{ "registry": { "index": "http://127.0.0.1:1/registry.json", "name": "x" } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(r.capabilities.is_empty());
    assert!(
        r.warnings.iter().any(|w| w.contains("registry index")),
        "an unreachable index is loudly reported: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn missing_dependency_is_warned() {
    let wd = tmp("deps");
    write(
        &wd.join("caps/a/capability.json"),
        &cap_json(
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
        &wd.join("caps/a/capability.json"),
        &cap_json(
            "a",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["b"], "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/b/capability.json"),
        &cap_json(
            "b",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["c"], "skill": { "body": "b" } }),
        ),
    );
    write(
        &wd.join("caps/c/capability.json"),
        &cap_json("c", "1.0.0", "skill", json!({ "skill": { "body": "c" } })),
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
        &wd.join("caps/a/capability.json"),
        &cap_json(
            "a",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["b"], "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/b/capability.json"),
        &cap_json(
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
        &wd.join("caps/a/capability.json"),
        &cap_json(
            "a",
            "1.0.0",
            "skill",
            json!({ "dependencies": ["b"], "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/b/capability.json"),
        &cap_json("b", "1.0.0", "skill", json!({ "skill": { "body": "b" } })),
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
        vec!["b".to_string()],
        "the lock pins the dependency graph"
    );
    assert_eq!(
        lock,
        Lock::from_json(&lock.to_json()).unwrap(),
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
    use super::*;
    use std::process::Command;

    fn git(args: &[&str], cwd: &Path) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git available")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo_with(cap_rel: &str, cap: &str) -> (PathBuf, String) {
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
            "caps/conv/capability.json",
            &cap_json(
                "conv",
                "0.4.1",
                "rule",
                json!({ "rule": { "body": "tabs" } }),
            ),
        );
        let wd = tmp("compose");
        write(
            &wd.join("local/greet/capability.json"),
            &cap_json(
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
            "caps/x/capability.json",
            &cap_json("x", "1.0.0", "skill", json!({ "skill": { "body": "v1" } })),
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
            &repo.join("caps/y/capability.json"),
            &cap_json("y", "1.0.0", "rule", json!({ "rule": { "body": "new" } })),
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

    /// Acceptance for the CDN-registry shape: the index is *fetched over HTTP*
    /// and its entry resolves to real capability bytes from a git source — the
    /// full indirection, end to end, with no network egress.
    #[test]
    fn remote_index_resolves_a_git_entry() {
        let (repo, sha) = init_repo_with(
            "caps/conv/capability.json",
            &cap_json(
                "conv",
                "0.4.1",
                "rule",
                json!({ "rule": { "body": "tabs" } }),
            ),
        );
        let wd = tmp("registry-http-git");
        let url = serve_index(
            json!({
                "capabilities": [{
                    "name": "conv-pack",
                    "version": "0.4.1",
                    "source": { "git": { "url": repo.to_string_lossy(), "rev": "HEAD", "subdir": "caps" } }
                }]
            })
            .to_string(),
        );
        let profile = profile_from(json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "registry": { "index": url, "name": "conv-pack" } }],
        }));

        let r = profile::resolve(&profile, &wd, None).unwrap();
        let ids: Vec<&str> = r
            .capabilities
            .iter()
            .map(|c| c.manifest.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["conv"],
            "the fetched index resolved its git entry: {ids:?} ({:?})",
            r.warnings
        );
        let src = &r.lock.sources[0];
        assert_eq!(src.kind, "registry", "provenance stays registry");
        assert_eq!(
            src.rev.as_deref(),
            Some(sha.as_str()),
            "the leaf git commit is still pinned through the registry"
        );
        assert!(
            src.origin.contains("conv-pack") && src.origin.contains("http://"),
            "the remote index + name is the recorded origin: {}",
            src.origin
        );

        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&repo);
    }
}
