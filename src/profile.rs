//! Profiles, sources, and the resolved lockfile (#15) — the L4 "compose a
//! capability set from several places, then pin it" layer.
//!
//! A **profile** is a named set of **sources** targeting a set of harnesses. A
//! source resolves to capabilities from a **local path**, a **git repo**
//! (personal-repo sync — cloned/fetched and pinned to a commit), or a
//! **registry** (stubbed; reported, never silently skipped). Resolving a profile
//! composes the sources in order and writes an `open-harness.lock` pinning every
//! resolved capability (id + version + content fingerprint) and every git
//! source's exact commit — so a re-resolve is reproducible.
//!
//! This is the sourcing layer beneath `oh sync` (#16): `sync --profile` resolves
//! here, then hands the composed capabilities + target harnesses to `sync::apply`.
//!
//! Git access shells out to the system `git` (the crate stays dependency-light);
//! a source `url` may be a remote or a local path, so the whole flow is testable
//! offline against a throwaway local repo.

use crate::adapters::Harness;
use crate::manifest::LoadedCapability;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Default lockfile name, written next to the profile.
pub const LOCK_NAME: &str = "open-harness.lock";

// ---- profile + source model ----------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Profile {
    #[serde(default = "default_profile_name")]
    pub name: String,
    /// Target harness ids for this profile (what `sync` fans out to).
    #[serde(default)]
    pub harnesses: Vec<String>,
    pub sources: Vec<Source>,
}

fn default_profile_name() -> String {
    "default".to_string()
}

