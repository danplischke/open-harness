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

/// Serve `body` on an ephemeral loopback port, returning the URL of `name`.
/// Exercises the real HTTP transport with no network egress, so it runs on every
/// platform (the same `TcpListener` trick as the MCP tests).
fn serve_bytes(body: Vec<u8>, content_type: &'static str, name: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf); // drain the request head
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/{name}")
}

/// Serve a registry index as `application/json`.
fn serve_index(body: String) -> String {
    serve_bytes(body.into_bytes(), "application/json", "registry.json")
}

/// Build an uncompressed tar archive from `(name, typeflag, data)` entries —
/// a minimal writer so the reader is tested against real archive bytes.
/// Typeflags: `b'0'` regular, `b'5'` directory, `b'2'` symlink.
fn tar(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
    fn put(h: &mut [u8; 512], off: usize, s: &str) {
        h[off..off + s.len()].copy_from_slice(s.as_bytes());
    }
    let mut out = Vec::new();
    for (name, typeflag, data) in entries {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        h[..nb.len()].copy_from_slice(nb);
        put(&mut h, 100, "0000644\0"); // mode
        put(&mut h, 108, "0000000\0"); // uid
        put(&mut h, 116, "0000000\0"); // gid
        put(&mut h, 124, &format!("{:011o}\0", data.len()));
        put(&mut h, 136, "00000000000\0"); // mtime
        h[156] = *typeflag;
        put(&mut h, 257, "ustar\0");
        put(&mut h, 263, "00");
        // The checksum is computed with its own field read as spaces.
        for b in h.iter_mut().skip(148).take(8) {
            *b = b' ';
        }
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        put(&mut h, 148, &format!("{sum:06o}\0 "));
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.resize(out.len() + (512 - data.len() % 512) % 512, 0);
    }
    out.resize(out.len() + 1024, 0); // two zero blocks terminate the archive
    out
}

