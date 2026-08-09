//! Profiles, sources, and the resolved lockfile (#15) — the L4 "compose a
//! capability set from several places, then pin it" layer.
//!
//! A **profile** is a named set of **sources** targeting a set of harnesses. A
//! source resolves to capabilities from a **local path**, a **git repo**
//! (personal-repo sync — cloned/fetched and pinned to a commit), an **archive**
//! fetched by URL and pinned by content digest, or a **registry** (#21) — a JSON
//! index mapping names to their real local/git/archive source, one point of
//! indirection over many repos. The index itself may be a local path, a
//! `file://` URL, or fetched over `http(s)://` (a CDN-hosted catalog; see
//! `docs/src/distribution.md`). Resolving a profile
//! composes the sources in order and writes an `open-harness.lock` pinning every
//! resolved capability (id + version + content fingerprint), every git source's
//! exact commit, and every archive's sha256 — so a re-resolve is reproducible.
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
use std::time::Duration;

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

/// Where a set of capabilities comes from.
///
/// Written either as **one string** — `"git+https://github.com/me/caps@v1"`,
/// `"./capabilities"` — whose kind is inferred (see [`Source::parse_spec`]), or
/// as an explicit tagged object when the inference needs overriding:
/// `{"local": {"path": "capabilities"}}`,
/// `{"git": {"url": "...", "rev": "main", "subdir": "capabilities"}}`,
/// `{"http": {"url": "https://cdn/….tar", "sha256": "…"}}`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Local(LocalSource),
    Git(GitSource),
    Http(HttpSource),
    Registry(RegistrySource),
}

/// The tagged spelling, kept separate so [`Source`]'s own `Deserialize` can also
/// accept the one-string form.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum SourceRepr {
    Local(LocalSource),
    Git(GitSource),
    Http(HttpSource),
    Registry(RegistrySource),
}

impl From<SourceRepr> for Source {
    fn from(r: SourceRepr) -> Source {
        match r {
            SourceRepr::Local(s) => Source::Local(s),
            SourceRepr::Git(s) => Source::Git(s),
            SourceRepr::Http(s) => Source::Http(s),
            SourceRepr::Registry(s) => Source::Registry(s),
        }
    }
}

impl<'de> Deserialize<'de> for Source {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Dispatched on the shape rather than with `#[serde(untagged)]`: untagged
        // reports only "data did not match any variant", discarding the tagged
        // form's own error — so a `http` source missing its `sha256` would lose
        // the one message that says what to fix.
        let value = serde_json::Value::deserialize(d)?;
        match value {
            serde_json::Value::String(s) => {
                Source::parse_spec(&s).map_err(serde::de::Error::custom)
            }
            other => serde_json::from_value::<SourceRepr>(other)
                .map(Into::into)
                .map_err(serde::de::Error::custom),
        }
    }
}

impl Source {
    /// Infer a source from a single spec string — so a URL can be given as-is,
    /// without also naming the kind it obviously is.
    ///
    /// The rules, in order:
    ///
    /// | Spec looks like | Becomes |
    /// |---|---|
    /// | `git+<anything>` | git (the prefix is an explicit override) |
    /// | `ssh://…`, `git://…`, `user@host:path` | git |
    /// | a path/URL ending `.git` | git |
    /// | `http(s)://….tar#sha256=<hex>` | a digest-pinned archive |
    /// | `http(s)://….json#name=<cap>` | a registry index |
    /// | any other `http(s)://` | git (a forge repo URL) |
    /// | a `file://` directory, or anything else | local |
    ///
    /// Two kinds cannot be inferred from a URL alone, because the URL is missing
    /// a fact the source requires: an archive needs its `sha256` and a registry
    /// needs the capability `name` to select. Both are supplied in the fragment
    /// (pip spells a direct-URL digest `#sha256=` the same way); without it the
    /// spec is **refused with the fix in the message** rather than guessed at.
    pub fn parse_spec(spec: &str) -> Result<Source, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("empty source spec".to_string());
        }
        // An explicit `git+` prefix always wins — the one way to force a git
        // source for something that would otherwise look local or ambiguous.
        if spec.starts_with("git+") {
            return GitSource::parse_spec(spec).map(Source::Git);
        }

        let (locator, fragment) = split_fragment(spec);
        let path_part = locator.rsplit('@').next().unwrap_or(locator);

        if spec.starts_with("ssh://") || spec.starts_with("git://") || is_scp_like(locator) {
            return GitSource::parse_spec(spec).map(Source::Git);
        }
        if strip_rev(locator).trim_end_matches('/').ends_with(".git") {
            return GitSource::parse_spec(spec).map(Source::Git);
        }

        if is_remote_url(spec) || locator.starts_with("file://") {
            if let Some(sha) = fragment_value(fragment, "sha256") {
                return Ok(Source::Http(HttpSource {
                    url: locator.to_string(),
                    sha256: sha,
                    subdir: fragment_value(fragment, "subdirectory"),
                }));
            }
            if let Some(name) = fragment_value(fragment, "name") {
                return Ok(Source::Registry(RegistrySource {
                    name,
                    version: fragment_value(fragment, "version"),
                    index: Some(locator.to_string()),
                }));
            }
            if path_part.ends_with(".tar") {
                return Err(format!(
                    "'{spec}' looks like a capability archive, which must be pinned: add its \
                     digest as `#sha256=<hex>` (an unpinned remote artifact cannot be verified)"
                ));
            }
            if path_part.ends_with(".json") {
                return Err(format!(
                    "'{spec}' looks like a registry index, which selects one capability by name: \
                     add `#name=<capability>` (or use the tagged form to pin a version)"
                ));
            }
            // A `file://` directory is used in place, like any other path; a
            // remote URL that is none of the above is a repository.
            if let Some(path) = file_url_path(locator)? {
                return Ok(Source::Local(LocalSource {
                    path: path.to_string_lossy().into_owned(),
                }));
            }
            return GitSource::parse_spec(spec).map(Source::Git);
        }

        // Anything left is a local path. Deliberately **not** checked for
        // existence here: a `local` path is relative to the *profile's* workdir,
        // not to wherever the process happens to be, so testing it now would ask
        // the wrong directory — and a profile may legitimately name a directory
        // that is created later. A missing path is reported at resolve time,
        // where the workdir is known.
        Ok(Source::Local(LocalSource {
            path: locator.to_string(),
        }))
    }
}

