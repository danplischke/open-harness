//! Producing a publishable capability catalog (#21) — the **producer** half of
//! `docs/src/distribution.md`, opposite the archive source in `profile.rs`.
//!
//! Packing turns a directory of capabilities into the tree a CDN serves:
//!
//! ```text
//! blobs/sha256/<digest>.tar     # immutable, content-addressed capability archives
//! index/sha256/<digest>.json    # an immutable snapshot of this exact catalog
//! registry.json                 # the current catalog — what a profile points at
//! ```
//!
//! Two properties make this safe to put behind a cache:
//!
//!   * **Everything but `registry.json` is immutable.** A blob's URL *is* its
//!     sha256, so bytes can never change under a URL and the cache never needs
//!     invalidating. Index snapshots are content-addressed for the same reason,
//!     which also makes rollback a copy rather than a rebuild.
//!   * **Publish order is upload-then-swap.** Blobs and the snapshot go up
//!     first; `registry.json` is replaced last, so a consumer never reads a
//!     catalog that names an artifact which has not landed.
//!
//! Packing does **not** sign: `oh sign` writes `capability.sig` into the
//! capability directory, and packing simply carries that file along. Signing
//! stays a separate, explicit act — but an unsigned capability is reported, so a
//! catalog does not quietly ship code nobody vouched for.

use crate::manifest::LoadedCapability;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// One capability packed into a content-addressed archive.
pub struct Packed {
    pub id: String,
    pub version: String,
    pub kind: String,
    /// sha256 of `bytes`, hex (no `sha256:` prefix) — the archive's content address.
    pub digest: String,
    pub bytes: Vec<u8>,
    /// Whether the capability carried a `capability.sig` into the archive.
    pub signed: bool,
}

impl Packed {
    /// This archive's path within the published tree.
    pub fn blob_path(&self) -> String {
        format!("blobs/sha256/{}.tar", self.digest)
    }
}

/// What `write_tree` put on disk.
pub struct Published {
    pub out: PathBuf,
    pub blobs: usize,
    pub index_path: PathBuf,
    pub snapshot_path: PathBuf,
    pub total_bytes: u64,
    /// Ids of capabilities published without a signature.
    pub unsigned: Vec<String>,
}

/// Pack each capability into a deterministic archive addressed by its digest.
pub fn pack_capabilities(caps: &[LoadedCapability]) -> Result<Vec<Packed>, String> {
    let mut out = Vec::with_capacity(caps.len());
    for cap in caps {
        // The archive unpacks to `<dir>/…` so `discover` finds it exactly as it
        // was authored on disk.
        let root = cap
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| cap.manifest.id.clone());
        let members = crate::archive::read_dir_members(&cap.dir, &root)?;
        let bytes = crate::archive::write_tar(&members)?;
        let digest = crate::trust::sha256_hex(&bytes);
        out.push(Packed {
            id: cap.manifest.id.clone(),
            version: cap.manifest.version.clone(),
            kind: cap.manifest.kind.as_str().to_string(),
            digest,
            bytes,
            signed: crate::trust::Signature::load(&cap.dir).is_some(),
        });
    }
    // Stable order in, stable catalog out.
    out.sort_by(|a, b| (a.id.as_str(), a.version.as_str()).cmp(&(&b.id, &b.version)));
    Ok(out)
}

/// Render the registry index: each capability's name → the digest-pinned archive
/// that serves it, rooted at `base_url`.
pub fn index_json(packed: &[Packed], base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let entries: Vec<Value> = packed
        .iter()
        .map(|p| {
            json!({
                "name": p.id,
                "version": p.version,
                "source": {
                    "http": {
                        "url": format!("{base}/{}", p.blob_path()),
                        "sha256": p.digest,
                    }
                }
            })
        })
        .collect();
    format!(
        "{}\n",
        serde_json::to_string_pretty(&json!({ "capabilities": entries }))
            .unwrap_or_else(|_| "{}".into())
    )
}

/// Write the publishable tree under `out`.
///
/// Blobs and the immutable snapshot are written before `registry.json`, mirroring
/// the upload order a publisher must use so the catalog never points at an
/// artifact that has not landed.
pub fn write_tree(packed: &[Packed], base_url: &str, out: &Path) -> Result<Published, String> {
    let index = index_json(packed, base_url);
    let index_digest = crate::trust::sha256_hex(index.as_bytes());

    let mut total_bytes = 0u64;
    for p in packed {
        let path = out.join(p.blob_path());
        write_file(&path, &p.bytes)?;
        total_bytes += p.bytes.len() as u64;
    }

    let snapshot_path = out.join(format!("index/sha256/{index_digest}.json"));
    write_file(&snapshot_path, index.as_bytes())?;

    // Last, so it is never the first thing a consumer can see.
    let index_path = out.join("registry.json");
    write_file(&index_path, index.as_bytes())?;

    Ok(Published {
        out: out.to_path_buf(),
        blobs: packed.len(),
        index_path,
        snapshot_path,
        total_bytes,
        unsigned: packed
            .iter()
            .filter(|p| !p.signed)
            .map(|p| p.id.clone())
            .collect(),
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}
