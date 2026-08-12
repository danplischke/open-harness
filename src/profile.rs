//! Profiles, sources, and the resolved lockfile (#15) — the L4 "compose a
//! capability set from several places, then pin it" layer.
//!
//! A **profile** is a named set of **sources** targeting a set of harnesses. A
//! source resolves to capabilities from a **local path**, a **git repo**
//! (personal-repo sync — cloned/fetched and pinned to a commit), an **archive**
//! fetched by URL and pinned by content digest, or a **registry** (#21) — an
//! index mapping names to their real local/git/archive source, one point of
//! indirection over many repos. The index itself may be a local path, a
//! `file://` URL, or fetched over `http(s)://` (a CDN-hosted catalog; see
//! `docs/src/distribution.md`). Resolving a profile composes the sources in
//! order and writes an `open-harness.lock` pinning every resolved capability
//! (qualified name + version + a sha256 over its file tree), every git source's
//! exact commit, and every archive's sha256 — so a re-resolve is reproducible.
//!
//! Because those sources are other people's repositories, identity is a
//! **qualified name** (`<namespace>/<id>`, the namespace derived from the
//! source), not a bare id that two authors could both claim.
//!
//! Composition then resolves the **dependency graph** (#23) — see
//! [`crate::deps`] for the requirement vocabulary. Capabilities declare
//! versioned `requires` / `suggests` / `conflicts` / `replaces` edges; sources
//! stay a preference order that a requirement may veto but never reorder; unmet
//! edges, version conflicts (naming who asked for what), cycles, live conflicts,
//! and dependencies Unsupported on a target harness are all reported loudly and
//! non-fatally; and the resolved graph is pinned in the lock.
//!
//! Following a dependency's *own* declared source is **opt-in**
//! ([`Resolution::Transitive`]) and gated on a signature by default
//! ([`TransitiveTrust`]) — acquiring a capability the user never named means
//! running code they never chose, with the agent's privileges.
//!
//! This is the sourcing layer beneath `oh sync` (#16): `sync --profile` resolves
//! here, then hands the composed capabilities + target harnesses to `sync::apply`.
//!
//! Git access shells out to the system `git` (the crate stays dependency-light);
//! a source `url` may be a remote or a local path, so the whole flow is testable
//! offline against a throwaway local repo.

use crate::adapters::Harness;
use crate::deps::{Relation, Requirement, Version};
use crate::manifest::LoadedCapability;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Default lockfile name, written next to the profile. Extension-less by
/// convention (like `Cargo.lock`); the contents are YAML.
pub const LOCK_NAME: &str = "open-harness.lock";

/// The profile filename without its extension. [`crate::config::find`] probes
/// `open-harness.yaml`, `open-harness.yml`, then a legacy `open-harness.json`.
pub const PROFILE_STEM: &str = "open-harness";

/// The canonical profile filename, for messages and `oh init`.
pub const PROFILE_NAME: &str = "open-harness.yaml";

/// Profile stems looked for in a project directory, in order. `open-harness` is
/// canonical; the shorter names are accepted because a project-root config file
/// is read far more often than it is written, and `oh.yaml` is what most people
/// reach for. Each stem is probed through [`crate::config::find`], so `.yaml`,
/// `.yml`, and a legacy `.json` all resolve.
pub const PROFILE_STEMS: &[&str] = &[PROFILE_STEM, "oh", "harness"];

/// Find the project's profile in `dir`, if there is exactly one to find.
pub fn find_profile(dir: &Path) -> Option<PathBuf> {
    all_profiles(dir).into_iter().next()
}

/// Every profile present in `dir`, so an ambiguous project can be reported
/// rather than silently resolved by picking one.
pub fn all_profiles(dir: &Path) -> Vec<PathBuf> {
    PROFILE_STEMS
        .iter()
        .filter_map(|stem| crate::config::find(&dir.join(stem)))
        .collect()
}

// ---- profile + source model ----------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Profile {
    #[serde(default = "default_profile_name")]
    pub name: String,
    /// Target harness ids for this profile (what `sync` fans out to).
    #[serde(default)]
    pub harnesses: Vec<String>,
    pub sources: Vec<Source>,
    /// What to do about a `requires` edge no declared source satisfies.
    #[serde(default)]
    pub resolution: Resolution,
    /// What a **transitively acquired** capability must prove before it is
    /// composed. Ignored under `resolution: flat`, where nothing is acquired.
    #[serde(default)]
    pub transitive_trust: TransitiveTrust,
    /// Trust store consulted by that gate, relative to the profile's workdir.
    /// Without one, only `transitive_trust: any` can admit anything — an empty
    /// store trusts no key, which is the correct default, not a bug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
}

fn default_profile_name() -> String {
    "default".to_string()
}

/// How a dependency that no declared source provides is handled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    /// Only the sources the profile names are used; an unmet dependency is
    /// reported and nothing is fetched. **The default**, deliberately: a
    /// capability runs with your agent's privileges, so "installing one thing
    /// silently clones arbitrary repositories" should be something you ask for.
    #[default]
    Flat,
    /// Additionally fetch the sources capabilities declare for their own
    /// dependencies, subject to [`TransitiveTrust`].
    Transitive,
}

/// What a transitively acquired capability must prove.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransitiveTrust {
    /// A valid signature by a key already in the trust store. The default.
    #[default]
    Trusted,
    /// Also admit a valid signature by an *unknown* key — trust-on-first-use.
    /// The content is still verified; only the signer is new.
    Tofu,
    /// Admit anything, including unsigned code. Never the default, and every
    /// admission is reported.
    Any,
}

/// Where a set of capabilities comes from.
///
/// Written either as **one string** — `git+https://github.com/me/caps@v1`,
/// `./capabilities` — whose kind is inferred (see [`Source::parse_spec`]), or as
/// an explicit tagged mapping when inference needs overriding or a field it
/// cannot carry (`select`, `version`) is needed:
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
    Plugin(PluginSource),
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
    Plugin(PluginSource),
}

impl From<SourceRepr> for Source {
    fn from(r: SourceRepr) -> Source {
        match r {
            SourceRepr::Local(s) => Source::Local(s),
            SourceRepr::Git(s) => Source::Git(s),
            SourceRepr::Http(s) => Source::Http(s),
            SourceRepr::Registry(s) => Source::Registry(s),
            SourceRepr::Plugin(s) => Source::Plugin(s),
        }
    }
}

impl<'de> Deserialize<'de> for Source {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Dispatched on the value's shape rather than with `#[serde(untagged)]`:
        // untagged reports only "data did not match any variant", discarding the
        // tagged form's own error — so a `http` source missing its `sha256` would
        // lose the one message that says what to fix.
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
    /// | `http(s)://…#sha256=<hex>` | a digest-pinned archive |
    /// | `http(s)://…#name=<cap>` | a registry index |
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
        let (path_part, _) = split_rev(locator);

        if spec.starts_with("ssh://") || spec.starts_with("git://") || is_scp_like(locator) {
            return GitSource::parse_spec(spec).map(Source::Git);
        }
        if path_part.trim_end_matches('/').ends_with(".git") {
            return GitSource::parse_spec(spec).map(Source::Git);
        }

