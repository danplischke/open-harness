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
        &dir.join("capability.yaml"),
        "id: web\nkind: tool\npermissions:\n  network: [\"*\"]\n  exec: [curl]\n",
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

/// Revocation (#22): a withdrawn key is rejected, even in lax mode, and even if
/// it is still (stale) in the trusted list.
#[test]
fn a_revoked_signer_is_rejected_even_in_lax_mode() {
    let dir = tmp("revoked");
    make_cap(&dir);
    let key = trust::generate_keyfile("alice").unwrap();
    trust::sign(&dir, &key).unwrap().write(&dir).unwrap();

    let mut store = trust::TrustStore::default();
    store.add(&key.public_key, "alice");
    assert!(
        matches!(trust::verify(&dir, &store), Verification::Trusted { .. }),
        "trusted before revocation"
    );

    assert!(store.revoke(&key.public_key, "key compromised"));
    let v = trust::verify(&dir, &store);
    assert!(
        matches!(v, Verification::Revoked { ref reason, .. } if reason.contains("compromised")),
        "got {v:?}"
    );
    assert!(
        !v.passes(false),
        "a revoked key never passes, even in lax mode"
    );
    assert!(!v.passes(true));
    assert!(
        !store.contains(&key.public_key),
        "revoke() drops the key from the trusted list"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revocation_overrides_a_stale_trusted_entry() {
    let dir = tmp("revwins");
    make_cap(&dir);
    let key = trust::generate_keyfile("alice").unwrap();
    trust::sign(&dir, &key).unwrap().write(&dir).unwrap();

    // A store where the key is BOTH trusted and revoked (a stale trusted entry).
    // Revocation is consulted first, so it must win.
    let store = trust::TrustStore::from_text(&format!(
        r#"{{ "keys": [{{"public_key":"{pk}","label":"alice"}}],
              "revoked": [{{"public_key":"{pk}","reason":"leaked"}}] }}"#,
        pk = key.public_key
    ))
    .unwrap();
    assert!(matches!(
        trust::verify(&dir, &store),
        Verification::Revoked { .. }
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_revoked_key_cannot_be_retrusted_and_survives_round_trip() {
    let dir = tmp("revrt");
    let key = trust::generate_keyfile("alice").unwrap();
    let mut store = trust::TrustStore::default();
    assert!(store.revoke(&key.public_key, "compromised"));
    assert!(
        !store.add(&key.public_key, "alice"),
        "a revoked key cannot be silently re-trusted"
    );

    let sf = dir.join("trust.json");
    store.write(&sf).unwrap();
    let reloaded = trust::TrustStore::load(&sf).unwrap();
    assert!(
        reloaded.is_revoked(&key.public_key),
        "revocation survives a round-trip"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Root of trust (#21): a trusted root vouches for an author via a signed
/// keyring, so the author verifies as Trusted without being pinned directly.
#[test]
fn delegated_trust_via_a_signed_keyring() {
    let dir = tmp("delegated");
    make_cap(&dir);
    let author = trust::generate_keyfile("author").unwrap();
    trust::sign(&dir, &author).unwrap().write(&dir).unwrap();

    let root = trust::generate_keyfile("root").unwrap();
    let keyring = trust::sign_keyring(
        vec![trust::TrustedKey {
            public_key: author.public_key.clone(),
            label: "author".into(),
        }],
        &root,
    )
    .unwrap();
    assert!(keyring.verify(), "the root's keyring signature verifies");

    // A keyring alone (root not trusted) confers nothing.
    let mut store = trust::TrustStore::default();
    store.attach_keyring(keyring);
    assert!(
        matches!(trust::verify(&dir, &store), Verification::Untrusted { .. }),
        "a keyring without a trusted root confers no trust"
    );

    // Trust the root → the author is now trusted via delegation.
    store.add_root(&root.public_key, "root");
    let v = trust::verify(&dir, &store);
    assert!(
        matches!(v, Verification::Trusted { ref label, .. } if label.contains("via root")),
        "got {v:?}"
    );
    assert!(v.passes(true), "delegated trust passes strict mode");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tampered_keyring_confers_no_trust() {
    let dir = tmp("krtamper");
    make_cap(&dir);
    let author = trust::generate_keyfile("author").unwrap();
    trust::sign(&dir, &author).unwrap().write(&dir).unwrap();
    let root = trust::generate_keyfile("root").unwrap();
    let mut keyring = trust::sign_keyring(
        vec![trust::TrustedKey {
            public_key: author.public_key.clone(),
            label: "a".into(),
        }],
        &root,
    )
    .unwrap();
    let flip = if keyring.signature.starts_with('a') {
        'b'
    } else {
        'a'
    };
    keyring.signature.replace_range(0..1, &flip.to_string());
    assert!(!keyring.verify());

    let mut store = trust::TrustStore::default();
    store.add_root(&root.public_key, "root");
    store.attach_keyring(keyring);
    assert!(
        matches!(trust::verify(&dir, &store), Verification::Untrusted { .. }),
        "a keyring whose root signature is invalid is ignored"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revoking_a_root_withdraws_its_delegated_trust() {
    let dir = tmp("krrevoke");
    make_cap(&dir);
    let author = trust::generate_keyfile("author").unwrap();
    trust::sign(&dir, &author).unwrap().write(&dir).unwrap();
    let root = trust::generate_keyfile("root").unwrap();
    let keyring = trust::sign_keyring(
        vec![trust::TrustedKey {
            public_key: author.public_key.clone(),
            label: "a".into(),
        }],
        &root,
    )
    .unwrap();

    let mut store = trust::TrustStore::default();
    store.add_root(&root.public_key, "root");
    store.attach_keyring(keyring);
    assert!(matches!(
        trust::verify(&dir, &store),
        Verification::Trusted { .. }
    ));

    store.revoke(&root.public_key, "root compromised");
    assert!(
        matches!(trust::verify(&dir, &store), Verification::Untrusted { .. }),
        "revoking the root withdraws every author it vouched for"
    );
    let _ = std::fs::remove_dir_all(&dir);
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

// ---- malformed input reaches these parsers off disk ------------------------
//
// `capability.sig`, the trust store and keyrings are files an attacker may
// influence, so every one of these is ordinary untrusted input.

/// A signature file that is *present but broken* is `Invalid`, never `Unsigned`.
/// The consequence if this is wrong: corrupting a signature would be strictly
/// better for an attacker than tampering with content, because `Unsigned` passes
/// the gate whenever `--require-signed` is off.
#[test]
fn a_malformed_signature_is_invalid_not_unsigned() {
    let dir = tmp("malformed-sig");
    make_cap(&dir);
    let key = trust::generate_keyfile("alice").unwrap();
    trust::sign(&dir, &key).unwrap().write(&dir).unwrap();

    write(&dir.join(trust::SIG_NAME), "not: [valid, yaml");

    let store = trust::TrustStore::default();
    let verdict = trust::verify(&dir, &store);
    assert!(
        matches!(verdict, Verification::Invalid(_)),
        "a broken signature must be Invalid, got {verdict:?}"
    );
    assert!(
        !verdict.passes(false),
        "and it must fail the gate even without --require-signed"
    );
}

/// Non-ASCII in a hex field returns an error rather than panicking. `"aaa€"`
/// splits inside the `€`, which byte-index slicing would panic on.
#[test]
fn non_ascii_in_a_key_field_is_rejected_not_a_panic() {
    for bad in ["aaa\u{20AC}", "\u{20AC}\u{20AC}", "zz", "abc"] {
        let dir = tmp("bad-hex");
        make_cap(&dir);
        let key = trust::generate_keyfile("alice").unwrap();
        let mut sig = trust::sign(&dir, &key).unwrap();
        sig.public_key = bad.to_string();
        sig.write(&dir).unwrap();

        let verdict = trust::verify(&dir, &trust::TrustStore::default());
        assert!(
            matches!(verdict, Verification::Invalid(_)),
            "public_key {bad:?} must be Invalid, got {verdict:?}"
        );

        // The same field on the signature itself, and on a keyring's root.
        let mut sig2 = trust::sign(&dir, &key).unwrap();
        sig2.signature = bad.to_string();
        sig2.write(&dir).unwrap();
        assert!(matches!(
            trust::verify(&dir, &trust::TrustStore::default()),
            Verification::Invalid(_)
        ));

        let keyring = trust::Keyring {
            keys: Vec::new(),
            root_public_key: bad.to_string(),
            signature: bad.to_string(),
        };
        assert!(!keyring.verify(), "a keyring with {bad:?} must not verify");
    }
}

/// A symlink is hashed as a *link* (its target path), not as the bytes it points
/// at. Dereferencing would fold a file the capability does not ship into its
/// identity — so editing an unrelated file would report it as tampered — and an
/// absolute or cyclic link would make the digest machine-dependent or unwalkable.
#[test]
fn a_symlink_is_hashed_as_a_link_not_dereferenced() {
    #[cfg(unix)]
    {
        let root = tmp("symlink-digest");
        let outside = root.join("outside.txt");
        write(&outside, "not part of the capability");
        let dir = root.join("cap");
        make_cap(&dir);
        std::os::unix::fs::symlink("../outside.txt", dir.join("link")).unwrap();

        let before = trust::capability_digest(&dir).unwrap();

        // Editing what the link points at must not change the capability.
        write(&outside, "edited out from under it");
        assert_eq!(
            trust::capability_digest(&dir).unwrap(),
            before,
            "a file outside the tree must not be part of the digest"
        );

        // Repointing the link itself must be caught.
        std::fs::remove_file(dir.join("link")).unwrap();
        std::os::unix::fs::symlink("../elsewhere.txt", dir.join("link")).unwrap();
        assert_ne!(
            trust::capability_digest(&dir).unwrap(),
            before,
            "repointing a link is a change to the capability and must be detected"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A link cycle terminates instead of walking until the OS refuses.
#[test]
fn a_symlink_cycle_does_not_walk() {
    #[cfg(unix)]
    {
        let dir = tmp("symlink-cycle");
        make_cap(&dir);
        std::os::unix::fs::symlink("..", dir.join("loop")).unwrap();
        assert!(
            trust::capability_digest(&dir).is_ok(),
            "a cyclic link must be recorded, not followed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A capability with no symlinks digests exactly as before this change, so
/// existing signatures keep verifying.
#[test]
fn the_digest_of_an_ordinary_tree_is_unchanged() {
    let dir = tmp("digest-stable");
    write(&dir.join("capability.yaml"), "id: web\nkind: tool\n");
    write(&dir.join("body.md"), "capability asset contents");
    write(&dir.join("nested/asset.txt"), "nested");
    assert_eq!(
        trust::capability_digest(&dir).unwrap(),
        // Pinned against the binary from before symlinks were handled: hashing
        // a link as a link must not renumber every existing signature.
        "sha256:65d447970f2d94e918ad2ca05564b0672e8a176c2aa4e6c12f7a1eff060bb2de",
        "the digest of a symlink-free tree is a compatibility contract"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Revocation gates roots exactly as it gates authors. A revoked root is ignored
/// at verification time anyway, so accepting it into the store only records a
/// trust relationship that does not exist.
#[test]
fn a_revoked_key_cannot_be_added_as_a_root_either() {
    let key = trust::generate_keyfile("acme").unwrap();
    let mut store = trust::TrustStore::default();

    assert!(store.add_root(&key.public_key, "acme"), "trusted once");
    assert!(store.revoke(&key.public_key, "compromised"));
    assert!(
        !store.add_root(&key.public_key, "acme"),
        "a revoked key must not be re-added as a root"
    );

    // And a fresh store refuses it up front, matching `add`.
    let mut fresh = trust::TrustStore::default();
    assert!(fresh.revoke(&key.public_key, "compromised"));
    assert!(!fresh.add_root(&key.public_key, "acme"));
    assert!(!fresh.add(&key.public_key, "acme"));
}