/// A one-capability archive plus its sha256 — the common fixture.
fn cap_archive(id: &str, version: &str) -> (Vec<u8>, String) {
    let manifest = cap_json(id, version, "skill", json!({ "skill": { "body": "hi" } }));
    let bytes = tar(&[
        (&format!("{id}/"), b'5', b""),
        (&format!("{id}/capability.json"), b'0', manifest.as_bytes()),
    ]);
    let digest = open_harness::trust::sha256_hex(&bytes);
    (bytes, digest)
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

// ---- http archive sources (the CDN leaf) ----------------------------------

#[test]
fn http_source_unpacks_a_verified_archive() {
    let wd = tmp("http-ok");
    let (bytes, digest) = cap_archive("greeter", "2.1.0");
    let url = serve_bytes(bytes, "application/x-tar", "greeter.tar");

    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "http": { "url": url, "sha256": digest } }],
    }));
    let r = profile::resolve(&profile, &wd, None).unwrap();

    let ids: Vec<&str> = r
        .capabilities
        .iter()
        .map(|c| c.manifest.id.as_str())
        .collect();
    assert_eq!(ids, vec!["greeter"], "the archive's capability resolved");
    let src = &r.lock.sources[0];
    assert_eq!(src.kind, "http");
    assert_eq!(
        src.rev.as_deref(),
        Some(format!("sha256:{digest}").as_str()),
        "the lock pins the archive's content digest"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn http_source_reads_a_file_url_and_caches_by_digest() {
    let wd = tmp("http-file");
    let (bytes, digest) = cap_archive("greeter", "1.0.0");
    let archive = wd.join("greeter.tar");
    std::fs::write(&archive, &bytes).unwrap();

    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "http": { "url": file_url(&archive), "sha256": digest } }],
    }));
    let first = profile::resolve(&profile, &wd, None).unwrap();
    assert_eq!(first.capabilities.len(), 1);

    // Content-addressed: once unpacked, the artifact is never re-fetched. With
    // the source gone, a re-resolve still succeeds from the cache.
    std::fs::remove_file(&archive).unwrap();
    let second = profile::resolve(&profile, &wd, None).unwrap();
    assert_eq!(
        second.capabilities.len(),
        1,
        "a cached archive resolves without the source: {:?}",
        second.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn http_archive_digest_mismatch_stops_the_resolve() {
    let wd = tmp("http-tamper");
    let (bytes, _real) = cap_archive("greeter", "1.0.0");
    let archive = wd.join("greeter.tar");
    std::fs::write(&archive, &bytes).unwrap();
    let wrong = "0".repeat(64);

    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "http": { "url": file_url(&archive), "sha256": wrong } }],
    }));
    // Tamper evidence is fatal, not a warning someone can scroll past.
    let Err(err) = profile::resolve(&profile, &wd, None) else {
        panic!("a digest mismatch must fail");
    };
    assert!(
        err.contains("digest mismatch"),
        "the error names the mismatch: {err}"
    );
    assert!(
        !wd.join(".open-harness/cache/http").join(&wrong).exists(),
        "nothing is unpacked when the digest does not match"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn http_archive_with_a_traversal_entry_is_refused() {
    let wd = tmp("http-slip");
    let bytes = tar(&[("../escaped.json", b'0', b"{}")]);
    let digest = open_harness::trust::sha256_hex(&bytes);
    let archive = wd.join("evil.tar");
    std::fs::write(&archive, &bytes).unwrap();

    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "http": { "url": file_url(&archive), "sha256": digest } }],
    }));
    let Err(err) = profile::resolve(&profile, &wd, None) else {
        panic!("a traversal entry must fail");
    };
    assert!(
        err.contains("escapes the archive root"),
        "the refusal names the escape: {err}"
    );
    assert!(
        !wd.join(".open-harness/cache/escaped.json").exists()
            && !wd.join(".open-harness/cache/http/escaped.json").exists(),
        "the entry is never written outside the destination"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn http_archive_with_a_symlink_is_refused() {
    let wd = tmp("http-link");
    // A symlink is a write-through escape even when its own name looks tame.
    let bytes = tar(&[("link", b'2', b"")]);
    let digest = open_harness::trust::sha256_hex(&bytes);
    let archive = wd.join("link.tar");
    std::fs::write(&archive, &bytes).unwrap();

    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "http": { "url": file_url(&archive), "sha256": digest } }],
    }));
    let Err(err) = profile::resolve(&profile, &wd, None) else {
        panic!("a link entry must fail");
    };
    assert!(err.contains("link"), "the refusal names the link: {err}");
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn http_archive_that_is_compressed_is_refused_honestly() {
    let wd = tmp("http-gz");
    // gzip magic — a compressed stream must be named, not mis-read as tar.
    let bytes = vec![0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00];
    let digest = open_harness::trust::sha256_hex(&bytes);
    let archive = wd.join("c.tar.gz");
    std::fs::write(&archive, &bytes).unwrap();

    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "http": { "url": file_url(&archive), "sha256": digest } }],
    }));
    let Err(err) = profile::resolve(&profile, &wd, None) else {
        panic!("a compressed archive must fail");
    };
    assert!(
        err.contains("gzip") && err.contains("uncompressed"),
        "the refusal names the format and the remedy: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn http_source_requires_a_pinned_digest() {
    // An unpinned remote artifact would be trust-by-URL; the profile cannot
    // even express it.
    let err = Profile::from_json(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "http": { "url": "https://cdn.example.com/x.tar" } }],
        })
        .to_string(),
    )
    .expect_err("an http source without `sha256` must be rejected");
    assert!(
        err.contains("sha256"),
        "the error names the missing pin: {err}"
    );
}