/// Split `…#fragment` off a spec.
fn split_fragment(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('#') {
        Some((l, f)) => (l, Some(f)),
        None => (spec, None),
    }
}

/// Drop a trailing `@rev` (only when it follows the last `/`, so ssh userinfo
/// survives — same rule as [`GitSource::parse_spec`]).
fn strip_rev(locator: &str) -> &str {
    let last_slash = locator.rfind('/');
    match locator.rfind('@') {
        Some(at) if last_slash.is_none_or(|s| at > s) => &locator[..at],
        _ => locator,
    }
}

/// `key=value` lookup in a `#a=1&b=2` fragment; a bare fragment has no keys.
fn fragment_value(fragment: Option<&str>, key: &str) -> Option<String> {
    let f = fragment?;
    f.split('&')
        .find_map(|part| part.strip_prefix(key)?.strip_prefix('=').map(String::from))
        .filter(|v| !v.is_empty())
}

/// scp-style `[user@]host:path` — a colon before any slash, and no `://`.
fn is_scp_like(locator: &str) -> bool {
    if locator.contains("://") {
        return false;
    }
    match locator.find(':') {
        // A Windows drive letter (`C:\…`) is a path, not a host.
        Some(i) => i > 1 && locator[..i].contains('.') || (i > 1 && locator[..i].contains('@')),
        None => false,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalSource {
    /// Directory of `*/capability.json`, relative to the profile's workdir.
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
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

impl GitSource {
    /// Parse a **pip-style git requirement** — the way a Python package is
    /// installed from a repo:
    ///
    /// ```text
    /// [git+]<url>[@<rev>][#subdirectory=<path>]
    ///
    /// git+https://github.com/acme/caps@v1.2.0#subdirectory=capabilities/commit-style
    /// git+ssh://git@github.com/acme/caps@main
    /// https://github.com/acme/caps.git
    /// /srv/mirrors/caps@v1.0.0
    /// ```
    ///
    /// The `git+` prefix is optional, `@<rev>` defaults to `HEAD`, and the
    /// fragment accepts pip's `subdirectory=<path>` as well as a bare `<path>`.
    pub fn parse_spec(spec: &str) -> Result<GitSource, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("empty git spec".to_string());
        }
        let rest = spec.strip_prefix("git+").unwrap_or(spec);

        // The fragment comes off first, so a `#` payload can never be mistaken
        // for part of the rev.
        let (locator, fragment) = match rest.split_once('#') {
            Some((l, f)) => (l, Some(f)),
            None => (rest, None),
        };
        let subdir = fragment
            .map(|f| f.strip_prefix("subdirectory=").unwrap_or(f))
            .map(|p| p.trim_matches('/').to_string())
            .filter(|p| !p.is_empty());

        // A rev is the text after the last `@` — but only when that `@` falls
        // after the last `/`, so `ssh://git@host/repo` keeps its userinfo
        // instead of reading `git@host/repo` as a revision.
        let last_slash = locator.rfind('/');
        let (url, rev) = match locator.rfind('@') {
            Some(at) if last_slash.is_none_or(|s| at > s) => {
                (&locator[..at], Some(locator[at + 1..].to_string()))
            }
            _ => (locator, None),
        };
        if url.is_empty() {
            return Err(format!("git spec '{spec}' has no URL"));
        }
        if rev.as_deref().is_some_and(str::is_empty) {
            return Err(format!("git spec '{spec}' has an empty revision after '@'"));
        }
        Ok(GitSource {
            url: url.to_string(),
            rev: rev.unwrap_or_else(default_rev),
            subdir,
        })
    }

    /// The repository's own name, used to identify a repo that *is* one
    /// capability (see [`crate::manifest::discover_named`]).
    fn repo_name(&self) -> String {
        let trimmed = self.url.trim_end_matches('/').replace('\\', "/");
        let last = trimmed.rsplit('/').next().unwrap_or(&trimmed);
        last.strip_suffix(".git").unwrap_or(last).to_string()
    }
}

/// A git source is written either as an object or as a single pip-style spec
/// string: `{"git": {"url": "…", "rev": "v1"}}` or `{"git": "git+https://…@v1"}`.
impl<'de> Deserialize<'de> for GitSource {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Full {
            url: String,
            #[serde(default = "default_rev")]
            rev: String,
            #[serde(default)]
            subdir: Option<String>,
        }
        // Shape dispatch, not `untagged` — see the note on `Source`.
        let value = serde_json::Value::deserialize(d)?;
        match value {
            serde_json::Value::String(s) => {
                GitSource::parse_spec(&s).map_err(serde::de::Error::custom)
            }
            other => serde_json::from_value::<Full>(other)
                .map(|f| GitSource {
                    url: f.url,
                    rev: f.rev,
                    subdir: f.subdir,
                })
                .map_err(serde::de::Error::custom),
        }
    }
}

