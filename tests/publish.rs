//! Publishing tests (#21) — the producer half of `docs/src/distribution.md`.
//!
//! The property that matters most here is **determinism**: a capability's
//! archive is named by its own sha256, so the same input must always produce the
//! same bytes. If packing were sensitive to mtimes, ownership, or directory
//! iteration order, the content address would drift for reasons unrelated to the
//! content — and every consumer's pin would break on a re-publish.

use open_harness::manifest::discover;
use open_harness::{archive, profile, publish, trust};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-publish-{}-{tag}-{n}", std::process::id()));
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

/// A capabilities directory holding one skill.
fn caps_dir(root: &Path, id: &str, version: &str, body: &str) -> PathBuf {
    let dir = root.join("capabilities");
    write(
        &dir.join(id).join("capability.json"),
        &json!({
            "id": id, "name": id, "description": "x", "version": version,
            "kind": "skill", "skill": { "body": body },
        })
        .to_string(),
    );
    dir
}

fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

#[test]
fn packing_is_deterministic() {
    let wd = tmp("determ");
    let caps = caps_dir(&wd, "greeter", "1.0.0", "hi");
    let loaded = discover(&caps).unwrap();

    let a = publish::pack_capabilities(&loaded).unwrap();
    let b = publish::pack_capabilities(&loaded).unwrap();
    assert_eq!(
        a[0].digest, b[0].digest,
        "the same tree packs to the same digest"
    );
    assert_eq!(
        a[0].bytes, b[0].bytes,
        "byte-identical, not merely equal-digest"
    );

    // The digest must not depend on when the files were written: pack a
    // freshly-created identical tree and expect the same content address.
    let wd2 = tmp("determ2");
    let caps2 = caps_dir(&wd2, "greeter", "1.0.0", "hi");
    let loaded2 = discover(&caps2).unwrap();
    let c = publish::pack_capabilities(&loaded2).unwrap();
    assert_eq!(
        a[0].digest, c[0].digest,
        "identical content in a different directory packs identically"
    );

    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&wd2);
}

#[test]
fn the_index_pins_every_archive_by_digest() {
    let wd = tmp("index");
    let caps = caps_dir(&wd, "greeter", "2.1.0", "hi");
    let loaded = discover(&caps).unwrap();
    let packed = publish::pack_capabilities(&loaded).unwrap();

    let index: Value =
        serde_json::from_str(&publish::index_json(&packed, "https://cdn.example.com/")).unwrap();
    let entry = &index["capabilities"][0];
    assert_eq!(entry["name"], "greeter");
    assert_eq!(entry["version"], "2.1.0");
    let http = &entry["source"]["http"];
    assert_eq!(http["sha256"], packed[0].digest);
    assert_eq!(
        http["url"],
        format!(
            "https://cdn.example.com/blobs/sha256/{}.tar",
            packed[0].digest
        ),
        "a trailing slash on the base URL must not double up"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn the_published_tree_is_content_addressed() {
    let wd = tmp("tree");
    let caps = caps_dir(&wd, "greeter", "1.0.0", "hi");
    let loaded = discover(&caps).unwrap();
    let packed = publish::pack_capabilities(&loaded).unwrap();
    let out = wd.join("dist");
    let report = publish::write_tree(&packed, "https://cdn.example.com", &out).unwrap();

    let blob = out.join(format!("blobs/sha256/{}.tar", packed[0].digest));
    assert!(
        blob.is_file(),
        "the archive is stored at its content address"
    );
    assert_eq!(
        trust::sha256_hex(&std::fs::read(&blob).unwrap()),
        packed[0].digest,
        "the stored bytes hash to the name they are stored under"
    );
    assert!(report.index_path.is_file(), "registry.json is written");
    assert!(
        report.snapshot_path.is_file(),
        "an immutable snapshot is written alongside"
    );
    assert_eq!(
        std::fs::read_to_string(&report.index_path).unwrap(),
        std::fs::read_to_string(&report.snapshot_path).unwrap(),
        "the snapshot is exactly the catalog that was published"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn unsigned_capabilities_are_reported_not_hidden() {
    let wd = tmp("unsigned");
    let caps = caps_dir(&wd, "greeter", "1.0.0", "hi");
    let loaded = discover(&caps).unwrap();
    let packed = publish::pack_capabilities(&loaded).unwrap();
    assert!(!packed[0].signed);
    let report = publish::write_tree(&packed, "https://cdn.example.com", &wd.join("dist")).unwrap();
    assert_eq!(
        report.unsigned,
        vec!["greeter".to_string()],
        "a catalog does not quietly ship code nobody vouched for"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn a_signature_travels_inside_the_archive() {
    let wd = tmp("signed");
    let caps = caps_dir(&wd, "greeter", "1.0.0", "hi");
    let key = trust::generate_keyfile("tester").unwrap();
    let sig = trust::sign(&caps.join("greeter"), &key).unwrap();
    sig.write(&caps.join("greeter")).unwrap();

    let loaded = discover(&caps).unwrap();
    let packed = publish::pack_capabilities(&loaded).unwrap();
    assert!(packed[0].signed, "the signature is noticed");

    // Unpack and confirm the detached signature came along, so a consumer can
    // verify the author from the archive alone.
    let dest = wd.join("unpacked");
    archive::unpack_tar(&packed[0].bytes, &dest).unwrap();
    assert!(
        dest.join("greeter/capability.sig").is_file(),
        "capability.sig is carried into the archive"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn a_published_catalog_resolves_back_through_a_registry_source() {
    // The whole point: what `pack` emits is what `resolve` consumes.
    let wd = tmp("roundtrip");
    let caps = caps_dir(&wd, "greeter", "3.0.0", "hello from a blob");
    let loaded = discover(&caps).unwrap();
    let packed = publish::pack_capabilities(&loaded).unwrap();
    let cdn = wd.join("cdn");
    publish::write_tree(&packed, &file_url(&cdn), &cdn).unwrap();

    let consumer = tmp("consumer");
    let profile = profile::Profile::from_json(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "registry": {
                "index": file_url(&cdn.join("registry.json")), "name": "greeter" } }],
        })
        .to_string(),
    )
    .unwrap();

    let r = profile::resolve(&profile, &consumer, None).unwrap();
    let ids: Vec<&str> = r
        .capabilities
        .iter()
        .map(|c| c.manifest.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["greeter"],
        "the published catalog resolved: {:?}",
        r.warnings
    );
    assert_eq!(r.capabilities[0].manifest.version, "3.0.0");
    assert_eq!(
        r.lock.sources[0].rev.as_deref(),
        Some(format!("sha256:{}", packed[0].digest).as_str()),
        "the consumer pins exactly the digest the publisher produced"
    );

    let _ = std::fs::remove_dir_all(&wd);
    let _ = std::fs::remove_dir_all(&consumer);
}
