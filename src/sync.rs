//! Composition + sync: merge a whole set of capabilities into a deterministic
//! desired project state, write it idempotently, and detect drift.
//!
//! This is the L4 "apply a profile to a project" layer (#16). The per-kind
//! `plan()` (`kind.rs`) already says *what* files one capability produces for
//! one harness; this module decides *how a whole set lands on disk* when several
//! capabilities target the same file, how to converge that state idempotently,
//! how to cleanly remove it, and how to verify it in CI.
//!
//! ## Composition rules (deterministic, documented, tested)
//!
//! Capabilities are composed in **id order** and, within a path, by
//! `(capability id, harness id)` — a stable total order, so the merged output
//! is reproducible. Artifacts are grouped by their **target path** (the real
//! on-disk file, independent of which harness asked for it — `.vscode/settings.json`
//! is genuinely one file shared by Windsurf and Copilot). A path with one
//! producer is written verbatim; a path with several is **merged**:
//!
//!   * **JSON** targets deep-merge — objects recurse, arrays union (dedup,
//!     first-seen order), and a scalar clash is resolved by:
//!       * **most-restrictive permission verdict wins** when both values are
//!         `allow`/`ask`/`deny` (the hook deny-wins rule, extended to
//!         permissions), otherwise
//!       * **last writer in the composition order wins**, recorded as a conflict.
//!   * **Non-JSON** targets (Markdown / TOML) can't be structurally merged:
//!     identical contents are idempotent; differing contents are a conflict and
//!     the **first producer in composition order wins** (loudly recorded).
//!
//! Nothing is silently dropped. Artifacts that can't be placed into the project
//! tree — hook **registrations** (native snippets to wire into an entrypoint)
//! and **global paths** (`~/…`, outside the project) — are reported as
//! *deferred* manual steps, and **Unsupported** targets as *blocked*.
//!
//! ## On-disk contract
//!
//! `apply` writes the desired files and records every managed path in a
//! lockfile (`.open-harness/sync.lock.json`). A subsequent `apply` with a
//! capability removed **prunes** the file it used to own (declarative
//! convergence); `uninstall` prunes everything in the lockfile. Pruning only
//! ever touches paths the lockfile records as ours — an unmanaged file is never
//! removed. `check` recomputes the desired state and compares it to disk;
//! `--ci` turns any drift into a non-zero exit.

use crate::adapters::Harness;
use crate::kind::{kind_impl, Artifact, Installability};
use crate::manifest::LoadedCapability;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Component, Path};

/// Relative location of the sync lockfile inside a synced project.
pub const LOCK_REL: &str = ".open-harness/sync.lock.json";

// ---- planning (pure, no filesystem) ---------------------------------------

/// A file the sync will create/update, after composing every contributing cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredFile {
    /// Project-relative path (POSIX separators, as emitted by the kinds).
    pub path: String,
    /// Final, merged contents — the single source of truth for write & check.
    pub contents: String,
    /// Contributing target harnesses, sorted + deduped.
    pub harnesses: Vec<String>,
    /// Contributing capability ids, sorted + deduped.
    pub sources: Vec<String>,
    /// Degradation notes carried from the contributing plans.
    pub degraded: Vec<String>,
    /// Conflict resolutions applied while merging (empty when clean).
    pub conflicts: Vec<String>,
}

/// Why a capability's output could not be placed as a project file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferReason {
    /// A hook native snippet: wire it into the harness's entrypoint by hand.
    Registration,
    /// The target is a global path (`~/…` / absolute), outside the project tree.
    GlobalPath,
    /// The harness cannot host this capability at all (matrix BLOCKED).
    Unsupported,
}

impl DeferReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeferReason::Registration => "registration",
            DeferReason::GlobalPath => "global-path",
            DeferReason::Unsupported => "unsupported",
        }
    }
}

/// An artifact reported instead of written — never silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferred {
    pub harness: String,
    pub source: String,
    pub kind: String,
    pub reason: DeferReason,
    /// The native snippet, the global path, or the unsupported reason.
    pub detail: String,
}

