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
//!   * **trust** — a [`TrustStore`] lists trusted (and revoked) public keys.
//!     Verification is five-valued: `Trusted` (valid signature by a known key),
//!     `Untrusted` (valid signature, unknown key — trust-on-first-use territory),
//!     `Revoked` (valid signature by a **withdrawn** key — always rejected, #22),
//!     `Invalid` (tampered content or bad signature — always rejected),
//!     `Unsigned`.
//!
//! Enforcement composes with the resolver (#15): a tampered capability is
//! rejected on resolve; `--require-signed` additionally rejects unsigned /
//! untrusted ones. The permission manifest (`manifest::Permissions`) is
//! surfaced for consent and checked against a policy (advisory). Beyond per-key
//! trust-on-first-use, a **root of trust** (#21) lets a trusted root vouch for
//! many authors via a signed [`Keyring`], so consumers pin a root once. A trusted
//! (or root) key can be **revoked** (#22): revocation is consulted before trust,
//! so a withdrawn key never verifies again — even in advisory mode, and revoking
//! a root withdraws every author it vouched for. See `SECURITY.md` for the
//! threat model and what is still deferred (runtime sandbox *enforcement* of the
//! permission manifest, key distribution).

use crate::manifest::{PermissionPolicy, Permissions};
use ed25519_dalek::{Signature as EdSig, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// The detached signature file written next to the capability manifest.
pub const SIG_NAME: &str = "capability.sig";

// ---- content digest -------------------------------------------------------

/// A stable `sha256:<hex>` digest of a capability directory's file tree. Files
/// are hashed individually, sorted by their forward-slash relative path, then
/// hashed together — so the result is order- and platform-independent. The
/// signature file itself is excluded.
///
/// A **symlink is hashed as a link** — its target *path*, not the bytes it
/// points at — exactly as git records one. Dereferencing instead would make the
/// digest describe files the capability does not ship: a link out of the tree
/// would fold an unrelated file into the capability's identity (so editing that
/// file reports the capability as *tampered*), an absolute link would produce a
/// machine-dependent digest whose signature could never verify on a consumer's
/// box, and a link cycle would be walked until the OS refused. Recording the
/// link itself keeps the digest self-contained and cycle-free while still
/// covering the link, so adding or repointing one is detected.
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
        // `file_type` describes the entry itself; `Path::is_dir`/`is_file` follow
        // the link and would let the walk leave the tree (see the fn docs). This
        // matches `manifest::discover`, which is symlink-safe for the same reason.
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => return Err(format!("stat {}: {e}", path.display())),
        };
        let rel = || -> Result<String, String> {
            Ok(path
                .strip_prefix(base)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/"))
        };

        if ft.is_symlink() {
            if name == SIG_NAME {
                continue;
            }
            let target = std::fs::read_link(&path)
                .map_err(|e| format!("read link {}: {e}", path.display()))?;
            // Prefixed so a link entry can never collide with a file's 64-hex
            // digest, and normalized so the same link hashes alike either side
            // of a Windows/POSIX boundary.
            let target = target.to_string_lossy().replace('\\', "/");
            out.push((rel()?, format!("symlink:{}", sha256_hex(target.as_bytes()))));
        } else if ft.is_dir() {
            // Skip the managed cache/lock tree if present.
            if name == ".open-harness" {
                continue;
            }
            collect(base, &path, out)?;
        } else if ft.is_file() {
            if name == SIG_NAME {
                continue; // never sign the signature
            }
            let bytes =
                std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            out.push((rel()?, sha256_hex(&bytes)));
        }
    }
    Ok(())
}

/// `sha256` of a byte slice, hex-encoded (no `sha256:` prefix).
///
/// The digest of a *fetched artifact* — what a content-addressed URL names and
/// what a profile pins — as opposed to [`capability_digest`], which hashes an
/// unpacked directory tree.
pub fn sha256_hex(bytes: &[u8]) -> String {
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
        crate::config::load(path).map_err(|e| format!("invalid key file: {e}"))
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        crate::config::write(path, self)
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
    /// Read the detached signature beside a capability.
    ///
    /// Three outcomes, deliberately distinguished: `Ok(None)` = no signature
    /// file, `Ok(Some(..))` = a well-formed one, `Err` = a file that is there
    /// but unreadable or malformed. Collapsing the last into the first would
    /// make *corrupting* a signature strictly better for an attacker than
    /// tampering with content — a `Trusted` capability would report as merely
    /// `Unsigned`, which passes the gate whenever `--require-signed` is off.
    pub fn load(dir: &Path) -> Result<Option<Signature>, String> {
        let path = dir.join(SIG_NAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        crate::config::from_str(&text, crate::config::Format::Either)
            .map(Some)
            .map_err(|e| format!("{SIG_NAME} is present but malformed: {e}"))
    }

    pub fn write(&self, dir: &Path) -> Result<(), String> {
        crate::config::write(&dir.join(SIG_NAME), self)
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

/// The five-valued verdict for a capability directory.
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
    /// A valid signature by a key that has since been **revoked** — always
    /// rejected, even in advisory mode (a withdrawn key is actively distrusted).
    Revoked { fingerprint: String, reason: String },
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
            Verification::Invalid(_) | Verification::Revoked { .. } => false,
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
            Verification::Revoked {
                fingerprint,
                reason,
            } => {
                if reason.is_empty() {
                    format!("REVOKED ({fingerprint})")
                } else {
                    format!("REVOKED ({fingerprint}) — {reason}")
                }
            }
            Verification::Unsigned => "unsigned".to_string(),
        }
    }
}