/// A capability archive fetched by URL and pinned by content digest — the
/// CDN-hosted leaf (see `docs/src/distribution.md`). The archive is an
/// uncompressed tar; `crate::archive` explains why compression is refused.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HttpSource {
    /// Archive URL: `http(s)://` (fetched) or `file://` (read).
    pub url: String,
    /// Expected `sha256` of the archive bytes, hex — with or without a
    /// `sha256:` prefix.
    ///
    /// **Required, deliberately.** Transport security authenticates the *hop*,
    /// not the *artifact*; without a pin there is nothing to compare a download
    /// against, so an unpinned remote source would be trust-by-URL. Making the
    /// field mandatory means a profile cannot express that by accident.
    pub sha256: String,
    /// Subdirectory within the unpacked archive to scan. Defaults to its root.
    #[serde(default)]
    pub subdir: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistrySource {
    /// Capability/pack name to resolve from the registry index.
    pub name: String,
    /// Optional exact version to select (else the first matching entry).
    #[serde(default)]
    pub version: Option<String>,
    /// Where the registry index (JSON) lives: an `http(s)://` URL, a `file://`
    /// URL, or a path relative to the profile workdir. The index maps names →
    /// real sources (local/git), giving one point of indirection over many
    /// repos. Without it the source is a loud no-op.
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

/// How long to wait on a remote registry index before giving up.
const INDEX_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait on a capability archive before giving up. Longer than the
/// index: an archive is a payload, not a small JSON catalog.
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// Is `spec` a URL that must be *fetched* (rather than read from disk)?
fn is_remote_url(spec: &str) -> bool {
    spec.starts_with("http://") || spec.starts_with("https://")
}

/// The filesystem path a `file://` URL names, or `None` when `spec` is not one.
///
/// Percent-escapes are **not** decoded — a location whose path needs escaping
/// should be given as a bare path instead.
fn file_url_path(spec: &str) -> Result<Option<PathBuf>, String> {
    let Some(rest) = spec.strip_prefix("file://") else {
        return Ok(None);
    };
    // `file://localhost/p` names the same file as `file:///p`.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if rest.is_empty() {
        return Err(format!("malformed file URL '{spec}' (no path)"));
    }
    // A Windows drive path arrives as `/C:/x`; drop the leading slash.
    let trimmed = rest.strip_prefix('/').unwrap_or(rest);
    let bytes = trimmed.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    Ok(Some(PathBuf::from(if drive { trimmed } else { rest })))
}