/// The composed desired state for a project — pure data, no IO performed.
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    /// Files to realize, sorted by path.
    pub files: Vec<DesiredFile>,
    /// Everything that couldn't be placed, sorted for stable reporting.
    pub deferred: Vec<Deferred>,
}

struct Contribution {
    source: String,
    harness: String,
    contents: String,
    degraded: Option<String>,
}

/// Compose the desired project state from `caps` across the target `harnesses`.
/// Pure: performs no filesystem writes. Deterministic for a given input.
pub fn plan_sync(caps: &[LoadedCapability], harnesses: &[Harness]) -> SyncPlan {
    let mut groups: BTreeMap<String, Vec<Contribution>> = BTreeMap::new();
    let mut deferred: Vec<Deferred> = Vec::new();

    // `discover` already returns id-sorted caps; enforce it so a caller passing
    // an ad-hoc slice still gets a deterministic composition order.
    let mut ordered: Vec<&LoadedCapability> = caps.iter().collect();
    ordered.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));

    for &h in harnesses {
        for cap in &ordered {
            let plan = kind_impl(cap.manifest.kind).plan(cap, h);
            let kind = cap.manifest.kind.as_str().to_string();
            let id = cap.manifest.id.clone();

            if let Installability::Unsupported(reason) = &plan.installability {
                deferred.push(Deferred {
                    harness: h.id().to_string(),
                    source: id,
                    kind,
                    reason: DeferReason::Unsupported,
                    detail: reason.clone(),
                });
                continue;
            }
            let degraded = match &plan.installability {
                Installability::Degraded(d) => Some(d.clone()),
                _ => None,
            };

            for art in plan.artifacts {
                match art {
                    Artifact::Registration { text } => deferred.push(Deferred {
                        harness: h.id().to_string(),
                        source: id.clone(),
                        kind: kind.clone(),
                        reason: DeferReason::Registration,
                        detail: text,
                    }),
                    Artifact::File { path, contents } if is_placeable(&path) => {
                        groups.entry(path).or_default().push(Contribution {
                            source: id.clone(),
                            harness: h.id().to_string(),
                            contents,
                            degraded: degraded.clone(),
                        });
                    }
                    Artifact::File { path, .. } => deferred.push(Deferred {
                        harness: h.id().to_string(),
                        source: id.clone(),
                        kind: kind.clone(),
                        reason: DeferReason::GlobalPath,
                        detail: path,
                    }),
                }
            }
        }
    }

    let mut files: Vec<DesiredFile> = groups
        .into_iter()
        .map(|(path, mut contribs)| {
            contribs.sort_by(|a, b| {
                (a.source.as_str(), a.harness.as_str())
                    .cmp(&(b.source.as_str(), b.harness.as_str()))
            });
            let (contents, conflicts) = merge_contributions(&path, &contribs);
            let harnesses = dedup_sorted(contribs.iter().map(|c| c.harness.clone()));
            let sources = dedup_sorted(contribs.iter().map(|c| c.source.clone()));
            let degraded = dedup_sorted(contribs.iter().filter_map(|c| c.degraded.clone()));
            DesiredFile {
                path,
                contents,
                harnesses,
                sources,
                degraded,
                conflicts,
            }
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));

    deferred.sort_by(|a, b| {
        (a.harness.as_str(), a.source.as_str(), a.detail.as_str()).cmp(&(
            b.harness.as_str(),
            b.source.as_str(),
            b.detail.as_str(),
        ))
    });
    deferred.dedup();

    SyncPlan { files, deferred }
}

fn dedup_sorted(it: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = it.collect();
    v.sort();
    v.dedup();
    v
}

