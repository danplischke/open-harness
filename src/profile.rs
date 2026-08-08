//! Profiles, sources, and the resolved lockfile (#15) — the L4 "compose a
//! capability set from several places, then pin it" layer.
//!
//! A **profile** is a named set of **sources** targeting a set of harnesses. A
//! source resolves to capabilities from a **local path**, a **git repo**
//! (personal-repo sync — cloned/fetched and pinned to a commit), or a
//! **registry** (#21) — an index mapping names to their real local/git source,
//! one point of indirection over many repos. Resolving a profile composes the
//! sources in order and writes an `open-harness.lock` pinning every resolved
//! capability (qualified name + version + a sha256 over its file tree) and every
//! git source's exact commit — so a re-resolve is reproducible.
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

/// Default lockfile name, written next to the profile. Extension-less by
/// convention (like `Cargo.lock`); the contents are YAML.
pub const LOCK_NAME: &str = "open-harness.lock";

/// The profile filename without its extension. [`crate::config::find`] probes
/// `open-harness.yaml`, `open-harness.yml`, then a legacy `open-harness.json`.
pub const PROFILE_STEM: &str = "open-harness";

/// The canonical profile filename, for messages and `oh init`.
pub const PROFILE_NAME: &str = "open-harness.yaml";

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
    /// Directory of capabilities, relative to the profile's workdir.
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
    /// Path to the registry index (YAML, or legacy JSON), relative to the
    /// profile workdir. The
    /// index maps names → real sources (local/git), giving one point of
    /// indirection over many repos. Without it the source is a loud no-op.
    #[serde(default)]
    pub index: Option<String>,
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

impl RegistryIndex {
    fn load(path: &Path) -> Result<RegistryIndex, String> {
        crate::config::load(path).map_err(|e| format!("invalid registry index: {e}"))
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
    let Some((origin, kind, rev, subdir, mut caps)) =
        resolve_source(source, workdir, lock, warnings)?
    else {
        return Ok(0); // a source that resolved to nothing was already warned about
    };
    let namespace = source_namespace(source);
    for cap in &mut caps {
        cap.adopt_namespace(namespace.as_deref());
    }

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

/// A resolved source: `(origin, kind, pinned rev, subdir, capabilities)`. The
/// capabilities already carry the source's namespace (see [`source_namespace`]).
type ResolvedSource = (
    String,
    &'static str,
    Option<String>,
    Option<String>,
    Vec<LoadedCapability>,
);

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
        Source::Registry(r) => Some(r.name.clone()),
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