        if is_remote_url(spec) || locator.starts_with("file://") {
            if let Some(sha) = fragment_value(fragment, "sha256") {
                return Ok(Source::Http(HttpSource {
                    url: locator.to_string(),
                    sha256: sha,
                    subdir: fragment_value(fragment, "subdirectory"),
                    select: Select::default(),
                }));
            }
            if let Some(name) = fragment_value(fragment, "name") {
                return Ok(Source::Registry(RegistrySource {
                    name,
                    version: fragment_value(fragment, "version"),
                    index: Some(locator.to_string()),
                    select: Select::default(),
                }));
            }
            if path_part.ends_with(".tar") {
                return Err(format!(
                    "'{spec}' looks like a capability archive, which must be pinned: add its \
                     digest as `#sha256=<hex>` (an unpinned remote artifact cannot be verified)"
                ));
            }
            if path_part.ends_with(".json")
                || path_part.ends_with(".yaml")
                || path_part.ends_with(".yml")
            {
                return Err(format!(
                    "'{spec}' looks like a registry index, which selects one capability by name: \
                     add `#name=<capability>` (or use the mapping form to pin a version)"
                ));
            }
            // A `file://` directory is used in place, like any other path; a
            // remote URL that is none of the above is a repository.
            if let Some(path) = file_url_path(locator)? {
                return Ok(Source::Local(LocalSource {
                    path: path.to_string_lossy().into_owned(),
                    select: Select::default(),
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
            select: Select::default(),
        }))
    }
}

/// A **harness-native plugin bundle** — a `.claude-plugin` directory — imported
/// into capabilities rather than read as one. See [`crate::plugin`] for what
/// travels and what does not.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginSource {
    /// Where the bundle lives: a git URL, or a local path when `rev` is absent.
    pub url: String,
    /// Branch / tag / SHA for a git bundle. Absent means `url` is a local path,
    /// so a plugin checked out beside your profile needs no git at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// Subdirectory of the repository holding the bundle. Usually unnecessary —
    /// a `marketplace.json` already says where each plugin lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// Which plugin to take, when the repository is a marketplace publishing
    /// several. A marketplace with one plugin resolves without this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Select::is_empty")]
    pub select: Select,
}

/// Which of a source's capabilities to take.
///
/// A source is usually somebody else's repository, and you rarely want all of
/// it — a plugin may ship nineteen skills when you want two. Without this the
/// only lever is `subdir`, which is an accident of how they laid the repo out
/// rather than a choice about what you need.
///
/// Filtering happens **before** the capabilities are locked, so the lockfile
/// records only what you selected: adding one later is a visible lock change
/// that `--locked` catches, not a silent widening.
///
/// `deny_unknown_fields` so `includes:` is a loud error rather than a filter
/// that silently selects everything.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Select {
    /// Patterns to keep. Empty means everything — the default, so an existing
    /// profile behaves exactly as before. Matched against the capability's
    /// qualified name *and* its bare id, as globs: `*` (spanning `/`), `?`, and
    /// character classes such as `[a-z]` / `[!abc]`. A malformed pattern is an
    /// error, not a literal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Patterns to drop. Applied after `include`, so exclude always wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Capability kinds to keep (`skill`, `hook`, …). Empty means all kinds.
    /// Useful for taking a plugin's skills without its harness-specific hooks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Pull back in any capability from this source that a selected one
    /// `requires`, even when the patterns excluded it. Off by default:
    /// silently widening a selection you deliberately narrowed is the wrong
    /// default, so an unmet dependency is reported instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_dependencies: bool,
}

impl Select {
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty() && self.kinds.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalSource {
    /// Directory of capabilities, relative to the profile's workdir.
    pub path: String,
    #[serde(default, skip_serializing_if = "Select::is_empty")]
    pub select: Select,
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
    #[serde(default, skip_serializing_if = "Select::is_empty")]
    pub select: Select,
}

impl GitSource {
    /// Parse a **pip-style git requirement** — the way a Python package is
    /// installed from a repo:
    ///
    /// ```text
    /// [git+]<url>[@<rev>][#subdirectory=<path>]
    ///
    /// git+https://github.com/acme/caps@v1.2.0#subdirectory=capabilities/x
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
        let (locator, fragment) = split_fragment(rest);
        let subdir = fragment
            .map(|f| f.strip_prefix("subdirectory=").unwrap_or(f))
            .map(|p| p.trim_matches('/').to_string())
            .filter(|p| !p.is_empty());

        let (url, rev) = split_rev(locator);
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
            select: Select::default(),
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

/// A git source is written either as a mapping or as a single pip-style spec
/// string: `{git: {url: …, rev: v1}}` or `git+https://…@v1`.
impl<'de> Deserialize<'de> for GitSource {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Full {
            url: String,
            #[serde(default = "default_rev")]
            rev: String,
            #[serde(default)]
            subdir: Option<String>,
            #[serde(default)]
            select: Select,
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
                    select: f.select,
                })
                .map_err(serde::de::Error::custom),
        }
    }
}

/// Split `…#fragment` off a spec.
fn split_fragment(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('#') {
        Some((l, f)) => (l, Some(f)),
        None => (spec, None),
    }
}

/// Split a trailing `@rev` off a locator — but only when the `@` falls after the
/// last `/`, so `ssh://git@host/repo` keeps its userinfo instead of resolving to
/// url `ssh://git` at rev `host/repo`.
fn split_rev(locator: &str) -> (&str, Option<String>) {
    let last_slash = locator.rfind('/');
    match locator.rfind('@') {
        Some(at) if last_slash.is_none_or(|s| at > s) => {
            (&locator[..at], Some(locator[at + 1..].to_string()))
        }
        _ => (locator, None),
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
        Some(i) => i > 1 && (locator[..i].contains('.') || locator[..i].contains('@')),
        None => false,
    }
}