impl RegistryIndex {
    /// Load the index named by `spec`: an `http(s)://` URL (fetched), a `file://`
    /// URL, or a path relative to `workdir`.
    fn load(spec: &str, workdir: &Path) -> Result<RegistryIndex, String> {
        let text = Self::read_text(spec, workdir)?;
        serde_json::from_str(&text).map_err(|e| format!("invalid registry index: {e}"))
    }

    fn read_text(spec: &str, workdir: &Path) -> Result<String, String> {
        if is_remote_url(spec) {
            let headers = [("Accept".to_string(), "application/json".to_string())];
            // The transport reports *that* TLS is missing; the remedy here is a
            // local index or a git source, not the MCP bridge's advice.
            let resp = crate::http::get(spec, &headers, INDEX_TIMEOUT).map_err(|e| match e {
                crate::http::Error::NoTls { host } => format!(
                    "this build has no TLS: cannot fetch the registry index from https://{host}. \
                     Rebuild with `--features http-tls`, or point `index` at a file:// URL, a \
                     local path, or a git source."
                ),
                other => format!("fetch registry index '{spec}': {other}"),
            })?;
            return Ok(resp.into_text());
        }
        let path = match file_url_path(spec)? {
            Some(p) => p,
            None => workdir.join(spec),
        };
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))
    }

    fn find(&self, name: &str, version: Option<&str>) -> Option<&RegistryEntry> {
        self.capabilities
            .iter()
            .find(|e| e.name == name && (version.is_none() || e.version.as_deref() == version))
    }
}

/// Profile filenames looked for in a directory, most specific first. A project
/// keeps exactly one; `oh` finds it so no command needs `--profile` spelled out.
pub const PROFILE_NAMES: &[&str] = &[
    "oh.yaml",
    "oh.yml",
    "harness.yaml",
    "harness.yml",
    "open-harness.yaml",
    "open-harness.yml",
    "open-harness.json",
    "oh.json",
];

/// The profile filename written when a command creates one.
pub const DEFAULT_PROFILE_NAME: &str = "oh.yaml";

/// Find the profile in `dir`, if there is one. Returns the first match in
/// [`PROFILE_NAMES`] order.
pub fn find_profile(dir: &Path) -> Option<PathBuf> {
    PROFILE_NAMES
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

/// Every profile present in `dir` — used to report an ambiguous project rather
/// than silently picking one.
pub fn all_profiles(dir: &Path) -> Vec<PathBuf> {
    PROFILE_NAMES
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.is_file())
        .collect()
}

/// Does this path name a YAML profile (by extension)?
fn is_yaml_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml") | Some("yml")
    )
}

impl Profile {
    pub fn from_json(text: &str) -> Result<Profile, String> {
        serde_json::from_str(text).map_err(|e| format!("invalid profile: {e}"))
    }

    /// Parse a YAML profile. JSON is a subset of YAML, so this also accepts a
    /// JSON document — the extension only decides how a profile is *written*.
    pub fn from_yaml(text: &str) -> Result<Profile, String> {
        let map =
            crate::yaml::parse_frontmatter(text).map_err(|e| format!("invalid profile: {e}"))?;
        serde_json::from_value(serde_json::Value::Object(map))
            .map_err(|e| format!("invalid profile: {e}"))
    }

    pub fn load(path: &Path) -> Result<Profile, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if is_yaml_path(path) {
            Profile::from_yaml(&text)
        } else {
            Profile::from_json(&text)
        }
        .map_err(|e| format!("{}: {e}", path.display()))
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
            // The checkout lives under a cache slug, so if the scanned root *is*
            // the capability, name it after the repository rather than the slug.
            let caps = crate::manifest::discover_named(&scan, Some(&g.repo_name()))?;
            Ok(Some((
                g.url.clone(),
                "git",
                Some(sha),
                g.subdir.clone(),
                caps,
            )))
        }
        Source::Http(h) => resolve_http(h, workdir, warnings),
        Source::Registry(r) => resolve_registry(r, workdir, lock, warnings),
    }
}