#[test]
fn http_source_with_a_malformed_digest_is_rejected() {
    let wd = tmp("http-baddigest");
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "http": { "url": "file:///nope.tar", "sha256": "abc" } }],
    }));
    let Err(err) = profile::resolve(&profile, &wd, None) else {
        panic!("a short digest must fail");
    };
    assert!(
        err.contains("64 hex characters"),
        "the error explains the expected form: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn unreachable_archive_is_warned_not_silently_skipped() {
    let wd = tmp("http-gone");
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "http": { "url": "file:///definitely/not/here.tar", "sha256": "a".repeat(64) } }],
    }));
    // A transport failure is recoverable — warn and compose the rest.
    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert!(r.capabilities.is_empty());
    assert!(
        r.warnings.iter().any(|w| w.contains("http source")),
        "an unreachable archive is loudly reported: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn remote_index_may_point_at_an_http_archive() {
    let wd = tmp("registry-http");
    let (bytes, digest) = cap_archive("greeter", "3.0.0");
    let archive_url = serve_bytes(bytes, "application/x-tar", "greeter.tar");
    // Unlike a `local` entry, a fetchable digest-pinned entry from a remote
    // index is exactly what a CDN catalog is for.
    let index_url = serve_index(
        json!({
            "capabilities": [
                { "name": "greeter-pack", "version": "3.0.0",
                  "source": { "http": { "url": archive_url, "sha256": digest } } }
            ]
        })
        .to_string(),
    );

    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "registry": { "index": index_url, "name": "greeter-pack" } }],
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
        "a remote index resolved a CDN archive: {:?}",
        r.warnings
    );
    let src = &r.lock.sources[0];
    assert_eq!(src.kind, "registry", "registry provenance is kept");
    assert_eq!(
        src.rev.as_deref(),
        Some(format!("sha256:{digest}").as_str()),
        "the archive's digest is still pinned through the indirection"
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
// ---- source inference + YAML profiles --------------------------------------
#[test]
fn a_source_kind_is_inferred_from_its_spec() {
    use open_harness::profile::Source;

    let cases = [
        ("git+https://github.com/acme/caps@v1", "git"),
        ("ssh://git@github.com/acme/caps", "git"),
        ("git@github.com:acme/caps.git", "git"),
        ("https://github.com/acme/caps.git", "git"),
        // A forge URL with no other signal is a repository.
        ("https://github.com/acme/caps", "git"),
        ("https://cdn.example.com/b.tar#sha256=abc123", "http"),
        (
            "https://cdn.example.com/registry.json#name=commit-style",
            "registry",
        ),
        ("capabilities", "local"),
        ("./caps/sub", "local"),
    ];
    for (spec, want) in cases {
        let got = match Source::parse_spec(spec).unwrap_or_else(|e| panic!("{spec}: {e}")) {
            Source::Git(_) => "git",
            Source::Http(_) => "http",
            Source::Registry(_) => "registry",
            Source::Local(_) => "local",
        };
        assert_eq!(got, want, "spec '{spec}' should infer as {want}");
    }
}

#[test]
fn an_unpinnable_spec_is_refused_with_the_fix() {
    use open_harness::profile::Source;

    // An archive without a digest cannot be verified, and a registry index
    // without a name selects nothing — say so instead of guessing.
    let err = Source::parse_spec("https://cdn.example.com/pack.tar").unwrap_err();
    assert!(err.contains("#sha256="), "names the missing pin: {err}");
    let err = Source::parse_spec("https://cdn.example.com/registry.json").unwrap_err();
    assert!(err.contains("#name="), "names the missing selector: {err}");
}

#[test]
fn a_local_spec_is_not_resolved_against_the_process_directory() {
    use open_harness::profile::Source;
    // A `local` path is relative to the *profile's* workdir, so parsing must not
    // test it against wherever the process happens to be running.
    let Source::Local(l) = Source::parse_spec("definitely/not/here").unwrap() else {
        panic!("a plain path is a local source even when absent");
    };
    assert_eq!(l.path, "definitely/not/here");
}

#[test]
fn a_yaml_profile_round_trips_through_string_sources() {
    let wd = tmp("yaml");
    write(
        &wd.join("caps/greet/capability.json"),
        &cap_json(
            "greet",
            "1.0.0",
            "skill",
            json!({ "skill": { "body": "hi" } }),
        ),
    );
    let text = "\
name: my-setup
harnesses:
  - claude-code
  - cursor
sources:
  - caps
";
    let profile = Profile::from_yaml(text).expect("a YAML profile parses");
    assert_eq!(profile.name, "my-setup");
    assert_eq!(profile.harnesses, vec!["claude-code", "cursor"]);

    let r = profile::resolve(&profile, &wd, None).unwrap();
    assert_eq!(r.capabilities.len(), 1, "{:?}", r.warnings);
    assert_eq!(r.lock.sources[0].kind, "local");
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn a_profile_is_found_without_being_named() {
    let wd = tmp("find");
    assert_eq!(profile::find_profile(&wd), None);
    write(&wd.join("open-harness.json"), "{}");
    assert_eq!(
        profile::find_profile(&wd),
        Some(wd.join("open-harness.json"))
    );
    // A YAML profile wins over the legacy JSON name.
    write(&wd.join("oh.yaml"), "name: x\n");
    assert_eq!(profile::find_profile(&wd), Some(wd.join("oh.yaml")));
    assert_eq!(
        profile::all_profiles(&wd).len(),
        2,
        "both are reported, so an ambiguous project can be flagged"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn yaml_emission_round_trips_a_profile() {
    let value = json!({
        "name": "p",
        "harnesses": ["claude-code"],
        "sources": ["capabilities", { "git": { "url": "https://h/r", "rev": "v1" } }],
    });
    let text =
        open_harness::yaml::to_yaml_document_ordered(&value, &["name", "harnesses", "sources"]);
    assert!(
        text.starts_with("name: p\n"),
        "a profile reads in written order, not alphabetical: {text}"
    );
    let back = Profile::from_yaml(&text).expect("emitted YAML parses back");
    assert_eq!(back.name, "p");
    assert_eq!(back.sources.len(), 2);
}

// ---- pip-style git specs ---------------------------------------------------
#[test]
fn git_specs_parse_like_a_pip_requirement() {
    use open_harness::profile::GitSource;

    let g = GitSource::parse_spec("git+https://github.com/acme/caps@v1.2.0#subdirectory=caps/x")
        .unwrap();
    assert_eq!(g.url, "https://github.com/acme/caps");
    assert_eq!(g.rev, "v1.2.0");
    assert_eq!(g.subdir.as_deref(), Some("caps/x"));

    // The `git+` prefix, the rev, and the fragment are all optional.
    let plain = GitSource::parse_spec("https://github.com/acme/caps.git").unwrap();
    assert_eq!(plain.url, "https://github.com/acme/caps.git");
    assert_eq!(plain.rev, "HEAD", "an unpinned spec defaults to HEAD");
    assert_eq!(plain.subdir, None);

    // A bare fragment is accepted alongside pip's `subdirectory=` spelling.
    assert_eq!(
        GitSource::parse_spec("git+https://h/r#caps/x")
            .unwrap()
            .subdir
            .as_deref(),
        Some("caps/x")
    );
    // A local path is a valid repo location too.
    let local = GitSource::parse_spec("/srv/mirrors/caps@v1.0.0").unwrap();
    assert_eq!(local.url, "/srv/mirrors/caps");
    assert_eq!(local.rev, "v1.0.0");
}

#[test]
fn an_ssh_spec_keeps_its_userinfo_instead_of_reading_it_as_a_revision() {
    use open_harness::profile::GitSource;

    // `git@` is userinfo, not a rev: splitting on the last `@` naively would
    // turn this into url `ssh://git` at rev `github.com/acme/caps`.
    let g = GitSource::parse_spec("git+ssh://git@github.com/acme/caps").unwrap();
    assert_eq!(g.url, "ssh://git@github.com/acme/caps");
    assert_eq!(g.rev, "HEAD");

    // …and with a rev, both are recovered.
    let pinned = GitSource::parse_spec("git+ssh://git@github.com/acme/caps@main").unwrap();
    assert_eq!(pinned.url, "ssh://git@github.com/acme/caps");
    assert_eq!(pinned.rev, "main");

    // scp-style shorthand has no scheme and no slash before the userinfo.
    let scp = GitSource::parse_spec("git@github.com:acme/caps.git").unwrap();
    assert_eq!(scp.url, "git@github.com:acme/caps.git");
    assert_eq!(scp.rev, "HEAD");
}

#[test]
fn a_malformed_git_spec_is_refused() {
    use open_harness::profile::GitSource;
    assert!(GitSource::parse_spec("").is_err());
    assert!(
        GitSource::parse_spec("https://h/r@").is_err(),
        "an empty revision is a typo, not a default"
    );
}

#[test]
fn a_git_source_may_be_written_as_a_spec_string() {
    let profile = profile_from(json!({
        "name": "p", "harnesses": ["claude-code"],
        "sources": [{ "git": "git+https://github.com/acme/caps@v2#subdirectory=capabilities" }],
    }));
    let json_back = serde_json::to_value(&profile.sources[0]).unwrap();
    assert_eq!(json_back["git"]["url"], "https://github.com/acme/caps");
    assert_eq!(json_back["git"]["rev"], "v2");
    assert_eq!(json_back["git"]["subdir"], "capabilities");
}

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
    fn a_repo_that_is_one_capability_installs_like_a_package() {
        // The shape you get from `pip install git+https://github.com/me/my-skill`:
        // the repository *is* the package, with its manifest at the root rather
        // than in a `capabilities/` container.
        let (repo, _) = init_repo_with(
            "SKILL.md",
            "---\nname: commit-style\ndescription: a whole repo, one capability\n---\nbody\n",
        );
        let wd = tmp("rootcap");
        let profile = profile_from(json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "git": format!("git+{}", repo.display()) }],
        }));

        let r = profile::resolve(&profile, &wd, None).unwrap();
        assert_eq!(
            r.capabilities.len(),
            1,
            "a repo whose root is the capability resolves, rather than silently \
             finding nothing: {:?}",
            r.warnings
        );
        // The checkout lives under a cache slug, so the id must come from the
        // repository's name — not from the directory the clone happens to land in.
        let repo_name = repo.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(r.capabilities[0].manifest.id, repo_name);
        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_spec_pins_a_tag_from_the_repo() {
        let (repo, first) = init_repo_with(
            "capabilities/greet/capability.json",
            &cap_json(
                "greet",
                "1.0.0",
                "skill",
                json!({ "skill": { "body": "v1" } }),
            ),
        );
        git(&["tag", "v1.0.0"], &repo);
        // Move the branch on, so resolving the tag proves the pin is honored.
        write(
            &repo.join("capabilities/greet/capability.json"),
            &cap_json(
                "greet",
                "2.0.0",
                "skill",
                json!({ "skill": { "body": "v2" } }),
            ),
        );
        git(&["add", "-A"], &repo);
        git(&["commit", "-qm", "v2"], &repo);

        let wd = tmp("tagpin");
        let profile = profile_from(json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "git": format!("git+{}@v1.0.0#subdirectory=capabilities", repo.display()) }],
        }));
        let r = profile::resolve(&profile, &wd, None).unwrap();
        assert_eq!(
            r.capabilities[0].manifest.version, "1.0.0",
            "the tag in the spec selected the older commit"
        );
        assert_eq!(r.lock.sources[0].rev.as_deref(), Some(first.as_str()));
        assert_eq!(r.lock.sources[0].subdir.as_deref(), Some("capabilities"));
        let _ = std::fs::remove_dir_all(&wd);
        let _ = std::fs::remove_dir_all(&repo);
    }

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