fn default_rev() -> String {
    "HEAD".to_string()
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
    #[serde(default, skip_serializing_if = "Select::is_empty")]
    pub select: Select,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistrySource {
    /// Capability/pack name to resolve from the registry index.
    pub name: String,
    /// Optional exact version to select (else the first matching entry).
    #[serde(default)]
    pub version: Option<String>,
    /// Where the registry index (YAML, or legacy JSON) lives: an `http(s)://`
    /// URL, a `file://` URL, or a path relative to the profile workdir. The
    /// index maps names → real sources (local/git/archive), giving one point of
    /// indirection over many repos. Without it the source is a loud no-op.
    #[serde(default)]
    pub index: Option<String>,
    #[serde(default, skip_serializing_if = "Select::is_empty")]
    pub select: Select,
}

/// A registry index mapping capability names to their real source:
///
/// ```yaml
/// capabilities:
///   - name: pack
///     version: 1.0.0
///     source:
///       git:
///         url: https://github.com/acme/capabilities
///         subdir: caps
/// ```
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
    ///
    /// The format follows the spec's extension (YAML, or legacy JSON); a URL
    /// that ends in neither is tried as both, like any extension-less config.
    fn load(spec: &str, workdir: &Path) -> Result<RegistryIndex, String> {
        let text = Self::read_text(spec, workdir)?;
        let format = crate::config::Format::of(Path::new(spec));
        crate::config::from_str(&text, format).map_err(|e| format!("invalid registry index: {e}"))
    }

    fn read_text(spec: &str, workdir: &Path) -> Result<String, String> {
        if is_remote_url(spec) {
            // The index may be served as either; ask for both rather than
            // implying the catalog is still JSON-only.
            let headers = [(
                "Accept".to_string(),
                "application/yaml, application/json".to_string(),
            )];
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

impl Profile {
    /// Parse a profile from YAML (or legacy JSON) text.
    pub fn from_text(text: &str) -> Result<Profile, String> {
        crate::config::from_str(text, crate::config::Format::Either)
            .map_err(|e| format!("invalid profile: {e}"))
    }

    pub fn load(path: &Path) -> Result<Profile, String> {
        crate::config::load(path).map_err(|e| format!("invalid profile: {e}"))
    }

    /// The profile's target harnesses, or the first id it does not recognise.
    pub fn harness_set(&self) -> Result<Vec<Harness>, String> {
        self.harnesses
            .iter()
            .map(|id| Harness::from_id(id).ok_or_else(|| format!("unknown harness '{id}'")))
            .collect()
    }
}

// ---- lockfile -------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedCap {
    /// Fully-qualified name — `<namespace>/<id>`, or the bare id for an
    /// unnamespaced (local) source. This is the identity dependencies resolve
    /// against; `id` is only the local alias.
    pub name: String,
    pub id: String,
    pub version: String,
    pub kind: String,
    /// `sha256` over the capability's whole file tree — the same digest
    /// signatures are made over, so a changed body changes the lock.
    pub digest: String,
    /// The resolved dependency graph for this capability: qualified name → the
    /// exact version it resolved to. Pinned so the graph is reproducible and
    /// auditable, not just the individual capabilities.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
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
    /// The revision the profile *asked* for (branch / tag / SHA), as distinct
    /// from the commit it resolved to. Recorded so that editing `rev:` in the
    /// profile invalidates the pin instead of being silently overridden by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<String>,
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
    /// Render the lock as a YAML document.
    pub fn to_yaml(&self) -> String {
        crate::config::to_yaml(self).unwrap_or_else(|_| "{}\n".into())
    }

    /// Parse a lockfile from YAML (or a legacy JSON lockfile).
    pub fn from_text(text: &str) -> Result<Lock, String> {
        crate::config::from_str(text, crate::config::Format::Either)
            .map_err(|e| format!("invalid lockfile: {e}"))
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        crate::config::write(path, self)
    }

    /// The resolved rev pinned for a git source at `url`, if any.
    /// The commit pinned for the git source at `url`, **provided the profile is
    /// still asking for the same revision**.
    ///
    /// A lock exists so upstream movement does not change your build — that is
    /// why a re-resolve against a lock keeps `main` at the recorded commit even
    /// after `main` advances. But it must not also override the *profile*: if
    /// someone edits `rev: v1.0` to `rev: v2.0`, the pin has to give way, or the
    /// lock would silently keep serving v1.0 forever. Comparing the recorded
    /// request to the current one is what separates those two cases.
    ///
    /// A lockfile written before `requested` was recorded has `None` there; its
    /// pin is honored, so an older lock keeps working.
    fn pinned_rev(&self, url: &str, requested: &str) -> Option<&str> {
        let source = self
            .sources
            .iter()
            .find(|s| s.kind == "git" && s.origin == url)?;
        match source.requested.as_deref() {
            Some(recorded) if recorded != requested => None,
            _ => source.rev.as_deref(),
        }
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
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut locked_sources: Vec<LockedSource> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for (source_index, source) in profile.sources.iter().enumerate() {
        ingest_source(
            source,
            source_index,
            workdir,
            lock,
            None,
            &mut candidates,
            &mut locked_sources,
            &mut warnings,
        )?;
    }

    if profile.resolution == Resolution::Transitive {
        acquire_transitively(
            profile,
            workdir,
            lock,
            &mut candidates,
            &mut locked_sources,
            &mut warnings,
        )?;
    }

    let selection = select(candidates, &mut warnings);
    warn_on_bare_id_collisions(&selection.capabilities, &mut warnings);

    // Resolve the dependency graph: report unmet requirements (transitively),
    // conflicts and cycles — loudly, but non-fatally, so nothing is silently
    // dropped — and order the set so a dependency precedes its dependents.
    let graph = resolve_dependencies(selection.capabilities, &mut warnings);

    warn_on_per_harness_gaps(&graph.capabilities, &graph.edges, &harnesses, &mut warnings);
    record_resolved_dependencies(&mut locked_sources, &graph.edges);

    let lock = Lock {
        version: 1,
        profile: profile.name.clone(),
        sources: locked_sources,
    };
    Ok(Resolved {
        profile: profile.name.clone(),
        harnesses,
        capabilities: graph.capabilities,
        lock,
        warnings,
    })
}

// ---- transitive acquisition (opt-in) --------------------------------------

/// How many rounds of "fetch a dependency's own source" to run before giving
/// up. Deep enough for any realistic capability graph, bounded so a
/// mutually-referential pair of repos cannot spin forever.
const MAX_TRANSITIVE_ROUNDS: usize = 8;

/// The trust decision applied to capabilities the profile never named.
struct TrustGate {
    policy: TransitiveTrust,
    store: crate::trust::TrustStore,
}

impl TrustGate {
    /// May this capability be composed? `Err` carries the reason, which is
    /// reported and the capability skipped — refusing is the safe outcome, so
    /// it is never fatal to the whole resolve.
    fn admit(&self, cap: &LoadedCapability, origin: &str) -> Result<(), String> {
        use crate::trust::Verification;
        if self.policy == TransitiveTrust::Any {
            return Ok(());
        }
        let verdict = crate::trust::verify(&cap.dir, &self.store);
        let name = cap.qualified_name();
        match (&verdict, self.policy) {
            (Verification::Trusted { .. }, _) => Ok(()),
            (Verification::Untrusted { .. }, TransitiveTrust::Tofu) => Ok(()),
            _ => Err(format!(
                "refused transitively acquired '{name}' from {origin}: {} \
                 (transitive_trust: {}). Add the source to the profile explicitly, \
                 pin the author's key, or relax the policy — deliberately.",
                verdict.status(),
                match self.policy {
                    TransitiveTrust::Trusted => "trusted",
                    TransitiveTrust::Tofu => "tofu",
                    TransitiveTrust::Any => "any",
                }
            )),
        }
    }
}

/// Fetch sources that capabilities declare for their own unmet dependencies,
/// repeatedly, until nothing new appears.
///
/// This is the part of a package manager that is genuinely dangerous here: a
/// capability is code that runs with the agent's privileges, so acquiring one
/// the user never named is a supply-chain decision, not a convenience. Hence:
/// opt-in per profile, gated on a signature by default, and every refusal and
/// every acquisition reported.
fn acquire_transitively(
    profile: &Profile,
    workdir: &Path,
    lock: Option<&Lock>,
    candidates: &mut Vec<Candidate>,
    locked_sources: &mut Vec<LockedSource>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let store = match &profile.trust {
        Some(path) => crate::trust::TrustStore::load(&workdir.join(path))?,
        None => crate::trust::TrustStore::default(),
    };
    if profile.trust.is_none() && profile.transitive_trust != TransitiveTrust::Any {
        warnings.push(format!(
            "resolution: transitive with no `trust:` store and transitive_trust: {} — \
             an empty store trusts no key, so nothing can be acquired",
            match profile.transitive_trust {
                TransitiveTrust::Trusted => "trusted",
                TransitiveTrust::Tofu => "tofu",
                TransitiveTrust::Any => "any",
            }
        ));
    }
    let gate = TrustGate {
        policy: profile.transitive_trust,
        store,
    };

    // Sources acquired this way sort after every declared source, so a profile's
    // own sources always keep preference.
    let mut next_index = profile.sources.len();
    let mut attempted: Vec<String> = Vec::new();

    for round in 0..MAX_TRANSITIVE_ROUNDS {
        let wanted = unmet_with_sources(candidates, &attempted);
        if wanted.is_empty() {
            return Ok(());
        }
        for (name, source_value) in wanted {
            attempted.push(name.clone());
            let source: Source = match serde_json::from_value(source_value) {
                Ok(s) => s,
                Err(e) => {
                    warnings.push(format!(
                        "dependency '{name}' declares an unusable source: {e}"
                    ));
                    continue;
                }
            };
            let accepted = ingest_source(
                &source,
                next_index,
                workdir,
                lock,
                Some(&gate),
                candidates,
                locked_sources,
                warnings,
            )?;
            next_index += 1;
            if accepted > 0 {
                warnings.push(format!(
                    "acquired {accepted} capability(ies) transitively for '{name}' \
                     (round {})",
                    round + 1
                ));
            }
        }
    }
    warnings.push(format!(
        "transitive acquisition stopped after {MAX_TRANSITIVE_ROUNDS} rounds — \
         remaining dependencies are reported as unmet"
    ));
    Ok(())
}

/// Unmet `requires` edges that name a source to fetch them from, skipping any
/// already attempted (so a source that yields nothing is not retried forever).
fn unmet_with_sources(
    candidates: &[Candidate],
    attempted: &[String],
) -> Vec<(String, serde_json::Value)> {
    let present: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
    let mut out: Vec<(String, serde_json::Value)> = Vec::new();
    for c in candidates {
        for dep in c
            .cap
            .manifest
            .dependencies
            .with_relation(Relation::Requires)
        {
            let Some(source) = &dep.source else { continue };
            let satisfied = present.contains(&dep.name.as_str())
                || present
                    .iter()
                    .any(|n| n.rsplit('/').next() == Some(&dep.name));
            if satisfied || attempted.contains(&dep.name) {
                continue;
            }
            if !out.iter().any(|(n, _)| *n == dep.name) {
                out.push((dep.name.clone(), source.clone()));
            }
        }
    }
    out
}

/// Resolve one source and append everything it provides to the running
/// candidate list and lock. `gate`, when present, decides whether each acquired
/// capability may be composed at all — used for transitive acquisition, where
/// the profile never named the source.
#[allow(clippy::too_many_arguments)]
fn ingest_source(
    source: &Source,
    source_index: usize,
    workdir: &Path,
    lock: Option<&Lock>,
    gate: Option<&TrustGate>,
    candidates: &mut Vec<Candidate>,
    locked_sources: &mut Vec<LockedSource>,
    warnings: &mut Vec<String>,
) -> Result<usize, String> {
    let Some(ResolvedSource {
        origin,
        kind,
        rev,
        requested,
        subdir,
        mut caps,
    }) = resolve_source(source, workdir, lock, warnings)?
    else {
        return Ok(0); // a source that resolved to nothing was already warned about
    };
    let namespace = source_namespace(source);
    for cap in &mut caps {
        cap.adopt_namespace(namespace.as_deref());
    }
    // Namespace first, then select — so `include` patterns can name either the
    // qualified name or the bare id, and so the lock records only what survived.
    let caps = apply_select(source_select(source), caps, &origin, warnings)?;

    let mut locked_caps = Vec::new();
    let mut accepted = 0usize;
    for cap in caps {
        if let Some(gate) = gate {
            if let Err(refusal) = gate.admit(&cap, &origin) {
                warnings.push(refusal);
                continue;
            }
        }
        let name = cap.qualified_name();
        locked_caps.push(LockedCap {
            name: name.clone(),
            id: cap.manifest.id.clone(),
            version: cap.manifest.version.clone(),
            kind: cap.manifest.kind.as_str().to_string(),
            digest: content_digest(&cap),
            // Filled in once selection is done — a dependency's resolved
            // version isn't known until we know which candidate won.
            dependencies: BTreeMap::new(),
        });
        candidates.push(Candidate {
            version: Version::parse_lenient(&cap.manifest.version),
            name,
            origin: origin.clone(),
            source_index,
            cap,
        });
        accepted += 1;
    }
    locked_sources.push(LockedSource {
        kind: kind.to_string(),
        origin,
        rev,
        requested,
        subdir,
        capabilities: locked_caps,
    });
    Ok(accepted)
}

/// One capability offered by one source, before selection.
struct Candidate {
    cap: LoadedCapability,
    /// Position of the providing source in the profile — the preference order.
    source_index: usize,
    origin: String,
    name: String,
    version: Version,
}

struct Selection {
    capabilities: Vec<LoadedCapability>,
}

/// Choose one capability per qualified name.
///
/// **Sources are a preference order**: the earliest source that provides a name
/// wins, exactly as before. A version requirement can *veto* that choice — if
/// the preferred candidate does not satisfy what its dependents ask for, the
/// next satisfying candidate is taken instead — but it never reorders sources
/// for its own sake. That keeps the common case ("my sources, in the order I
/// listed them") predictable, and makes requirements a constraint rather than a
/// competing ranking.
///
/// Requirements are collected from the *preferred* candidate of each name, and
/// selection runs once. There is no backtracking: a requirement that only
/// appears in a version that a different requirement rules out will not be
/// discovered. That limit is deliberate at this scale, and the conflict report
/// says what was asked and by whom rather than pretending to have searched.
fn select(candidates: Vec<Candidate>, warnings: &mut Vec<String>) -> Selection {
    // Group by qualified name, preserving source order within each group.
    let mut groups: Vec<(String, Vec<Candidate>)> = Vec::new();
    for c in candidates {
        match groups.iter_mut().find(|(n, _)| *n == c.name) {
            Some((_, list)) => list.push(c),
            None => groups.push((c.name.clone(), vec![c])),
        }
    }
    for (_, list) in &mut groups {
        list.sort_by_key(|c| c.source_index);
    }

    // Pass 1: the preferred candidate of each name, used only to read
    // requirements off (see the note about backtracking above).
    let preferred: Vec<&Candidate> = groups.iter().filter_map(|(_, l)| l.first()).collect();
    let known: Vec<String> = groups.iter().map(|(n, _)| n.clone()).collect();
    let aliases = build_aliases(&preferred);

    // Pass 2: every `requires` edge, indexed by the name it constrains.
    let mut demands: HashMap<String, Vec<(String, Requirement)>> = HashMap::new();
    for c in &preferred {
        for dep in c
            .cap
            .manifest
            .dependencies
            .with_relation(Relation::Requires)
        {
            if dep.requirement.is_any() {
                continue; // "any" constrains nothing; unmet-ness is checked later
            }
            if let Some(target) = resolve_dep_name(&dep.name, &c.cap, &known, &aliases) {
                demands
                    .entry(target)
                    .or_default()
                    .push((c.name.clone(), dep.requirement.clone()));
            }
        }
    }

    // Pass 3: pick, honoring source order but skipping vetoed candidates.
    let mut capabilities = Vec::new();
    for (name, list) in groups {
        let asks = demands.get(&name).cloned().unwrap_or_default();
        let combined = asks
            .iter()
            .fold(Requirement::any(), |acc, (_, r)| acc.intersect(r));

        let chosen = list.iter().position(|c| combined.matches(&c.version));
        match chosen {
            Some(index) => {
                report_shadowed(&name, &list, index, warnings);
                let mut list = list;
                capabilities.push(list.remove(index).cap);
            }
            None => {
                // Nothing on offer satisfies every dependent. Say who asked for
                // what and what was actually available — the part of a solver
                // people actually need.
                let asked = asks
                    .iter()
                    .map(|(who, r)| format!("\n    {who} requires {r}"))
                    .collect::<String>();
                let offered = list
                    .iter()
                    .map(|c| format!("\n    {} offers {}", c.origin, c.version))
                    .collect::<String>();
                warnings.push(format!(
                    "version conflict on '{name}': no available version satisfies every \
                     dependent{asked}{offered}\n    keeping {} (from {}) — \
                     the dependents' requirements are NOT met",
                    list[0].version, list[0].origin
                ));
                let mut list = list;
                capabilities.push(list.remove(0).cap);
            }
        }
    }
    Selection { capabilities }
}

/// Note the candidates a selection passed over, and why.
fn report_shadowed(name: &str, list: &[Candidate], chosen: usize, warnings: &mut Vec<String>) {
    for (i, c) in list.iter().enumerate() {
        if i == chosen {
            continue;
        }
        let why = if i < chosen {
            " (does not satisfy a dependent's version requirement)"
        } else {
            ""
        };
        let version_note = if c.version != list[chosen].version {
            format!(" (kept {}, ignored {})", list[chosen].version, c.version)
        } else {
            String::new()
        };
        warnings.push(format!(
            "capability '{name}' from {} shadowed by an earlier source{version_note}{why}",
            c.origin
        ));
    }
}

/// Names a capability answers to besides its own: the targets of its `replaces`
/// edges. A fork standing in for the original satisfies dependencies on it.
fn build_aliases(candidates: &[&Candidate]) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for c in candidates {
        for dep in c
            .cap
            .manifest
            .dependencies
            .with_relation(Relation::Replaces)
        {
            aliases.insert(dep.name.clone(), c.name.clone());
        }
    }
    aliases
}

/// Resolve a dependency's written name to a composed capability's qualified
/// name, in decreasing order of specificity:
///
/// 1. an exact qualified-name match;
/// 2. a **sibling** — the bare name inside the requirer's own namespace, so
///    capabilities in one repo keep referring to each other by bare id;
/// 3. a `replaces` alias;
/// 4. a bare id that is unique across everything composed.
///
/// An ambiguous bare id (step 4 with several matches) resolves to nothing, and
/// the caller reports it as unmet — guessing between two strangers' capabilities
/// is exactly the failure qualified names exist to prevent.
fn resolve_dep_name(
    written: &str,
    requirer: &LoadedCapability,
    known: &[String],
    aliases: &HashMap<String, String>,
) -> Option<String> {
    if known.iter().any(|n| n == written) {
        return Some(written.to_string());
    }
    if let Some(ns) = &requirer.manifest.namespace {
        let sibling = format!("{ns}/{written}");
        if known.contains(&sibling) {
            return Some(sibling);
        }
    }
    if let Some(target) = aliases.get(written) {
        return Some(target.clone());
    }
    let mut bare = known
        .iter()
        .filter(|n| n.rsplit('/').next() == Some(written));
    match (bare.next(), bare.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

/// What one source resolved to. The capabilities do not yet carry the source's
/// namespace — [`ingest_source`] applies it (see [`source_namespace`]).
struct ResolvedSource {
    /// Path (local), url (git), or `index#name` (registry).
    origin: String,
    /// `"local"` | `"git"` | `"registry"`.
    kind: &'static str,
    /// The commit a git source resolved to.
    rev: Option<String>,
    /// The revision the profile asked for, kept so a later resolve can tell
    /// "the lock pinned this" from "the profile now asks for something else".
    requested: Option<String>,
    subdir: Option<String>,
    caps: Vec<LoadedCapability>,
}

/// Warn when two distinct capabilities share a bare `id`.
///
/// Namespacing lets `acme/mem-search` and `thedotmack/mem-search` both compose,
/// which is right — but the *emitted* filenames use the bare id, so they will
/// land on the same `.claude/skills/mem-search/SKILL.md`. `sync` already handles
/// that collision honestly (first producer wins, loudly flagged), and this warns
/// at resolve time so the clash is visible before anything is written, with the
/// fix named: override the path per harness, or rename the id.
fn warn_on_bare_id_collisions(caps: &[LoadedCapability], warnings: &mut Vec<String>) {
    let mut by_id: HashMap<&str, Vec<String>> = HashMap::new();
    for cap in caps {
        by_id
            .entry(cap.manifest.id.as_str())
            .or_default()
            .push(cap.qualified_name());
    }
    let mut collisions: Vec<(&str, Vec<String>)> = by_id
        .into_iter()
        .filter(|(_, names)| names.len() > 1)
        .collect();
    collisions.sort_by(|a, b| a.0.cmp(b.0));
    for (id, names) in collisions {
        warnings.push(format!(
            "bare id '{id}' is claimed by {} capabilities ({}) — they compose, but \
             emit to the same paths; set a per-harness `overrides.path` or rename one",
            names.len(),
            names.join(", ")
        ));
    }
}

// ---- selection ------------------------------------------------------------

/// The `select` block a source carries.
fn source_select(source: &Source) -> &Select {
    match source {
        Source::Local(l) => &l.select,
        Source::Git(g) => &g.select,
        Source::Http(h) => &h.select,
        Source::Registry(r) => &r.select,
        Source::Plugin(p) => &p.select,
    }
}

/// Compile a `select` pattern, naming the offender when it is malformed.
///
/// Matching is `glob::Pattern` used purely on strings — nothing here touches
/// the filesystem. Its default options leave `require_literal_separator` off,
/// so `*` spans `/`: `acme/*` selects a whole namespace and `*` selects
/// everything. Character classes (`[abc]`, `[a-z]`, `[!abc]`) come along for
/// free, and an unclosed one is now a reported error rather than being silently
/// treated as a literal bracket.
fn compile_pattern(pattern: &str) -> Result<glob::Pattern, String> {
    glob::Pattern::new(pattern)
        .map_err(|e| format!("select pattern '{pattern}' is not a valid glob: {e}"))
}

/// Compile a whole list, so one bad pattern fails the resolve rather than
/// quietly matching nothing.
fn compile_patterns(patterns: &[String]) -> Result<Vec<glob::Pattern>, String> {
    patterns.iter().map(|p| compile_pattern(p)).collect()
}

/// Does any pattern match this capability, by qualified name or bare id?
fn any_match(patterns: &[glob::Pattern], cap: &LoadedCapability) -> bool {
    let qualified = cap.qualified_name();
    patterns
        .iter()
        .any(|p| p.matches(&qualified) || p.matches(&cap.manifest.id))
}

/// Apply a source's `select` block to the capabilities it provided.
///
/// An `include` pattern that matches nothing **in the source at all** is a hard
/// error listing what is available: a typo silently yielding zero capabilities
/// is precisely the failure this feature would otherwise introduce. A pattern
/// that matches something `kinds` or `exclude` then removes is not an error —
/// that combination is deliberate.
fn apply_select(
    select: &Select,
    caps: Vec<LoadedCapability>,
    origin: &str,
    warnings: &mut Vec<String>,
) -> Result<Vec<LoadedCapability>, String> {
    if select.is_empty() {
        return Ok(caps);
    }
    let include = compile_patterns(&select.include)?;
    let exclude = compile_patterns(&select.exclude)?;

    for (pattern, compiled) in select.include.iter().zip(&include) {
        if !caps
            .iter()
            .any(|c| any_match(std::slice::from_ref(compiled), c))
        {
            let mut available: Vec<String> = caps.iter().map(|c| c.manifest.id.clone()).collect();
            available.sort();
            return Err(format!(
                "select.include pattern '{pattern}' matches nothing in {origin}. Available: {}",
                if available.is_empty() {
                    "(the source provided no capabilities)".to_string()
                } else {
                    summarize(&available)
                }
            ));
        }
    }

    let keep = |c: &LoadedCapability| -> bool {
        if !select.kinds.is_empty() && !select.kinds.iter().any(|k| k == c.manifest.kind.as_str()) {
            return false;
        }
        if !include.is_empty() && !any_match(&include, c) {
            return false;
        }
        // Exclude is applied last, so it always wins.
        !any_match(&exclude, c)
    };

    let (mut kept, dropped): (Vec<_>, Vec<_>) = caps.into_iter().partition(|c| keep(c));
    let mut dropped = dropped;

    if select.with_dependencies {
        pull_in_dependencies(&mut kept, &mut dropped, warnings, origin);
    }

    if !dropped.is_empty() {
        // Bare ids, not qualified names: every capability here came from the
        // same source, so repeating its namespace on each one is noise — and
        // the source is already named by `from {origin}`.
        let mut names: Vec<String> = dropped.iter().map(|c| c.manifest.id.clone()).collect();
        names.sort();
        warnings.push(format!(
            "select kept {} of {} from {origin} (dropped {})",
            kept.len(),
            kept.len() + names.len(),
            summarize(&names)
        ));
    }
    Ok(kept)
}

/// Join names for a report, capping the list so selecting 2 of 19 does not
/// produce a warning longer than the output it explains.
fn summarize(names: &[String]) -> String {
    const SHOWN: usize = 6;
    if names.len() <= SHOWN {
        return names.join(", ");
    }
    format!(
        "{}, … and {} more",
        names[..SHOWN].join(", "),
        names.len() - SHOWN
    )
}

/// Re-admit dropped capabilities that a kept one `requires`, to a fixpoint —
/// a pulled-in dependency may itself require another.
fn pull_in_dependencies(
    kept: &mut Vec<LoadedCapability>,
    dropped: &mut Vec<LoadedCapability>,
    warnings: &mut Vec<String>,
    origin: &str,
) {
    loop {
        // A dependency name matches a dropped capability by qualified name or
        // bare id — the same two spellings `select` matches on.
        let wanted: Vec<usize> = dropped
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                let (qualified, id) = (d.qualified_name(), d.manifest.id.as_str());
                kept.iter().any(|k| {
                    k.manifest
                        .dependencies
                        .with_relation(Relation::Requires)
                        .any(|dep| dep.name == qualified || dep.name == id)
                })
            })
            .map(|(i, _)| i)
            .collect();
        if wanted.is_empty() {
            return;
        }
        // Remove high-to-low so the earlier indices stay valid.
        for i in wanted.into_iter().rev() {
            let cap = dropped.remove(i);
            warnings.push(format!(
                "select re-admitted '{}' from {origin} — required by a selected capability \
                 (with_dependencies)",
                cap.qualified_name()
            ));
            kept.push(cap);
        }
    }
}

/// The namespace a source lends the capabilities it provides.
///
/// A **local** path is your own tree, so it lends nothing — bare ids stay bare,
/// and nothing about single-project authoring changes. A **git** source lends
/// `<owner>/<repo>`, which is how people already name these things. A
/// **registry** entry lends its own name, since that is the identity the index
/// publishes.
fn source_namespace(source: &Source) -> Option<String> {
    match source {
        Source::Local(_) => None,
        Source::Git(g) => namespace_from_git_url(&g.url),
        // An archive is identified by its **digest**, not by a name it
        // publishes, and a digest is not a usable namespace. Its URL is a
        // storage location (typically `blobs/sha256/…`), so deriving one from
        // that would be noise dressed as identity — the capabilities keep their
        // bare ids, exactly as from a local path. Publish the archive through a
        // registry to give it a name.
        Source::Http(_) => None,
        Source::Registry(r) => Some(r.name.clone()),
        // A plugin's namespace comes from its own manifest, which `import`
        // reads — so it is applied there rather than guessed from the URL.
        Source::Plugin(_) => None,
    }
}

/// `<owner>/<repo>` from a git URL, tolerating the spellings people actually
/// write: `https://host/owner/repo(.git)`, `git@host:owner/repo.git`, and a
/// bare local path (which yields just the directory name).
fn namespace_from_git_url(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    // `scp`-style remotes put the path after a colon, not a slash.
    let path = match trimmed.split_once("://") {
        Some((_, rest)) => rest.split_once('/').map(|(_, p)| p).unwrap_or(rest),
        None => match trimmed.split_once(':') {
            Some((_, p)) => p,
            None => trimmed,
        },
    };
    let parts: Vec<&str> = path
        .split('/')
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();
    match parts.as_slice() {
        [] => None,
        [repo] => Some((*repo).to_string()),
        [.., owner, repo] => Some(format!("{owner}/{repo}")),
    }
}

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
            Ok(Some(ResolvedSource {
                origin: l.path.clone(),
                kind: "local",
                rev: None,
                requested: None,
                subdir: None,
                caps,
            }))
        }
        Source::Git(g) => {
            let pin = lock.and_then(|lk| lk.pinned_rev(&g.url, &g.rev));
            let (checkout_dir, sha) = git_sync(&g.url, pin.unwrap_or(&g.rev), workdir)?;
            let scan = match &g.subdir {
                Some(s) => checkout_dir.join(s),
                None => checkout_dir,
            };
            // The checkout lives under a cache slug, so if the scanned root *is*
            // the capability, name it after the repository rather than the slug.
            let caps = crate::manifest::discover_named(&scan, Some(&g.repo_name()))?;
            Ok(Some(ResolvedSource {
                origin: g.url.clone(),
                kind: "git",
                rev: Some(sha),
                requested: Some(g.rev.clone()),
                subdir: g.subdir.clone(),
                caps,
            }))
        }
        Source::Http(h) => resolve_http(h, workdir, warnings),
        Source::Registry(r) => resolve_registry(r, workdir, lock, warnings),
        Source::Plugin(p) => resolve_plugin(p, workdir, lock, warnings),
    }
}