/// A path is placeable into a project tree when it is relative, has no `~`
/// home prefix, and contains no `..` traversal. This doubles as the guard that
/// keeps `apply` from ever writing outside the target directory.
fn is_placeable(path: &str) -> bool {
    if path.starts_with('~') {
        return false;
    }
    let p = Path::new(path);
    p.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

// ---- composition / merge --------------------------------------------------

fn is_json_path(path: &str) -> bool {
    path.ends_with(".json")
}

fn merge_contributions(path: &str, contribs: &[Contribution]) -> (String, Vec<String>) {
    if contribs.len() == 1 {
        return (contribs[0].contents.clone(), Vec::new());
    }
    let mut conflicts = Vec::new();

    if is_json_path(path) {
        let parsed: Option<Vec<Value>> = contribs
            .iter()
            .map(|c| serde_json::from_str::<Value>(&c.contents).ok())
            .collect();
        if let Some(values) = parsed {
            let mut acc = values[0].clone();
            for (i, v) in values.iter().enumerate().skip(1) {
                acc = merge_json(acc, v.clone(), path, &contribs[i].source, &mut conflicts);
            }
            let out = serde_json::to_string_pretty(&acc).unwrap_or_else(|_| acc.to_string());
            return (out, conflicts);
        }
    }

    // Non-JSON (or unparseable): can't merge structurally. Identical is
    // idempotent; differing is a conflict, first producer wins (loud).
    let first = &contribs[0];
    if contribs.iter().any(|c| c.contents != first.contents) {
        let others: Vec<&str> = contribs[1..].iter().map(|c| c.source.as_str()).collect();
        conflicts.push(format!(
            "{path}: {} capabilities emit different non-mergeable contents; kept '{}', ignored {}",
            contribs.len(),
            first.source,
            others.join(", ")
        ));
    }
    (first.contents.clone(), conflicts)
}

/// Deep-merge `b` into `a`. Objects recurse, arrays union, scalars clash by the
/// documented precedence (permission verdict > id order).
fn merge_json(a: Value, b: Value, path: &str, src: &str, conflicts: &mut Vec<String>) -> Value {
    match (a, b) {
        (Value::Object(mut ao), Value::Object(bo)) => {
            for (k, bv) in bo {
                let merged = match ao.remove(&k) {
                    Some(av) => merge_json(av, bv, &format!("{path} > {k}"), src, conflicts),
                    None => bv,
                };
                ao.insert(k, merged);
            }
            Value::Object(ao)
        }
        (Value::Array(mut aa), Value::Array(ba)) => {
            for item in ba {
                if !aa.contains(&item) {
                    aa.push(item);
                }
            }
            Value::Array(aa)
        }
        (a, b) if a == b => a,
        (a, b) => {
            if let (Some(x), Some(y)) = (verdict_rank(&a), verdict_rank(&b)) {
                // Most-restrictive verdict wins: deny(2) > ask(1) > allow(0).
                let win = if y > x { b } else { a };
                if x != y {
                    conflicts.push(format!(
                        "{path}: verdict clash -> kept '{}' (most restrictive)",
                        scalar(&win)
                    ));
                }
                win
            } else {
                conflicts.push(format!(
                    "{path}: value clash '{}' vs '{}' -> kept '{}' ({src} wins by composition order)",
                    scalar(&a),
                    scalar(&b),
                    scalar(&b)
                ));
                b
            }
        }
    }
}

fn verdict_rank(v: &Value) -> Option<u8> {
    match v.as_str()? {
        "allow" => Some(0),
        "ask" => Some(1),
        "deny" => Some(2),
        _ => None,
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---- lockfile -------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockEntry {
    pub path: String,
    /// Non-cryptographic content fingerprint (FNV-1a) of the last-written file.
    pub fingerprint: String,
    pub harnesses: Vec<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lockfile {
    pub version: u32,
    pub managed: Vec<LockEntry>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Lockfile {
            version: 1,
            managed: Vec::new(),
        }
    }
}

impl Lockfile {
    fn load(root: &Path) -> Lockfile {
        let p = root.join(LOCK_REL);
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn managed_paths(&self) -> Vec<String> {
        self.managed.iter().map(|e| e.path.clone()).collect()
    }
}

/// FNV-1a 64-bit — a small, portable, deterministic content fingerprint
/// (stable across platforms/toolchains, unlike `DefaultHasher`).
pub(crate) fn fingerprint(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

// ---- apply / uninstall (filesystem) ---------------------------------------

/// What happened (or would happen) to one path during `apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeAction {
    Create,
    Update,
    Unchanged,
    Prune,
}

impl ChangeAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeAction::Create => "create",
            ChangeAction::Update => "update",
            ChangeAction::Unchanged => "unchanged",
            ChangeAction::Prune => "prune",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApplyChange {
    pub path: String,
    pub action: ChangeAction,
    pub harnesses: Vec<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ApplyReport {
    pub dry_run: bool,
    pub changes: Vec<ApplyChange>,
    pub deferred: Vec<Deferred>,
    /// (path, conflict messages) for every file whose merge resolved a clash.
    pub conflicts: Vec<(String, Vec<String>)>,
}

impl ApplyReport {
    pub fn count(&self, action: ChangeAction) -> usize {
        self.changes.iter().filter(|c| c.action == action).count()
    }
    /// Did anything actually change on disk (create/update/prune)?
    pub fn mutated(&self) -> bool {
        self.changes
            .iter()
            .any(|c| c.action != ChangeAction::Unchanged)
    }
}

/// Realize a plan into `root`: write desired files, prune managed files no
/// longer desired, and rewrite the lockfile. Idempotent — a second run with the
/// same inputs reports only `Unchanged`. With `dry_run`, computes the report but
/// touches nothing.
pub fn apply(root: &Path, plan: &SyncPlan, dry_run: bool) -> std::io::Result<ApplyReport> {
    let lock = Lockfile::load(root);
    let desired: Vec<String> = plan.files.iter().map(|f| f.path.clone()).collect();

    let mut changes = Vec::new();
    let mut new_entries = Vec::new();

    for f in &plan.files {
        let action = classify(root, &f.path, &f.contents)?;
        if !dry_run && matches!(action, ChangeAction::Create | ChangeAction::Update) {
            write_file(root, &f.path, &f.contents)?;
        }
        new_entries.push(LockEntry {
            path: f.path.clone(),
            fingerprint: fingerprint(&f.contents),
            harnesses: f.harnesses.clone(),
            sources: f.sources.clone(),
        });
        changes.push(ApplyChange {
            path: f.path.clone(),
            action,
            harnesses: f.harnesses.clone(),
            sources: f.sources.clone(),
        });
    }

    // Prune: previously-managed paths that are no longer desired.
    for entry in &lock.managed {
        if !desired.contains(&entry.path) {
            if !dry_run {
                remove_managed(root, &entry.path)?;
            }
            changes.push(ApplyChange {
                path: entry.path.clone(),
                action: ChangeAction::Prune,
                harnesses: entry.harnesses.clone(),
                sources: entry.sources.clone(),
            });
        }
    }

    if !dry_run {
        write_lockfile(
            root,
            &Lockfile {
                version: 1,
                managed: new_entries,
            },
        )?;
    }

    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let conflicts = plan
        .files
        .iter()
        .filter(|f| !f.conflicts.is_empty())
        .map(|f| (f.path.clone(), f.conflicts.clone()))
        .collect();
    Ok(ApplyReport {
        dry_run,
        changes,
        deferred: plan.deferred.clone(),
        conflicts,
    })
}

/// Remove every managed file recorded in the lockfile and clear it. Only touches
/// paths the lockfile owns; unmanaged files are never removed.
pub fn uninstall(root: &Path, dry_run: bool) -> std::io::Result<ApplyReport> {
    let lock = Lockfile::load(root);
    let mut changes = Vec::new();
    for entry in &lock.managed {
        if !dry_run {
            remove_managed(root, &entry.path)?;
        }
        changes.push(ApplyChange {
            path: entry.path.clone(),
            action: ChangeAction::Prune,
            harnesses: entry.harnesses.clone(),
            sources: entry.sources.clone(),
        });
    }
    if !dry_run {
        // Clear the lockfile itself (and remove it from disk).
        let p = root.join(LOCK_REL);
        if p.exists() {
            std::fs::remove_file(&p)?;
            prune_empty_dirs(root, &p);
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ApplyReport {
        dry_run,
        changes,
        deferred: Vec::new(),
        conflicts: Vec::new(),
    })
}

fn classify(root: &Path, rel: &str, contents: &str) -> std::io::Result<ChangeAction> {
    let full = root.join(rel);
    match std::fs::read_to_string(&full) {
        Ok(on_disk) if on_disk == contents => Ok(ChangeAction::Unchanged),
        Ok(_) => Ok(ChangeAction::Update),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ChangeAction::Create),
        Err(e) => Err(e),
    }
}

fn write_file(root: &Path, rel: &str, contents: &str) -> std::io::Result<()> {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(full, contents)
}

fn write_lockfile(root: &Path, lock: &Lockfile) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(lock).unwrap_or_else(|_| "{}".to_string());
    write_file(root, LOCK_REL, &format!("{text}\n"))
}

fn remove_managed(root: &Path, rel: &str) -> std::io::Result<()> {
    let full = root.join(rel);
    if full.exists() {
        std::fs::remove_file(&full)?;
        prune_empty_dirs(root, &full);
    }
    Ok(())
}

/// Walk up from a just-removed file removing now-empty directories, stopping at
/// (and never removing) `root`. Best-effort: a non-empty or unreadable dir ends
/// the walk.
fn prune_empty_dirs(root: &Path, removed: &Path) {
    let mut dir = removed.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) {
            break;
        }
        let is_empty = std::fs::read_dir(d).map(|mut e| e.next().is_none());
        match is_empty {
            Ok(true) if std::fs::remove_dir(d).is_ok() => {}
            _ => break,
        }
        dir = d.parent();
    }
}

// ---- check / drift --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftKind {
    /// Desired file present on disk with the expected contents.
    Ok,
    /// Desired file absent from disk.
    Missing,
    /// Desired file present but contents differ from the plan.
    Modified,
    /// Managed by the lockfile, no longer desired, still on disk.
    Stale,
}

impl DriftKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DriftKind::Ok => "ok",
            DriftKind::Missing => "missing",
            DriftKind::Modified => "modified",
            DriftKind::Stale => "stale",
        }
    }
    fn is_drift(&self) -> bool {
        !matches!(self, DriftKind::Ok)
    }
}

