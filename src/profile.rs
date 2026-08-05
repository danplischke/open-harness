//! Profiles, sources, and the resolved lockfile (#15) — the L4 "compose a
//! capability set from several places, then pin it" layer.
//!
//! A **profile** is a named set of **sources** targeting a set of harnesses. A
//! source resolves to capabilities from a **local path**, a **git repo**
//! (personal-repo sync — cloned/fetched and pinned to a commit), or a
//! **registry** (#21) — a JSON index mapping names to their real local/git
//! source, one point of indirection over many repos. Resolving a profile
//! composes the sources in order and writes an `open-harness.lock` pinning every
//! resolved capability (id + version + content fingerprint) and every git
//! source's exact commit — so a re-resolve is reproducible.
//!
//! Composition also resolves the **dependency graph** (#23): capabilities may
//! declare `dependencies` on other ids; the resolver reports unmet dependencies
//! and cycles (loudly, non-fatally), orders the set so a dependency precedes its
//! dependents, and pins each capability's resolved dependencies in the lock.
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
use std::collections::HashMap;
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
    /// Capability/pack name to resolve from the registry index.
    pub name: String,
    /// Optional exact version to select (else the first matching entry).
    #[serde(default)]
    pub version: Option<String>,
    /// Path to the registry index (JSON), relative to the profile workdir. The
    /// index maps names → real sources (local/git), giving one point of
    /// indirection over many repos. Without it the source is a loud no-op.
    #[serde(default)]
    pub index: Option<String>,
}