/// Resolve a plugin bundle: fetch it (git or a local path), locate the plugin
/// root, and import it.
///
/// Every note the importer produced is surfaced as a warning. That is the
/// point: a plugin is not uniformly portable, and which parts travelled is
/// exactly what a user needs told.
fn resolve_plugin(
    p: &PluginSource,
    workdir: &Path,
    lock: Option<&Lock>,
    warnings: &mut Vec<String>,
) -> Result<Option<ResolvedSource>, String> {
    // A `rev` means git; without one the url is a path, so a plugin checked out
    // beside the profile imports with no network and no git.
    let (repo, rev, requested) = match &p.rev {
        Some(rev) => {
            let pin = lock.and_then(|lk| lk.pinned_rev(&p.url, rev));
            let (dir, sha) = git_sync(&p.url, pin.unwrap_or(rev), workdir)?;
            (dir, Some(sha), Some(rev.clone()))
        }
        None => (workdir.join(&p.url), None, None),
    };
    let repo = match &p.subdir {
        Some(s) => repo.join(s),
        None => repo,
    };

    let root = crate::plugin::find_plugin_root(&repo, p.name.as_deref())?;
    let imported = crate::plugin::import(&root)?;

    // The plugin's own name is its published identity, so it becomes the
    // namespace — better than `<owner>/<repo>`, which is where it happens to be
    // hosted rather than what it is called.
    let namespace = imported.manifest.name.clone();
    let mut caps = imported.capabilities;
    for cap in &mut caps {
        cap.adopt_namespace(Some(&namespace));
    }
    for note in imported.notes {
        warnings.push(format!("plugin '{namespace}': {note}"));
    }

    Ok(Some(ResolvedSource {
        origin: format!("{} (plugin {namespace})", p.url),
        kind: "plugin",
        rev,
        requested,
        subdir: p.subdir.clone(),
        caps,
    }))
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
    Ok(Some(ResolvedSource {
        origin: h.url.clone(),
        kind: "http",
        rev: Some(format!("sha256:{digest}")),
        // The pin *is* the request: an archive source names one artifact, so
        // there is no "requested a branch, got a SHA" gap to record.
        requested: Some(format!("sha256:{digest}")),
        subdir: h.subdir.clone(),
        caps,
    }))
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
    Ok(
        resolve_source(&entry.source, workdir, lock, warnings)?.map(|leaf| ResolvedSource {
            origin,
            kind: "registry",
            ..leaf
        }),
    )
}