/// Resolve an archive source: fetch, **verify the digest before unpacking**,
/// unpack into a content-addressed cache, and scan the result.
///
/// The two failure modes are deliberately not alike. A transport failure is a
/// loud skip — the artifact may be momentarily unreachable, and other sources
/// should still compose. A **digest mismatch is tamper evidence**: the bytes are
/// not what the profile pinned, so it stops the whole resolve rather than
/// degrading into a warning someone can scroll past.
fn resolve_http(
    h: &HttpSource,
    workdir: &Path,
    warnings: &mut Vec<String>,
) -> Result<Option<ResolvedSource>, String> {
    let digest =
        normalize_digest(&h.sha256).map_err(|e| format!("http source '{}': {e}", h.url))?;

    // Content-addressed, so the digest *is* the cache key: an artifact that is
    // already unpacked can never be a stale copy of a different one.
    let root = workdir.join(".open-harness").join("cache").join("http");
    let tree = root.join(&digest);
    let marker = root.join(format!("{digest}.ok"));

    if !marker.is_file() {
        let bytes = match fetch_archive(&h.url) {
            Ok(b) => b,
            Err(e) => {
                warnings.push(format!("http source '{}': {e}", h.url));
                return Ok(None);
            }
        };
        let actual = crate::trust::sha256_hex(&bytes);
        if actual != digest {
            return Err(format!(
                "http source '{}': content digest mismatch — pinned sha256:{digest}, \
                 downloaded sha256:{actual}. Refusing to unpack an artifact that is not \
                 what the profile pinned.",
                h.url
            ));
        }
        // Unpack to a sibling and swap in, so an interrupted unpack cannot leave
        // a half-written tree that a later run mistakes for complete. The marker
        // is written last: it is the only completion signal.
        let staging = root.join(format!("{digest}.unpacking"));
        let _ = std::fs::remove_dir_all(&staging);
        crate::archive::unpack_tar(&bytes, &staging)
            .map_err(|e| format!("http source '{}': {e}", h.url))?;
        let _ = std::fs::remove_dir_all(&tree);
        std::fs::rename(&staging, &tree)
            .map_err(|e| format!("http source '{}': install unpacked archive: {e}", h.url))?;
        std::fs::write(&marker, format!("sha256:{digest}\n"))
            .map_err(|e| format!("http source '{}': write cache marker: {e}", h.url))?;
    }

    let scan = match &h.subdir {
        Some(s) => tree.join(s),
        None => tree,
    };
    let caps = crate::manifest::discover(&scan)?;
    Ok(Some((
        h.url.clone(),
        "http",
        Some(format!("sha256:{digest}")),
        h.subdir.clone(),
        caps,
    )))
}

/// Read an archive from an `http(s)://` URL (fetched) or a `file://` URL.
///
/// Unlike a registry index, a bare path is **not** accepted: an archive source
/// exists to name a fetchable artifact, and a local directory of capabilities is
/// already spelled `{"local": …}`.
fn fetch_archive(url: &str) -> Result<Vec<u8>, String> {
    if is_remote_url(url) {
        let resp = crate::http::get(url, &[], ARCHIVE_TIMEOUT).map_err(|e| match e {
            crate::http::Error::NoTls { host } => format!(
                "this build has no TLS: cannot fetch the archive from https://{host}. \
                 Rebuild with `--features http-tls`, or point `url` at a file:// URL, or use \
                 a git or local source."
            ),
            other => format!("fetch archive: {other}"),
        })?;
        return Ok(resp.into_bytes());
    }
    let path = file_url_path(url)?.ok_or_else(|| {
        format!("unsupported archive URL '{url}' (want http://, https://, or file://)")
    })?;
    std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))
}

/// Normalize a pinned digest to bare lowercase hex, rejecting anything that is
/// not a full sha256.
fn normalize_digest(s: &str) -> Result<String, String> {
    let hex = s
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(s.trim())
        .to_ascii_lowercase();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "`sha256` must be 64 hex characters (optionally 'sha256:'-prefixed), got '{s}'"
        ));
    }
    Ok(hex)
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
    let Some(index_spec) = &r.index else {
        warnings.push(format!(
            "registry source '{}' has no `index` — skipped (set an index path to resolve it)",
            r.name
        ));
        return Ok(None);
    };
    let index = match RegistryIndex::load(index_spec, workdir) {
        Ok(i) => i,
        Err(e) => {
            warnings.push(format!("registry index '{index_spec}': {e}"));
            return Ok(None);
        }
    };
    let want = r.version.as_deref();
    let Some(entry) = index.find(&r.name, want) else {
        warnings.push(format!(
            "registry '{index_spec}' has no capability '{}'{}",
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
    // A *remote* index must not be able to select paths on this machine: the
    // index is written elsewhere, so a `local` entry would resolve against the
    // consumer's workdir. Refuse loudly rather than read a path a remote file
    // chose.
    if is_remote_url(index_spec) {
        if let Source::Local(l) = &entry.source {
            warnings.push(format!(
                "registry entry '{}' from remote index '{index_spec}' points at local path '{}' \
                 — refused (a remote index must not select paths on this machine)",
                r.name, l.path
            ));
            return Ok(None);
        }
    }
    // Resolve the leaf source, then relabel it with registry provenance (the
    // resolved git rev, if any, is still recorded for auditability).
    let origin = format!("{index_spec}#{}", r.name);
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