/// A registry index: a JSON file mapping capability names to their real source.
/// `{ "capabilities": [ { "name": "pack", "version": "1.0.0",
///    "source": { "git": { "url": "…", "subdir": "…" } } } ] }`.
#[derive(Debug, Clone, Deserialize)]
struct RegistryIndex {
    #[serde(default)]
    capabilities: Vec<RegistryEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryEntry {
    name: String,
    #[serde(default)]
    version: Option<String>,
    /// Where this named capability actually lives (a local or git source).
    source: Source,
}

impl RegistryIndex {
    fn load(path: &Path) -> Result<RegistryIndex, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("invalid registry index: {e}"))
    }

    fn find(&self, name: &str, version: Option<&str>) -> Option<&RegistryEntry> {
        self.capabilities
            .iter()
            .find(|e| e.name == name && (version.is_none() || e.version.as_deref() == version))
    }
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
    /// Direct capability dependencies (ids). Recorded so the resolved dependency
    /// graph is pinned and auditable in the lock.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
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
    let mut kept_version: HashMap<String, String> = HashMap::new();
    let mut locked_sources: Vec<LockedSource> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for source in &profile.sources {
        let Some((origin, kind, rev, subdir, caps)) =
            resolve_source(source, workdir, lock, &mut warnings)?
        else {
            continue; // a source that resolved to nothing was already warned about
        };

        let mut locked_caps = Vec::new();
        for cap in caps {
            locked_caps.push(LockedCap {
                id: cap.manifest.id.clone(),
                version: cap.manifest.version.clone(),
                kind: cap.manifest.kind.as_str().to_string(),
                fingerprint: crate::sync::fingerprint(&manifest_json(&cap)),
                dependencies: cap.manifest.dependencies.clone(),
            });
            // Earlier sources win; a later duplicate id is a loud skip, noting a
            // version conflict when the shadowed copy differs.
            if let Some(kept) = kept_version.get(&cap.manifest.id) {
                let conflict = if *kept != cap.manifest.version {
                    format!(" (kept {kept}, ignored {})", cap.manifest.version)
                } else {
                    String::new()
                };
                warnings.push(format!(
                    "capability '{}' from {origin} shadowed by an earlier source{conflict}",
                    cap.manifest.id
                ));
            } else {
                kept_version.insert(cap.manifest.id.clone(), cap.manifest.version.clone());
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

    // Resolve the dependency graph: report unmet dependencies (transitively) and
    // cycles — loudly, but non-fatally, so nothing is silently dropped — and
    // order the set so a dependency is composed before whatever depends on it.
    let composed = resolve_dependencies(composed, &seen_ids, &mut warnings);

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

/// A resolved source: `(origin, kind, pinned rev, subdir, capabilities)`.
type ResolvedSource = (
    String,
    &'static str,
    Option<String>,
    Option<String>,
    Vec<LoadedCapability>,
);

/// Resolve one source to its capabilities + lock metadata. `Ok(None)` means the
/// source resolved to nothing (already warned) and is skipped.
fn resolve_source(
    source: &Source,
    workdir: &Path,
    lock: Option<&Lock>,
    warnings: &mut Vec<String>,
) -> Result<Option<ResolvedSource>, String> {
    match source {
        Source::Local(l) => {
            let caps = crate::manifest::discover(&workdir.join(&l.path))?;
            Ok(Some((l.path.clone(), "local", None, None, caps)))
        }
        Source::Git(g) => {
            let pin = lock.and_then(|lk| lk.pinned_rev(&g.url));
            let (checkout_dir, sha) = git_sync(&g.url, pin.unwrap_or(&g.rev), workdir)?;
            let scan = match &g.subdir {
                Some(s) => checkout_dir.join(s),
                None => checkout_dir,
            };
            let caps = crate::manifest::discover(&scan)?;
            Ok(Some((
                g.url.clone(),
                "git",
                Some(sha),
                g.subdir.clone(),
                caps,
            )))
        }
        Source::Registry(r) => resolve_registry(r, workdir, lock, warnings),
    }
}

/// Resolve a registry source: load its index, find the named entry, and resolve
/// the real (local/git) source it points to — one point of indirection over many
/// repos. Provenance is recorded on the lock as kind `registry`, origin
/// `index#name`. Nested registries are refused (loudly).
fn resolve_registry(
    r: &RegistrySource,
    workdir: &Path,
    lock: Option<&Lock>,
    warnings: &mut Vec<String>,
) -> Result<Option<ResolvedSource>, String> {
    let Some(index_rel) = &r.index else {
        warnings.push(format!(
            "registry source '{}' has no `index` — skipped (set an index path to resolve it)",
            r.name
        ));
        return Ok(None);
    };
    let index = match RegistryIndex::load(&workdir.join(index_rel)) {
        Ok(i) => i,
        Err(e) => {
            warnings.push(format!("registry index '{index_rel}': {e}"));
            return Ok(None);
        }
    };
    let want = r.version.as_deref();
    let Some(entry) = index.find(&r.name, want) else {
        warnings.push(format!(
            "registry '{index_rel}' has no capability '{}'{}",
            r.name,
            want.map(|v| format!("@{v}")).unwrap_or_default()
        ));
        return Ok(None);
    };
    if matches!(entry.source, Source::Registry(_)) {
        warnings.push(format!(
            "registry entry '{}' points to another registry — nested registries are not resolved",
            r.name
        ));
        return Ok(None);
    }
    // Resolve the leaf source, then relabel it with registry provenance (the
    // resolved git rev, if any, is still recorded for auditability).
    let origin = format!("{index_rel}#{}", r.name);
    Ok(resolve_source(&entry.source, workdir, lock, warnings)?
        .map(|(_, _, rev, subdir, caps)| (origin, "registry", rev, subdir, caps)))
}

/// Resolve the dependency graph over the composed set: warn on unmet
/// dependencies and cycles (loud, non-fatal — nothing is dropped), and return the
/// set ordered so every dependency precedes the capabilities that declare it (a
/// stable topological sort; ties keep source order, so a dependency-free set is
/// returned unchanged).
///
/// Unmet dependencies are reported transitively for free: an intermediate
/// dependency's own missing dependencies surface when that capability is visited.
fn resolve_dependencies(
    caps: Vec<LoadedCapability>,
    present: &[String],
    warnings: &mut Vec<String>,
) -> Vec<LoadedCapability> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    for cap in &caps {
        for dep in &cap.manifest.dependencies {
            if dep != &cap.manifest.id && !present.contains(dep) {
                warnings.push(format!(
                    "capability '{}' depends on '{dep}', which no source provides",
                    cap.manifest.id
                ));
            }
        }
    }

    let n = caps.len();
    let index: HashMap<&str, usize> = caps
        .iter()
        .enumerate()
        .map(|(i, c)| (c.manifest.id.as_str(), i))
        .collect();
    // Edge dependency → dependent; a node's in-degree is its count of present deps.
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, c) in caps.iter().enumerate() {
        for dep in &c.manifest.dependencies {
            if let Some(&j) = index.get(dep.as_str()) {
                if j != i {
                    dependents[j].push(i);
                    in_degree[i] += 1;
                }
            }
        }
    }

    // Kahn's algorithm with a stable, lowest-source-index-first tie-break.
    let mut ready: BinaryHeap<Reverse<usize>> =
        (0..n).filter(|&i| in_degree[i] == 0).map(Reverse).collect();
    let mut order = Vec::with_capacity(n);
    let mut emitted = vec![false; n];
    while let Some(Reverse(i)) = ready.pop() {
        order.push(i);
        emitted[i] = true;
        for &d in &dependents[i] {
            in_degree[d] -= 1;
            if in_degree[d] == 0 {
                ready.push(Reverse(d));
            }
        }
    }

    // Any node never reaching in-degree 0 is part of a cycle: report it and keep
    // it (in source order) rather than dropping it.
    if order.len() < n {
        let cyclic: Vec<&str> = (0..n)
            .filter(|&i| !emitted[i])
            .map(|i| caps[i].manifest.id.as_str())
            .collect();
        warnings.push(format!(
            "dependency cycle among [{}] — kept in source order",
            cyclic.join(", ")
        ));
        for (i, done) in emitted.iter().enumerate() {
            if !done {
                order.push(i);
            }
        }
    }

    let mut slots: Vec<Option<LoadedCapability>> = caps.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
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