/// Verify a capability directory against a trust store.
pub fn verify(dir: &Path, trust: &TrustStore) -> Verification {
    let sig = match Signature::load(dir) {
        Ok(Some(sig)) => sig,
        Ok(None) => return Verification::Unsigned,
        // Present but unusable is a broken signature, not an absent one.
        Err(e) => return Verification::Invalid(e),
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
    // Revocation wins over trust: a withdrawn key is rejected even if it is also
    // (stale) in the trusted list.
    if let Some(reason) = trust.revocation_reason(&sig.public_key) {
        return Verification::Revoked {
            fingerprint: fp,
            reason: reason.to_string(),
        };
    }
    // Direct trust, then delegated trust (a trusted root vouching via a keyring),
    // then untrusted.
    if let Some(label) = trust
        .label_for(&sig.public_key)
        .or_else(|| trust.delegated_label(&sig.public_key))
    {
        return Verification::Trusted {
            label,
            fingerprint: fp,
        };
    }
    Verification::Untrusted {
        fingerprint: fp,
        public_key: sig.public_key.clone(),
    }
}

// ---- trust store ----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedKey {
    pub public_key: String,
    #[serde(default)]
    pub label: String,
}

/// A withdrawn key. Once revoked, a signature by this key never verifies again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokedKey {
    pub public_key: String,
    #[serde(default)]
    pub reason: String,
}

/// A **keyring**: a set of author keys vouched for by a single **root** key
/// (#21). It lets one trusted root delegate trust to many authors, so consumers
/// pin a root once instead of every author key (trust-on-first-use per author).
/// The root signs the sorted author public keys, so re-labeling doesn't break it
/// and adding/removing an author requires the root to re-sign.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keyring {
    /// Author keys this keyring vouches for.
    pub keys: Vec<TrustedKey>,
    /// The root public key that signed this keyring.
    pub root_public_key: String,
    /// ed25519 signature by the root over the sorted author public keys.
    pub signature: String,
}

/// The stable message a root signs to vouch for a keyring: the author public
/// keys, sorted and newline-joined (order- and label-independent).
fn keyring_message(keys: &[TrustedKey]) -> String {
    let mut pks: Vec<&str> = keys.iter().map(|k| k.public_key.as_str()).collect();
    pks.sort_unstable();
    pks.join("\n")
}

/// Sign a keyring: the `root` vouches for `keys`.
pub fn sign_keyring(keys: Vec<TrustedKey>, root: &Keyfile) -> Result<Keyring, String> {
    let signing = root.signing_key()?;
    let sig = signing.sign(keyring_message(&keys).as_bytes());
    Ok(Keyring {
        keys,
        root_public_key: root.public_key.clone(),
        signature: hex(&sig.to_bytes()),
    })
}

impl Keyring {
    pub fn load(path: &Path) -> Result<Keyring, String> {
        crate::config::load(path).map_err(|e| format!("invalid keyring: {e}"))
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        crate::config::write(path, self)
    }

    /// Verify the root's signature over the vouched keys.
    pub fn verify(&self) -> bool {
        let Ok(pk) = decode_hex_array::<32>(&self.root_public_key) else {
            return false;
        };
        let Ok(vk) = VerifyingKey::from_bytes(&pk) else {
            return false;
        };
        let Ok(sig_bytes) = decode_hex_array::<64>(&self.signature) else {
            return false;
        };
        let ed = EdSig::from_bytes(&sig_bytes);
        vk.verify(keyring_message(&self.keys).as_bytes(), &ed)
            .is_ok()
    }