/// The composed set plus the dependency edges that were actually resolved.
pub struct Graph {
    pub capabilities: Vec<LoadedCapability>,
    /// Requirer's qualified name → (resolved dependency name → its version).
    /// Recorded in the lock so the resolved graph is pinned and auditable.
    pub edges: HashMap<String, BTreeMap<String, String>>,
}

/// Resolve the dependency graph over the composed set.
///
/// Every relation is checked and reported — loudly, non-fatally, nothing
/// dropped:
///
/// * **requires** — unmet (no source provides it, or the name is an ambiguous
///   bare id) and version-unsatisfied edges are warned. Only these create
///   ordering edges.
/// * **suggests** — an absent target is a note, not a problem.
/// * **conflicts** — both composed is warned. This matters here more than in a
///   normal package manager: `sync` merges same-path config deny-wins, so two
///   contradictory policies silently produce the strictest union, which may be
///   neither author's intent.
/// * **replaces** — the fork *and* the original both composed is warned; they
///   will fight over the same emitted paths.
///
/// The returned set is ordered so every dependency precedes the capabilities
/// that declare it (a stable topological sort; ties keep source order, so a
/// dependency-free set is returned unchanged).
///
/// Unmet dependencies are reported transitively for free: an intermediate
/// dependency's own missing dependencies surface when that capability is visited.
fn resolve_dependencies(caps: Vec<LoadedCapability>, warnings: &mut Vec<String>) -> Graph {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let names: Vec<String> = caps.iter().map(|c| c.qualified_name()).collect();
    let versions: HashMap<&str, Version> = caps
        .iter()
        .zip(&names)
        .map(|(c, n)| (n.as_str(), Version::parse_lenient(&c.manifest.version)))
        .collect();
    let aliases: HashMap<String, String> = caps
        .iter()
        .zip(&names)
        .flat_map(|(c, n)| {
            c.manifest
                .dependencies
                .with_relation(Relation::Replaces)
                .map(move |d| (d.name.clone(), n.clone()))
        })
        .collect();

    let mut edges: HashMap<String, BTreeMap<String, String>> = HashMap::new();
    // Ordering edges, as (dependency index → dependent index).
    let mut requires_edges: Vec<(usize, usize)> = Vec::new();

    for (i, cap) in caps.iter().enumerate() {
        let me = &names[i];
        for dep in &cap.manifest.dependencies {
            let target = resolve_dep_name(&dep.name, cap, &names, &aliases);
            match (dep.relation, &target) {
                (Relation::Requires, None) => warnings.push(format!(
                    "capability '{me}' requires '{}', which no source provides",
                    dep.name
                )),
                (Relation::Suggests, None) => warnings.push(format!(
                    "capability '{me}' suggests '{}', which is not composed (optional)",
                    dep.name
                )),
                // An absent conflict/replace target is the good outcome.
                (Relation::Conflicts, None) | (Relation::Replaces, None) => {}

                (Relation::Conflicts, Some(t)) => warnings.push(format!(
                    "capability '{me}' declares a conflict with '{t}', but both are composed — \
                     same-path config merges deny-wins, so the result may be neither author's intent"
                )),
                (Relation::Replaces, Some(t)) if t != me => warnings.push(format!(
                    "capability '{me}' replaces '{t}', but '{t}' is also composed — \
                     both will emit to the same paths; drop one"
                )),
                (Relation::Replaces, Some(_)) => {}

                (Relation::Requires | Relation::Suggests, Some(t)) => {
                    let version = versions.get(t.as_str()).cloned().unwrap_or_default();
                    if dep.relation == Relation::Requires && !dep.requirement.matches(&version) {
                        warnings.push(format!(
                            "capability '{me}' requires '{}' {}, but the composed version is {version}",
                            dep.name, dep.requirement
                        ));
                    }
                    edges
                        .entry(me.clone())
                        .or_default()
                        .insert(t.clone(), version.to_string());
                    // Only a hard requirement constrains ordering; a suggestion
                    // that created ordering edges would turn optional advice
                    // into a cycle.
                    if dep.relation == Relation::Requires {
                        if let Some(j) = names.iter().position(|n| n == t) {
                            if j != i {
                                requires_edges.push((j, i));
                            }
                        }
                    }
                }
            }
        }
    }

    let n = caps.len();
    // Edge dependency → dependent; a node's in-degree is its count of present deps.
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (dependency, dependent) in requires_edges {
        dependents[dependency].push(dependent);
        in_degree[dependent] += 1;
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
            .map(|i| names[i].as_str())
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
    Graph {
        capabilities: order.into_iter().filter_map(|i| slots[i].take()).collect(),
        edges,
    }
}

/// Report dependencies that are satisfied *in the composed set* but not *on
/// every harness the profile targets*.
///
/// "Present" is not the same as "usable here". A skill that depends on a tool
/// capability is fine on Claude Code and broken on Aider, which has no MCP at
/// all — the dependency resolves, and then the harness cannot host it. A
/// name-only check cannot see that, and it is exactly the kind of silent
/// degradation this project exists to refuse.
fn warn_on_per_harness_gaps(
    caps: &[LoadedCapability],
    edges: &HashMap<String, BTreeMap<String, String>>,
    harnesses: &[Harness],
    warnings: &mut Vec<String>,
) {
    use crate::kind::{kind_impl, Installability};

    let by_name: HashMap<String, &LoadedCapability> =
        caps.iter().map(|c| (c.qualified_name(), c)).collect();

    // `plan` is the same call `sync` makes, so this asks the real question.
    let hosts = |cap: &LoadedCapability, h: Harness| -> bool {
        !matches!(
            kind_impl(cap.manifest.kind).plan(cap, h).installability,
            Installability::Unsupported(_)
        )
    };

    for cap in caps {
        let me = cap.qualified_name();
        let Some(deps) = edges.get(&me) else { continue };
        for dep_name in deps.keys() {
            let Some(dep) = by_name.get(dep_name) else {
                continue;
            };
            let gaps: Vec<&str> = harnesses
                .iter()
                .filter(|h| hosts(cap, **h) && !hosts(dep, **h))
                .map(|h| h.id())
                .collect();
            if !gaps.is_empty() {
                warnings.push(format!(
                    "capability '{me}' depends on '{dep_name}', which is Unsupported on: {} — \
                     '{me}' installs there without it",
                    gaps.join(", ")
                ));
            }
        }
    }
}

/// Copy the resolved dependency edges onto the locked capabilities, so the lock
/// pins the graph that was actually resolved — name *and* version — rather than
/// the names the author happened to write.
fn record_resolved_dependencies(
    sources: &mut [LockedSource],
    edges: &HashMap<String, BTreeMap<String, String>>,
) {
    for source in sources {
        for cap in &mut source.capabilities {
            if let Some(resolved) = edges.get(&cap.name) {
                cap.dependencies = resolved.clone();
            }
        }
    }
}

/// The content digest pinned in the lock: a `sha256` over the capability's whole
/// file tree ([`crate::trust::capability_digest`]) — the same digest signatures
/// are made over, so the lock and the trust layer agree on what "this
/// capability" means.
///
/// An unreadable directory degrades to a marker rather than a plausible-looking
/// hash, because a digest that silently means "we couldn't look" is worse than
/// one that says so.
fn content_digest(cap: &LoadedCapability) -> String {
    crate::trust::capability_digest(&cap.dir).unwrap_or_else(|_| "sha256:unavailable".to_string())
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
    let sha = resolve_rev(&dir, rev).map_err(|e| format!("resolve '{rev}' in {url}: {e}"))?;
    // Detach: the cached clone is a checkout target, never a branch to advance.
    git(&["-C", &dir, "checkout", "--quiet", "--detach", &sha])
        .map_err(|e| format!("checkout '{rev}' ({sha}) in {url}: {e}"))?;
    Ok((dest, sha))
}

/// Resolve `rev` to a commit SHA inside the cached clone, **preferring the
/// remote-tracking ref**.
///
/// This ordering is load-bearing. `fetch` updates `origin/<branch>` but never
/// fast-forwards the cache's local branch of the same name, so resolving a
/// branch locally would pin whatever commit was HEAD at first clone — forever,
/// however often the source is re-resolved. A tag or an explicit SHA has no
/// remote-tracking ref and falls through to the second candidate.
fn resolve_rev(dir: &str, rev: &str) -> Result<String, String> {
    for candidate in [format!("origin/{rev}"), rev.to_string()] {
        let spec = format!("{candidate}^{{commit}}");
        if let Ok(sha) = git(&["-C", dir, "rev-parse", "--verify", "--quiet", &spec]) {
            if !sha.is_empty() {
                return Ok(sha);
            }
        }
    }
    Err(format!("unknown revision '{rev}'"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Namespace derivation is what every git-sourced capability's identity
    /// rests on, and the integration tests only ever reach it through local
    /// paths — so the URL spellings people actually write are pinned here.
    #[test]
    fn namespace_from_the_url_spellings_people_write() {
        let ns = |u: &str| namespace_from_git_url(u).unwrap();

        assert_eq!(
            ns("https://github.com/thedotmack/claude-mem"),
            "thedotmack/claude-mem"
        );
        assert_eq!(
            ns("https://github.com/thedotmack/claude-mem.git"),
            "thedotmack/claude-mem"
        );
        assert_eq!(
            ns("https://github.com/thedotmack/claude-mem/"),
            "thedotmack/claude-mem"
        );
        // scp-style remotes put the path after a colon, not a slash.
        assert_eq!(
            ns("git@github.com:thedotmack/claude-mem.git"),
            "thedotmack/claude-mem"
        );
        assert_eq!(ns("ssh://git@github.com/acme/caps"), "acme/caps");
        // A nested group keeps the last two segments — the repo and its parent.
        assert_eq!(ns("https://gitlab.com/group/sub/repo"), "sub/repo");
        // A bare local path still yields something usable.
        assert_eq!(ns("/srv/repos/caps"), "repos/caps");
        assert_eq!(ns("caps"), "caps");
    }

    /// Two authors' repos must not collapse to the same namespace.
    #[test]
    fn different_owners_get_different_namespaces() {
        assert_ne!(
            namespace_from_git_url("https://github.com/alice/caps"),
            namespace_from_git_url("https://github.com/bob/caps"),
        );
    }
}