/// Where a set of capabilities comes from. Externally tagged, e.g.
/// `{"local": {"path": "capabilities"}}` or
/// `{"git": {"url": "...", "rev": "main", "subdir": "capabilities"}}`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Local(LocalSource),
    Git(GitSource),
    Registry(RegistrySource),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalSource {
    /// Directory of `*/capability.json`, relative to the profile's workdir.
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitSource {
    pub url: String,
    /// Branch / tag / SHA to resolve. Defaults to `HEAD`.
    #[serde(default = "default_rev")]
    pub rev: String,
    /// Subdirectory within the repo to scan. Defaults to the repo root.
    #[serde(default)]
    pub subdir: Option<String>,
}

fn default_rev() -> String {
    "HEAD".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistrySource {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

impl Profile {
    pub fn from_json(text: &str) -> Result<Profile, String> {
        serde_json::from_str(text).map_err(|e| format!("invalid profile: {e}"))
    }

    pub fn load(path: &Path) -> Result<Profile, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Profile::from_json(&text)
    }

    fn harness_set(&self) -> Result<Vec<Harness>, String> {
        self.harnesses
            .iter()
            .map(|id| Harness::from_id(id).ok_or_else(|| format!("unknown harness '{id}'")))
            .collect()
    }
}

// ---- lockfile -------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedCap {
    pub id: String,
    pub version: String,
    pub kind: String,
    /// Content fingerprint of the capability's `capability.json`.
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedSource {
    /// `"local"` | `"git"` | `"registry"`.
    pub kind: String,
    /// Path (local) or url (git) the capabilities came from.
    pub origin: String,
    /// Resolved commit SHA for a git source; `None` for local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    pub capabilities: Vec<LockedCap>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lock {
    pub version: u32,
    pub profile: String,
    pub sources: Vec<LockedSource>,
}

impl Lock {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(text: &str) -> Result<Lock, String> {
        serde_json::from_str(text).map_err(|e| format!("invalid lockfile: {e}"))
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, format!("{}\n", self.to_json()))
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    /// The resolved rev pinned for a git source at `url`, if any.
    fn pinned_rev(&self, url: &str) -> Option<&str> {
        self.sources
            .iter()
            .find(|s| s.kind == "git" && s.origin == url)
            .and_then(|s| s.rev.as_deref())
    }
}

// ---- resolution -----------------------------------------------------------

/// The outcome of resolving a profile: the composed capability set, its target
/// harnesses, the lockfile to write, and any non-fatal warnings.
pub struct Resolved {
    pub profile: String,
    pub harnesses: Vec<Harness>,
    pub capabilities: Vec<LoadedCapability>,
    pub lock: Lock,
    pub warnings: Vec<String>,
}

/// Resolve a profile. `workdir` roots relative local paths and the git cache.
/// If `lock` is given, git sources are checked out at their pinned commit
/// (reproducible); otherwise each git source resolves its requested `rev` and
/// the resulting SHA is recorded.
pub fn resolve(profile: &Profile, workdir: &Path, lock: Option<&Lock>) -> Result<Resolved, String> {
    let harnesses = profile.harness_set()?;
    let mut composed: Vec<LoadedCapability> = Vec::new();
    let mut seen_ids: Vec<String> = Vec::new();
    let mut locked_sources: Vec<LockedSource> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for source in &profile.sources {
        let (origin, kind, rev, subdir, caps) = match source {
            Source::Local(l) => {
                let dir = workdir.join(&l.path);
                let caps = crate::manifest::discover(&dir)?;
                (l.path.clone(), "local", None, None, caps)
            }
            Source::Git(g) => {
                let pin = lock.and_then(|lk| lk.pinned_rev(&g.url));
                let (checkout_dir, sha) = git_sync(&g.url, pin.unwrap_or(&g.rev), workdir)?;
                let scan = match &g.subdir {
                    Some(s) => checkout_dir.join(s),
                    None => checkout_dir,
                };
                let caps = crate::manifest::discover(&scan)?;
                (g.url.clone(), "git", Some(sha), g.subdir.clone(), caps)
            }
            Source::Registry(r) => {
                warnings.push(format!(
                    "registry source '{}' skipped — registry resolution is not yet implemented (use a local or git source)",
                    r.name
                ));
                continue;
            }
        };

        let mut locked_caps = Vec::new();
        for cap in caps {
            locked_caps.push(LockedCap {
                id: cap.manifest.id.clone(),
                version: cap.manifest.version.clone(),
                kind: cap.manifest.kind.as_str().to_string(),
                fingerprint: crate::sync::fingerprint(&manifest_json(&cap)),
            });
            // Earlier sources win; a later duplicate id is a loud skip.
            if seen_ids.contains(&cap.manifest.id) {
                warnings.push(format!(
                    "capability '{}' from {origin} shadowed by an earlier source; kept the earlier one",
                    cap.manifest.id
                ));
            } else {
                seen_ids.push(cap.manifest.id.clone());
                composed.push(cap);
            }
        }
        locked_sources.push(LockedSource {
            kind: kind.to_string(),
            origin,
            rev,
            subdir,
            capabilities: locked_caps,
        });
    }

    // Dependency check (advisory): every declared dependency must be present.
    for cap in &composed {
        for dep in &cap.manifest.dependencies {
            if !seen_ids.contains(dep) {
                warnings.push(format!(
                    "capability '{}' declares dependency '{dep}', which no source provides",
                    cap.manifest.id
                ));
            }
        }
    }

    let lock = Lock {
        version: 1,
        profile: profile.name.clone(),
        sources: locked_sources,
    };
    Ok(Resolved {
        profile: profile.name.clone(),
        harnesses,
        capabilities: composed,
        lock,
        warnings,
    })
}

/// Re-serialize a capability's manifest to a stable string for fingerprinting.
fn manifest_json(cap: &LoadedCapability) -> String {
    // Read the on-disk capability.json when available (stable bytes); fall back
    // to the id if unreadable.
    std::fs::read_to_string(cap.dir.join("capability.json"))
        .unwrap_or_else(|_| cap.manifest.id.clone())
}

// ---- git personal-repo sync (shells out to `git`) -------------------------

/// Clone-or-fetch `url` into a cache under `workdir/.open-harness/cache/git/`,
/// check out `rev`, and return `(checkout_dir, resolved_sha)`.
fn git_sync(url: &str, rev: &str, workdir: &Path) -> Result<(PathBuf, String), String> {
    let dest = workdir
        .join(".open-harness")
        .join("cache")
        .join("git")
        .join(cache_slug(url));

    if dest.join(".git").is_dir() {
        // Best-effort refresh; ignore fetch failure (offline / pinned SHA).
        let _ = git(&[
            "-C",
            &dest.to_string_lossy(),
            "fetch",
            "--all",
            "--tags",
            "--quiet",
        ]);
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir cache: {e}"))?;
        }
        git(&["clone", "--quiet", url, &dest.to_string_lossy()])?;
    }

    let dir = dest.to_string_lossy().into_owned();
    git(&["-C", &dir, "checkout", "--quiet", rev])
        .map_err(|e| format!("checkout '{rev}' in {url}: {e}"))?;
    let sha = git(&["-C", &dir, "rev-parse", "HEAD"])?;
    Ok((dest, sha))
}

/// A filesystem-safe cache directory name for a git url.
fn cache_slug(url: &str) -> String {
    let safe: String = url
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed: String = safe
        .trim_matches('-')
        .chars()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{trimmed}-{}", crate::sync::fingerprint(url))
}

fn git(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("git not available: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
