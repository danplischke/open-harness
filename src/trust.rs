//! Trust & signing for distributable capabilities (#17).
//!
//! A registry that installs runnable hooks into people's agents is a
//! supply-chain surface, so integrity + authenticity are designed in, not
//! bolted on. The scheme is deliberately small and standard:
//!
//!   * **content digest** — `sha256` over the capability's file tree
//!     (each file hashed, entries sorted by path, then hashed together). Any
//!     tampered byte or renamed file changes it.
//!   * **signature** — the author signs the digest with an **ed25519** key; the
//!     detached `capability.sig` carries `{algorithm, public_key, digest,
//!     signature}`.
//!   * **trust** — a [`TrustStore`] lists trusted public keys. Verification is
//!     four-valued: `Trusted` (valid signature by a known key), `Untrusted`
//!     (valid signature, unknown key — trust-on-first-use territory), `Invalid`
//!     (tampered content or bad signature — always rejected), `Unsigned`.
//!
//! Enforcement composes with the resolver (#15): a tampered capability is
//! rejected on resolve; `--require-signed` additionally rejects unsigned /
//! untrusted ones. The permission manifest (`manifest::Permissions`) is
//! surfaced for consent and checked against a policy (advisory). See
//! `SECURITY.md` for the threat model and what is deferred (sandbox
//! enforcement, revocation, key distribution).

use crate::manifest::{PermissionPolicy, Permissions};
use ed25519_dalek::{Signature as EdSig, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// The detached signature file written next to `capability.json`.
pub const SIG_NAME: &str = "capability.sig";

// ---- content digest -------------------------------------------------------

/// A stable `sha256:<hex>` digest of a capability directory's file tree. Files
/// are hashed individually, sorted by their forward-slash relative path, then
/// hashed together — so the result is order- and platform-independent. The
/// signature file itself is excluded.
pub fn capability_digest(dir: &Path) -> Result<String, String> {
    let mut entries: Vec<(String, String)> = Vec::new();
    collect(dir, dir, &mut entries)?;
    entries.sort();
    let mut h = Sha256::new();
    for (rel, file_hash) in &entries {
        h.update(rel.as_bytes());
        h.update(b" ");
        h.update(file_hash.as_bytes());
        h.update(b"\n");
    }
    Ok(format!("sha256:{}", hex(&h.finalize())))
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("scan {}: {e}", dir.display()))?;
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            // Skip the managed cache/lock tree if present.
            if name == ".open-harness" {
                continue;
            }
            collect(base, &path, out)?;
        } else if path.is_file() {
            if name == SIG_NAME {
                continue; // never sign the signature
            }
            let rel = path
                .strip_prefix(base)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes =
                std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            out.push((rel, sha256_hex(&bytes)));
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

// ---- keys -----------------------------------------------------------------

/// A keypair file (`ed25519`). The `secret_key` is sensitive — never commit it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keyfile {
    pub algorithm: String,
    pub secret_key: String,
    pub public_key: String,
    #[serde(default)]
    pub label: String,
}

/// Generate a fresh ed25519 keypair (OS randomness).
pub fn generate_keyfile(label: &str) -> Result<Keyfile, String> {
    let mut secret = [0u8; 32];
    getrandom::getrandom(&mut secret).map_err(|e| format!("os rng: {e}"))?;
    let signing = SigningKey::from_bytes(&secret);
    let public = signing.verifying_key().to_bytes();
    Ok(Keyfile {
        algorithm: "ed25519".to_string(),
        secret_key: hex(&secret),
        public_key: hex(&public),
        label: label.to_string(),
    })
}

impl Keyfile {
    fn signing_key(&self) -> Result<SigningKey, String> {
        let bytes: [u8; 32] = decode_hex_array(&self.secret_key)?;
        Ok(SigningKey::from_bytes(&bytes))
    }

    pub fn load(path: &Path) -> Result<Keyfile, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("invalid key file: {e}"))
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, format!("{text}\n"))
            .map_err(|e| format!("write {}: {e}", path.display()))
    }
}

/// A short, human-comparable fingerprint of a public key (`oh:<16 hex>`).
pub fn fingerprint(public_key_hex: &str) -> String {
    let bytes = decode_hex(public_key_hex).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&bytes);
    let digest = h.finalize();
    format!("oh:{}", hex(&digest[..8]))
}

// ---- signature ------------------------------------------------------------

/// The detached `capability.sig` document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signature {
    pub algorithm: String,
    pub public_key: String,
    pub digest: String,
    pub signature: String,
}

impl Signature {
    pub fn load(dir: &Path) -> Option<Signature> {
        let text = std::fs::read_to_string(dir.join(SIG_NAME)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn write(&self, dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(SIG_NAME), format!("{text}\n"))
            .map_err(|e| format!("write {SIG_NAME}: {e}"))
    }
}

/// Sign a capability directory with `key`, producing (but not writing) its
/// detached signature.
pub fn sign(dir: &Path, key: &Keyfile) -> Result<Signature, String> {
    let digest = capability_digest(dir)?;
    let signing = key.signing_key()?;
    let sig = signing.sign(digest.as_bytes());
    Ok(Signature {
        algorithm: "ed25519".to_string(),
        public_key: key.public_key.clone(),
        digest,
        signature: hex(&sig.to_bytes()),
    })
}

// ---- verification ---------------------------------------------------------

/// The four-valued verdict for a capability directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// Valid signature by a key in the trust store.
    Trusted { label: String, fingerprint: String },
    /// Valid signature by an unknown key (trust-on-first-use territory).
    Untrusted {
        fingerprint: String,
        public_key: String,
    },
    /// Tampered content or a bad signature — always rejected.
    Invalid(String),
    /// No signature present.
    Unsigned,
}

