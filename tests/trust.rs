//! Trust & signing tests (#17): sign → verify, tamper detection, trust levels,
//! and the advisory permission policy.

use open_harness::manifest::{PermissionPolicy, Permissions};
use open_harness::trust::{self, Verification};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-trust-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// A capability dir with a manifest (declaring permissions) and an asset file.
fn make_cap(dir: &Path) {
    write(
        &dir.join("capability.json"),
        r#"{ "id": "web", "kind": "tool", "permissions": { "network": ["*"], "exec": ["curl"] } }"#,
    );
    write(&dir.join("body.md"), "capability asset contents");
}

#[test]
fn sign_then_verify_trusted() {
    let dir = tmp("trusted");
    make_cap(&dir);
    let key = trust::generate_keyfile("alice").unwrap();
    let sig = trust::sign(&dir, &key).unwrap();
    sig.write(&dir).unwrap();

    let mut store = trust::TrustStore::default();
    assert!(store.add(&key.public_key, "alice"));

    let v = trust::verify(&dir, &store);
    assert!(matches!(v, Verification::Trusted { .. }), "got {v:?}");
    assert!(
        v.passes(true),
        "a trusted signature passes even in strict mode"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The headline acceptance: a tampered capability is rejected on verify.
#[test]
fn tampered_capability_is_rejected() {
    let dir = tmp("tamper");
    make_cap(&dir);
    let key = trust::generate_keyfile("alice").unwrap();
    trust::sign(&dir, &key).unwrap().write(&dir).unwrap();

    // Modify a file after signing.
    write(&dir.join("body.md"), "MALICIOUS PAYLOAD");

    let mut store = trust::TrustStore::default();
    store.add(&key.public_key, "alice");
    let v = trust::verify(&dir, &store);
    assert!(
        matches!(v, Verification::Invalid(ref r) if r.contains("tampered")),
        "tampering must be Invalid, got {v:?}"
    );
    assert!(!v.passes(false), "Invalid never passes, even in lax mode");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_forged_signature_is_rejected() {
    let dir = tmp("forged");
    make_cap(&dir);
    let key = trust::generate_keyfile("alice").unwrap();
    let sig = trust::sign(&dir, &key).unwrap();

    // Corrupt the signature bytes (still valid hex, wrong value); content is
    // untouched so the digest matches but the ed25519 check must fail.
    let mut bad = sig.clone();
    let flip = if sig.signature.starts_with('a') {
        'b'
    } else {
        'a'
    };
    bad.signature.replace_range(0..1, &flip.to_string());
    bad.write(&dir).unwrap();

    let v = trust::verify(&dir, &trust::TrustStore::default());
    assert!(matches!(v, Verification::Invalid(_)), "got {v:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_valid_signature_by_an_unknown_key_is_untrusted() {
    let dir = tmp("untrusted");
    make_cap(&dir);
    let key = trust::generate_keyfile("stranger").unwrap();
    trust::sign(&dir, &key).unwrap().write(&dir).unwrap();

    // Empty trust store: the signature is valid, but the key is unknown.
    let v = trust::verify(&dir, &trust::TrustStore::default());
    assert!(matches!(v, Verification::Untrusted { .. }), "got {v:?}");
    assert!(
        v.passes(false),
        "untrusted passes when signatures aren't required"
    );
    assert!(!v.passes(true), "untrusted fails under --require-signed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unsigned_capability_is_unsigned() {
    let dir = tmp("unsigned");
    make_cap(&dir);
    let v = trust::verify(&dir, &trust::TrustStore::default());
    assert_eq!(v, Verification::Unsigned);
    assert!(v.passes(false));
    assert!(!v.passes(true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn digest_is_deterministic_and_content_sensitive() {
    let dir = tmp("digest");
    make_cap(&dir);
    let d1 = trust::capability_digest(&dir).unwrap();
    let d2 = trust::capability_digest(&dir).unwrap();
    assert_eq!(d1, d2, "same tree → same digest");
    assert!(d1.starts_with("sha256:"));

    write(&dir.join("new-asset.txt"), "another file");
    assert_ne!(
        trust::capability_digest(&dir).unwrap(),
        d1,
        "adding a file changes the digest"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn signature_file_is_excluded_from_the_digest() {
    // Digest before and after writing capability.sig must match, so signing
    // then verifying is stable (the sig doesn't hash itself).
    let dir = tmp("selfexcl");
    make_cap(&dir);
    let before = trust::capability_digest(&dir).unwrap();
    let key = trust::generate_keyfile("a").unwrap();
    trust::sign(&dir, &key).unwrap().write(&dir).unwrap();
    let after = trust::capability_digest(&dir).unwrap();
    assert_eq!(before, after);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn permission_policy_flags_denied_capabilities() {
    let perms = Permissions {
        read: vec![],
        exec: vec!["curl".into()],
        network: vec!["*".into()],
    };
    let deny_net = PermissionPolicy {
        allow_network: false,
        allow_exec: true,
    };
    let violations = trust::permission_violations(&perms, &deny_net);
    assert!(
        violations.iter().any(|v| v.contains("network")),
        "denied network must be flagged: {violations:?}"
    );

    let allow_all = PermissionPolicy::default();
    assert!(
        trust::permission_violations(&perms, &allow_all).is_empty(),
        "default policy permits everything"
    );
}

#[test]
fn keyfile_and_trust_store_round_trip() {
    let dir = tmp("keys");
    let key = trust::generate_keyfile("me").unwrap();
    let kf = dir.join("openharness.key");
    key.write(&kf).unwrap();
    let loaded = trust::Keyfile::load(&kf).unwrap();
    assert_eq!(loaded.public_key, key.public_key);
    assert_eq!(loaded.algorithm, "ed25519");

    let mut store = trust::TrustStore::default();
    store.add(&key.public_key, "me");
    assert!(!store.add(&key.public_key, "me"), "adding twice is a no-op");
    let sf = dir.join("trust.json");
    store.write(&sf).unwrap();
    let reloaded = trust::TrustStore::load(&sf).unwrap();
    assert!(reloaded.contains(&key.public_key));
    let _ = std::fs::remove_dir_all(&dir);
}