#[derive(Debug, Clone)]
pub struct DriftItem {
    pub path: String,
    pub kind: DriftKind,
    pub harnesses: Vec<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DriftReport {
    pub items: Vec<DriftItem>,
    pub deferred: Vec<Deferred>,
}

impl DriftReport {
    /// Any missing / modified / stale file — the CI failure condition.
    pub fn has_drift(&self) -> bool {
        self.items.iter().any(|i| i.kind.is_drift())
    }
    pub fn count(&self, kind: DriftKind) -> usize {
        self.items.iter().filter(|i| i.kind == kind).count()
    }
}

/// Compare a plan against the current on-disk state of `root`. Read-only.
pub fn check(root: &Path, plan: &SyncPlan) -> std::io::Result<DriftReport> {
    let lock = Lockfile::load(root);
    let desired: Vec<String> = plan.files.iter().map(|f| f.path.clone()).collect();
    let mut items = Vec::new();

    for f in &plan.files {
        let kind = match classify(root, &f.path, &f.contents)? {
            ChangeAction::Unchanged => DriftKind::Ok,
            ChangeAction::Create => DriftKind::Missing,
            ChangeAction::Update => DriftKind::Modified,
            ChangeAction::Prune => unreachable!("classify never returns Prune"),
        };
        items.push(DriftItem {
            path: f.path.clone(),
            kind,
            harnesses: f.harnesses.clone(),
            sources: f.sources.clone(),
        });
    }

    // Stale: lockfile-managed paths no longer desired but still present.
    for entry in &lock.managed {
        if !desired.contains(&entry.path) && root.join(&entry.path).exists() {
            items.push(DriftItem {
                path: entry.path.clone(),
                kind: DriftKind::Stale,
                harnesses: entry.harnesses.clone(),
                sources: entry.sources.clone(),
            });
        }
    }

    items.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(DriftReport {
        items,
        deferred: plan.deferred.clone(),
    })
}

/// Convenience: discover capabilities under `caps_dir`, plan them for `harnesses`.
pub fn plan_from_dir(caps_dir: &Path, harnesses: &[Harness]) -> Result<SyncPlan, String> {
    let caps = crate::manifest::discover(caps_dir)?;
    Ok(plan_sync(&caps, harnesses))
}

/// The lockfile-managed paths currently recorded under `root` (for reporting).
pub fn managed_paths(root: &Path) -> Vec<String> {
    Lockfile::load(root).managed_paths()
}