impl Verification {
    /// Does this verdict pass the gate? `Invalid` never passes; `Untrusted` /
    /// `Unsigned` pass only when signatures are not required.
    pub fn passes(&self, require_signed: bool) -> bool {
        match self {
            Verification::Trusted { .. } => true,
            Verification::Untrusted { .. } | Verification::Unsigned => !require_signed,
            Verification::Invalid(_) => false,
        }
    }

    /// A short status label for reporting.
    pub fn status(&self) -> String {
        match self {
            Verification::Trusted { label, fingerprint } => {
                let who = if label.is_empty() { fingerprint } else { label };
                format!("trusted ({who})")
            }
            Verification::Untrusted { fingerprint, .. } => format!("UNTRUSTED ({fingerprint})"),
            Verification::Invalid(r) => format!("INVALID — {r}"),
            Verification::Unsigned => "unsigned".to_string(),
        }
    }
}

/// Verify a capability directory against a trust store.
pub fn verify(dir: &Path, trust: &TrustStore) -> Verification {
    let Some(sig) = Signature::load(dir) else {
        return Verification::Unsigned;
    };
    if sig.algorithm != "ed25519" {
        return Verification::Invalid(format!("unsupported algorithm '{}'", sig.algorithm));
    }
    // Recompute the digest: any content change breaks this before crypto runs.
    let digest = match capability_digest(dir) {
        Ok(d) => d,
        Err(e) => return Verification::Invalid(format!("cannot digest: {e}")),
    };
    if digest != sig.digest {
        return Verification::Invalid("content digest mismatch (tampered)".to_string());
    }
    let pk_bytes: [u8; 32] = match decode_hex_array(&sig.public_key) {
        Ok(b) => b,
        Err(_) => return Verification::Invalid("malformed public key".to_string()),
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk_bytes) else {
        return Verification::Invalid("invalid public key".to_string());
    };
    let sig_bytes: [u8; 64] = match decode_hex_array(&sig.signature) {
        Ok(b) => b,
        Err(_) => return Verification::Invalid("malformed signature".to_string()),
    };
    let ed = EdSig::from_bytes(&sig_bytes);
    if vk.verify(digest.as_bytes(), &ed).is_err() {
        return Verification::Invalid("signature verification failed".to_string());
    }
    let fp = fingerprint(&sig.public_key);
    match trust.label_for(&sig.public_key) {
        Some(label) => Verification::Trusted {
            label,
            fingerprint: fp,
        },
        None => Verification::Untrusted {
            fingerprint: fp,
            public_key: sig.public_key.clone(),
        },
    }
}

// ---- trust store ----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedKey {
    pub public_key: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default)]
    pub keys: Vec<TrustedKey>,
}

impl TrustStore {
    pub fn from_json(text: &str) -> Result<TrustStore, String> {
        serde_json::from_str(text).map_err(|e| format!("invalid trust store: {e}"))
    }

    pub fn load(path: &Path) -> Result<TrustStore, String> {
        match std::fs::read_to_string(path) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| format!("invalid trust store: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TrustStore::default()),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, format!("{text}\n"))
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    pub fn label_for(&self, public_key: &str) -> Option<String> {
        self.keys
            .iter()
            .find(|k| k.public_key == public_key)
            .map(|k| k.label.clone())
    }

    pub fn contains(&self, public_key: &str) -> bool {
        self.keys.iter().any(|k| k.public_key == public_key)
    }

    /// Add a key (trust-on-first-use consent). Returns false if already present.
    pub fn add(&mut self, public_key: &str, label: &str) -> bool {
        if self.contains(public_key) {
            return false;
        }
        self.keys.push(TrustedKey {
            public_key: public_key.to_string(),
            label: label.to_string(),
        });
        true
    }
}

// ---- permission policy (advisory enforcement) -----------------------------

/// Check a capability's declared permissions against a policy, returning any
/// violations. Advisory: the caller decides whether a violation warns or blocks.
pub fn permission_violations(perms: &Permissions, policy: &PermissionPolicy) -> Vec<String> {
    let mut v = Vec::new();
    if !policy.allow_network && perms.requests_network() {
        v.push(format!(
            "requests network ({}) but policy denies network",
            perms.network_summary()
        ));
    }
    if !policy.allow_exec && !perms.exec.is_empty() {
        v.push(format!(
            "requests exec {:?} but policy denies exec",
            perms.exec
        ));
    }
    v
}

// ---- hex ------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn decode_hex_array<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let v = decode_hex(s)?;
    v.try_into().map_err(|_| format!("expected {N} bytes"))
}