    fn vouches_for(&self, public_key: &str) -> Option<&TrustedKey> {
        self.keys.iter().find(|k| k.public_key == public_key)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default)]
    pub keys: Vec<TrustedKey>,
    /// Revoked keys. Consulted before the trusted list, so revocation wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked: Vec<RevokedKey>,
    /// Trusted root keys — a root vouches for authors via a signed keyring (#21).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<TrustedKey>,
    /// Keyrings attached to this store; each is honored only if signed by a
    /// trusted (non-revoked) root and its signature verifies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyrings: Vec<Keyring>,
}

impl TrustStore {
    /// Parse a trust store from YAML (or legacy JSON) text.
    pub fn from_text(text: &str) -> Result<TrustStore, String> {
        crate::config::from_str(text, crate::config::Format::Either)
            .map_err(|e| format!("invalid trust store: {e}"))
    }

    pub fn load(path: &Path) -> Result<TrustStore, String> {
        match std::fs::read_to_string(path) {
            Ok(t) => TrustStore::from_text(&t),
            // An absent store is an empty one — trust-on-first-use starts here.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TrustStore::default()),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        crate::config::write(path, self)
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

    /// Add a key (trust-on-first-use consent). Returns false if already present
    /// or revoked (a revoked key cannot be re-trusted without un-revoking first).
    pub fn add(&mut self, public_key: &str, label: &str) -> bool {
        if self.contains(public_key) || self.is_revoked(public_key) {
            return false;
        }
        self.keys.push(TrustedKey {
            public_key: public_key.to_string(),
            label: label.to_string(),
        });
        true
    }

    /// The revocation reason for a key, if it has been revoked.
    pub fn revocation_reason(&self, public_key: &str) -> Option<&str> {
        self.revoked
            .iter()
            .find(|k| k.public_key == public_key)
            .map(|k| k.reason.as_str())
    }

    pub fn is_revoked(&self, public_key: &str) -> bool {
        self.revoked.iter().any(|k| k.public_key == public_key)
    }

    /// Revoke a key: drop it from the trusted list and add it to the revocation
    /// list. Returns false if it was already revoked.
    pub fn revoke(&mut self, public_key: &str, reason: &str) -> bool {
        if self.is_revoked(public_key) {
            return false;
        }
        self.keys.retain(|k| k.public_key != public_key);
        self.revoked.push(RevokedKey {
            public_key: public_key.to_string(),
            reason: reason.to_string(),
        });
        true
    }

    /// Trust a root key (delegation anchor). Returns false if already trusted.
    pub fn add_root(&mut self, public_key: &str, label: &str) -> bool {
        if self.roots.iter().any(|k| k.public_key == public_key) {
            return false;
        }
        self.roots.push(TrustedKey {
            public_key: public_key.to_string(),
            label: label.to_string(),
        });
        true
    }

    /// Attach a keyring so its authors are trusted (once its root is trusted).
    pub fn attach_keyring(&mut self, keyring: Keyring) {
        self.keyrings.push(keyring);
    }

    fn is_root(&self, public_key: &str) -> bool {
        self.roots.iter().any(|k| k.public_key == public_key)
    }

    /// A label for `public_key` if some attached keyring vouches for it, that
    /// keyring's root is trusted (and not revoked), and its signature verifies.
    /// This is the delegated-trust path: a valid root vouching for an author.
    pub fn delegated_label(&self, public_key: &str) -> Option<String> {
        for kr in &self.keyrings {
            if !self.is_root(&kr.root_public_key) || self.is_revoked(&kr.root_public_key) {
                continue;
            }
            if !kr.verify() {
                continue;
            }
            if let Some(vouched) = kr.vouches_for(public_key) {
                let via = fingerprint(&kr.root_public_key);
                return Some(if vouched.label.is_empty() {
                    format!("via root {via}")
                } else {
                    format!("{} (via root {via})", vouched.label)
                });
            }
        }
        None
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

/// Decode a hex string to bytes.
///
/// Works on **bytes**, not string slices: these strings come off disk — out of a
/// `capability.sig`, a trust store, a keyring — so a non-ASCII one is ordinary
/// untrusted input, and `&s[i..i + 2]` would panic on it rather than return the
/// `Err` every caller is written to handle (`"aaa€"` splits inside the `€`).
fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("odd-length hex".to_string());
    }
    bytes
        .chunks_exact(2)
        .map(|pair| Ok(hex_digit(pair[0])? << 4 | hex_digit(pair[1])?))
        .collect()
}

fn hex_digit(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        other => Err(format!(
            "invalid hex digit {:?}",
            char::from_u32(other as u32).unwrap_or('?')
        )),
    }
}

fn decode_hex_array<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let v = decode_hex(s)?;
    v.try_into().map_err(|_| format!("expected {N} bytes"))
}
