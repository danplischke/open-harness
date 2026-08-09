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
//! lockfile (`.open-harness/sync.lock.yaml`). A subsequent `apply` with a
//! capability removed **prunes** the file it used to own (declarative
//! convergence); `uninstall` prunes everything in the lockfile. `check`
//! recomputes the desired state and compares it to disk; `--ci` turns any drift
//! into a non-zero exit.
//!
//! ## Ownership: open-harness never destroys content it did not write
//!
//! The target paths are *shared* — `.vscode/settings.json`, `opencode.json` and
//! `CLAUDE.md` belong to the developer, and open-harness is only one contributor
//! to them. So every write and every removal is gated on **ownership**, decided
//! by comparing the file on disk against the fingerprint the lockfile recorded
//! for it:
//!
//!   * **absent** — created, and owned outright.
//!   * **ours** (matches the recorded fingerprint) — rewritten in place.
//!   * **foreign** (no lock entry, or edited by a human since) — never
//!     overwritten. A **JSON** target is *merged into*, treating the existing
//!     document as one more contributor under the same rules as above, so the
//!     developer's keys survive. A **non-JSON** target can't be merged
//!     structurally, so it is **blocked** and reported with its reason.
//!
//! A file open-harness merged into is recorded as **shared**, along with the
//! exact contribution it made. That makes both directions honest: a re-sync
//! subtracts the old contribution before merging the new one (so a key we stop
//! emitting stops appearing), and a prune subtracts the contribution instead of
//! deleting the file, leaving the developer's content behind. A file that was
//! *edited* since we wrote it is never deleted at all — it is reported for the
//! developer to resolve.
//!
//! Lockfile paths are re-validated on load, not trusted: a lockfile is ordinary
//! data that is often committed, and an entry pointing outside the project
//! (`../…`, `~/…`, absolute) is refused and reported rather than removed. A
//! lockfile that is present but unparseable is reported too, never read as an
//! empty one — that would silently forget every file open-harness manages, so
//! pruning would stop and `--uninstall` would claim a clean removal having
//! removed nothing.
//!
//! The lockfile is also written when a run **fails** partway. `apply` is not
//! atomic — there is no transaction across many files in many directories — so
//! it is instead *honest about what happened*: the first IO error stops the run,
//! everything already written is recorded, everything not yet reached keeps its
//! previous entry, and the error is returned afterwards. Bailing out before
//! writing the lockfile would orphan the files that did land: never pruned,
//! never removed by `--uninstall`, and invisible to the ownership check that
//! stops the next run overwriting them.

use crate::adapters::Harness;
use crate::kind::{kind_impl, Artifact, Installability};
use crate::manifest::LoadedCapability;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Relative location of the sync lockfile inside a synced project.
pub const LOCK_REL: &str = ".open-harness/sync.lock.yaml";

/// The pre-YAML sync lockfile, still read so an already-synced project converges
/// (and prunes correctly) instead of orphaning every file it used to manage.
pub const LEGACY_LOCK_REL: &str = ".open-harness/sync.lock.json";

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

/// The project-relative path a target resolves to, or `None` when it could
/// escape the project tree (a `~` home prefix, an absolute path, or any `..`).
///
/// Returns a **normalized** path built from `Normal` components only, so every
/// caller joins something that is provably inside `root` — `root.join(rel)` on
/// an un-normalized `rel` is not enough, because `Path::starts_with` is lexical
/// and `project/../victim` "starts with" `project`.
///
/// Every filesystem operation goes through this, including ones fed by the
/// lockfile: a lockfile is data on disk (and often committed), so its paths are
/// re-validated rather than trusted.
fn safe_rel(path: &str) -> Option<PathBuf> {
    if path.starts_with('~') {
        return None;
    }
    let mut out = PathBuf::new();
    for c in Path::new(path).components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            // Prefix / RootDir / ParentDir can all leave the project tree.
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Whether a path can be placed into a project tree at all (see [`safe_rel`]).
fn is_placeable(path: &str) -> bool {
    safe_rel(path).is_some()
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
    /// This is what decides **ownership**: a file whose current contents hash to
    /// this value is exactly as open-harness left it and may be rewritten or
    /// removed; anything else carries content we did not write.
    pub fingerprint: String,
    pub harnesses: Vec<String>,
    pub sources: Vec<String>,
    /// True when the file also holds content open-harness did not author, so it
    /// was merged into rather than owned outright. Such a file is never deleted
    /// wholesale — the contribution below is subtracted instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
    /// Exactly what open-harness contributed to a `shared` file, so the next
    /// sync can subtract it before merging the new contribution (otherwise a key
    /// we stop emitting would linger forever) and a prune can subtract it
    /// instead of deleting the developer's file.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub contribution: String,
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
    /// Load the lockfile, **partitioning out any entry whose path could escape
    /// the project**. The refused entries are returned rather than dropped so
    /// the caller reports them: a lockfile is ordinary data on disk, so a
    /// committed (or hand-edited) one must not be able to steer a `remove_file`
    /// at `../../.ssh/authorized_keys`.
    fn load(root: &Path) -> (Lockfile, Vec<Blocked>) {
        // The YAML lockfile wins; a pre-YAML project falls back to the legacy
        // JSON one so its managed paths are still known (and still prunable).
        let mut lock = Lockfile::default();
        let mut refused = Vec::new();
        let mut unreadable: Vec<Blocked> = Vec::new();
        let mut loaded = false;
        for rel in [LOCK_REL, LEGACY_LOCK_REL] {
            let text = match std::fs::read_to_string(root.join(rel)) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    unreadable.push(Blocked {
                        path: rel.to_string(),
                        reason: format!("cannot read the sync lockfile: {e}"),
                    });
                    continue;
                }
            };
            match crate::config::from_str(&text, crate::config::Format::Either) {
                Ok(parsed) => {
                    lock = parsed;
                    loaded = true;
                    break;
                }
                // A lockfile that exists but will not parse is *not* the same as
                // no lockfile. Treating it as empty silently forgets every file
                // open-harness manages: pruning stops, and `--uninstall` reports
                // a clean removal having removed nothing.
                Err(e) => unreadable.push(Blocked {
                    path: rel.to_string(),
                    reason: format!(
                        "the sync lockfile is present but unparseable, so the files it records \
                         cannot be identified: {e}"
                    ),
                }),
            }
        }
        if !loaded {
            refused.append(&mut unreadable);
        }
        lock.managed.retain(|e| {
            if is_placeable(&e.path) {
                return true;
            }
            refused.push(Blocked {
                path: e.path.clone(),
                reason: "lockfile entry points outside the project — refusing to touch it \
                         (remove the entry from the lockfile if it is genuinely stale)"
                    .to_string(),
            });
            false
        });
        (lock, refused)
    }

    fn entry(&self, path: &str) -> Option<&LockEntry> {
        self.managed.iter().find(|e| e.path == path)
    }

    fn managed_paths(&self) -> Vec<String> {
        self.managed.iter().map(|e| e.path.clone()).collect()
    }
}

/// Something open-harness refused to write or remove, with the reason. Surfaced
/// in every report — a refusal is never a silent skip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    pub path: String,
    pub reason: String,
}

// ---- resolving a desired file against what is already on disk --------------

/// A desired file resolved against the current contents of its target path.
#[derive(Debug, Clone)]
struct Resolution {
    /// The exact bytes that should end up on disk, or `None` when open-harness
    /// refuses to write (then `blocked` carries the reason).
    contents: Option<String>,
    /// The target also holds content open-harness did not author.
    shared: bool,
    /// What open-harness contributed, recorded so the next sync can subtract it.
    contribution: String,
    /// Why nothing was written. Reported, never silent.
    blocked: Option<String>,
    /// Clashes resolved while merging into pre-existing content.
    conflicts: Vec<String>,
}

impl Resolution {
    /// The whole file is open-harness's: write the contribution verbatim.
    fn owned(contents: &str) -> Resolution {
        Resolution {
            contents: Some(contents.to_string()),
            shared: false,
            contribution: contents.to_string(),
            blocked: None,
            conflicts: Vec::new(),
        }
    }

    fn blocked(contribution: &str, reason: String) -> Resolution {
        Resolution {
            contents: None,
            shared: false,
            contribution: contribution.to_string(),
            blocked: Some(reason),
            conflicts: Vec::new(),
        }
    }
}

/// Resolve one desired file against its target path (see the module docs on
/// ownership). Reads the target; performs no writes.
fn resolve_target(
    root: &Path,
    rel: &str,
    desired: &str,
    entry: Option<&LockEntry>,
) -> std::io::Result<Resolution> {
    let Some(safe) = safe_rel(rel) else {
        return Ok(Resolution::blocked(
            desired,
            "target path would escape the project tree".to_string(),
        ));
    };
    let on_disk = match std::fs::read_to_string(root.join(safe)) {
        Ok(text) => text,
        // Nothing there: we create the file and own it outright.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Resolution::owned(desired))
        }
        Err(e) => return Err(e),
    };

    // Ownership is decided by the fingerprint we recorded when we last wrote.
    let ours = entry.is_some_and(|e| e.fingerprint == fingerprint(&on_disk));
    let previous = entry
        .filter(|e| e.shared && !e.contribution.is_empty())
        .map(|e| e.contribution.as_str());

    match (ours, previous) {
        // Exactly as we left it, and the whole file was ours: rewrite it.
        (true, None) => Ok(Resolution::owned(desired)),
        // Ours, but the file also carries the developer's content: strip the
        // contribution we made last time before merging the new one, so a key we
        // no longer emit does not linger.
        (true, Some(prev)) => Ok(merge_into(rel, &on_disk, Some(prev), desired)),
        // Foreign — pre-existing, or edited by a human since we wrote it. Never
        // overwrite; merge if the format allows it, otherwise refuse.
        (false, prev) => Ok(merge_into(rel, &on_disk, prev, desired)),
    }
}

/// Merge open-harness's contribution into an existing file, preserving whatever
/// the developer put there. Only structured (JSON) targets can be merged; any
/// other format is refused with its reason rather than clobbered.
fn merge_into(rel: &str, on_disk: &str, previous: Option<&str>, desired: &str) -> Resolution {
    if !is_json_path(rel) {
        return Resolution::blocked(
            desired,
            format!(
                "{rel} already exists and open-harness did not write it; a {} file cannot be \
                 merged structurally, so it is left untouched (move it aside, or delete it, to \
                 let open-harness manage it)",
                Path::new(rel)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("non-JSON")
            ),
        );
    }
    let Ok(existing) = serde_json::from_str::<Value>(on_disk) else {
        return Resolution::blocked(
            desired,
            format!(
                "{rel} already exists and is not valid JSON, so open-harness cannot merge into \
                 it without discarding content (it is left untouched)"
            ),
        );
    };
    let Ok(contribution) = serde_json::from_str::<Value>(desired) else {
        // A kind emitted a `.json` artifact that is not JSON — a bug upstream of
        // here, but not a reason to destroy the developer's file.
        return Resolution::blocked(
            desired,
            format!("{rel}: the planned contents are not valid JSON"),
        );
    };

    // The developer's content is whatever remains once our previous contribution
    // is taken back out.
    let base = match previous.and_then(|p| serde_json::from_str::<Value>(p).ok()) {
        Some(prev) => subtract_json(existing, &prev),
        None => existing,
    };
    let shared = !is_empty_container(&base);

    let mut conflicts = Vec::new();
    let merged = merge_json(
        base,
        contribution,
        rel,
        "open-harness",
        // A clash here is between the developer's value and ours; the merge
        // rules (most-restrictive verdict, else last writer) already apply.
        &mut conflicts,
    );
    let contents = serde_json::to_string_pretty(&merged)
        .map(|s| format!("{s}\n"))
        .unwrap_or_else(|_| merged.to_string());
    Resolution {
        contents: Some(contents),
        shared,
        contribution: desired.to_string(),
        blocked: None,
        conflicts,
    }
}

/// Remove `contribution` from `base` — the inverse of [`merge_json`], used to
/// recover the developer's own content from a file open-harness merged into.
///
/// A key whose value still equals exactly what we contributed is dropped; one
/// the developer has since changed is kept. Arrays give back the items we did
/// not union in. The ambiguous case is a key the developer authored with the
/// same value we did: it is treated as ours and dropped, because there is no
/// record distinguishing the two.
fn subtract_json(base: Value, contribution: &Value) -> Value {
    match (base, contribution) {
        (Value::Object(mut bo), Value::Object(co)) => {
            for (k, cv) in co {
                let Some(bv) = bo.remove(k) else { continue };
                if &bv == cv {
                    continue; // exactly our contribution — take it back out
                }
                let reduced = subtract_json(bv, cv);
                if !is_empty_container(&reduced) {
                    bo.insert(k.clone(), reduced);
                }
            }
            Value::Object(bo)
        }
        (Value::Array(ba), Value::Array(ca)) => {
            Value::Array(ba.into_iter().filter(|i| !ca.contains(i)).collect())
        }
        // A scalar the developer changed out from under us stays theirs.
        (b, _) => b,
    }
}

/// An empty object/array — i.e. nothing of the developer's is left.
fn is_empty_container(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

/// FNV-1a 64-bit — a small, portable, deterministic content fingerprint
/// (stable across platforms/toolchains, unlike `DefaultHasher`).
///
/// Public because it is part of the on-disk lockfile contract: this is the value
/// [`LockEntry::fingerprint`] holds, and the test of whether a file is still
/// open-harness's to rewrite or remove.
pub fn fingerprint(s: &str) -> String {
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
    /// open-harness's contribution was removed from a file it shares with the
    /// developer; the file itself stays, carrying their content.
    Reduce,
}

impl ChangeAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeAction::Create => "create",
            ChangeAction::Update => "update",
            ChangeAction::Unchanged => "unchanged",
            ChangeAction::Prune => "prune",
            ChangeAction::Reduce => "reduce",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApplyChange {
    pub path: String,
    pub action: ChangeAction,
    pub harnesses: Vec<String>,
    pub sources: Vec<String>,
    /// The file also carries content open-harness did not author, so it was
    /// merged into rather than overwritten.
    pub shared: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ApplyReport {
    pub dry_run: bool,
    pub changes: Vec<ApplyChange>,
    pub deferred: Vec<Deferred>,
    /// (path, conflict messages) for every file whose merge resolved a clash.
    pub conflicts: Vec<(String, Vec<String>)>,
    /// Files open-harness refused to write or remove, each with its reason —
    /// a pre-existing file it cannot merge into, one edited since it was
    /// written, or a lockfile entry pointing outside the project.
    pub blocked: Vec<Blocked>,
}

impl ApplyReport {
    pub fn count(&self, action: ChangeAction) -> usize {
        self.changes.iter().filter(|c| c.action == action).count()
    }
    /// Did anything actually change on disk (create/update/prune/reduce)?
    pub fn mutated(&self) -> bool {
        self.changes
            .iter()
            .any(|c| c.action != ChangeAction::Unchanged)
    }
    /// Was anything left unrealized? The caller's non-zero-exit condition.
    pub fn has_blocked(&self) -> bool {
        !self.blocked.is_empty()
    }
}

/// Realize a plan into `root`: write desired files, prune managed files no
/// longer desired, and rewrite the lockfile. Idempotent — a second run with the
/// same inputs reports only `Unchanged`. With `dry_run`, computes the report but
/// touches nothing.
pub fn apply(root: &Path, plan: &SyncPlan, dry_run: bool) -> std::io::Result<ApplyReport> {
    let (lock, mut blocked) = Lockfile::load(root);
    let desired: Vec<String> = plan.files.iter().map(|f| f.path.clone()).collect();

    let mut changes = Vec::new();
    let mut new_entries = Vec::new();
    let mut conflicts: Vec<(String, Vec<String>)> = Vec::new();

    // A write that fails partway must still leave a truthful lockfile: files
    // already on disk but unrecorded would be orphaned — never pruned, never
    // removed by `--uninstall`, invisible to the ownership check that stops the
    // next run clobbering them. So the first IO error stops the run and is
    // returned *after* recording everything that did land.
    let mut fatal: Option<std::io::Error> = None;

    for f in &plan.files {
        let resolved = match resolve_target(root, &f.path, &f.contents, lock.entry(&f.path)) {
            Ok(r) => r,
            Err(e) => {
                fatal = Some(e);
                break;
            }
        };

        // Merging into a developer's file can resolve clashes of its own; report
        // them next to the ones the capability set produced.
        let mut messages = f.conflicts.clone();
        messages.extend(resolved.conflicts.iter().cloned());
        if !messages.is_empty() {
            conflicts.push((f.path.clone(), messages));
        }

        let Some(contents) = resolved.contents else {
            // Refused: keep neither a lock entry (we own nothing here) nor a
            // change — the file is untouched and reported as blocked.
            blocked.push(Blocked {
                path: f.path.clone(),
                reason: resolved.blocked.unwrap_or_default(),
            });
            continue;
        };

        let action = match classify(root, &f.path, &contents) {
            Ok(a) => a,
            Err(e) => {
                fatal = Some(e);
                break;
            }
        };
        if !dry_run && matches!(action, ChangeAction::Create | ChangeAction::Update) {
            if let Err(e) = write_file(root, &f.path, &contents) {
                fatal = Some(e);
                break;
            }
        }
        // Recorded only once the bytes are actually down.
        new_entries.push(LockEntry {
            path: f.path.clone(),
            // The fingerprint covers what actually lands on disk (the merged
            // file), not just our contribution — it is the ownership test.
            fingerprint: fingerprint(&contents),
            harnesses: f.harnesses.clone(),
            sources: f.sources.clone(),
            shared: resolved.shared,
            contribution: if resolved.shared {
                resolved.contribution
            } else {
                String::new()
            },
        });
        changes.push(ApplyChange {
            path: f.path.clone(),
            action,
            harnesses: f.harnesses.clone(),
            sources: f.sources.clone(),
            shared: resolved.shared,
        });
    }

    // Prune: previously-managed paths that are no longer desired. Skipped once a
    // write has failed — the run is being abandoned, and removing files while the
    // desired state is half-written would compound it.
    for entry in &lock.managed {
        if desired.contains(&entry.path) {
            continue;
        }
        if fatal.is_some() {
            // Not processed, so it stays ours to prune on the next run.
            new_entries.push(entry.clone());
            continue;
        }
        match remove_entry(root, entry, dry_run) {
            Ok(Removal::Done(action)) => changes.push(ApplyChange {
                path: entry.path.clone(),
                action,
                harnesses: entry.harnesses.clone(),
                sources: entry.sources.clone(),
                shared: entry.shared,
            }),
            Ok(Removal::Refused(reason)) => {
                blocked.push(Blocked {
                    path: entry.path.clone(),
                    reason,
                });
                // Still ours in spirit but not removable — keep tracking it so a
                // later run can retry rather than forgetting the file exists.
                new_entries.push(entry.clone());
            }
            Err(e) => {
                fatal = Some(e);
                new_entries.push(entry.clone());
            }
        }
    }

    if !dry_run {
        // Keep any entry this run never reached, so an abandoned run forgets
        // nothing it had previously written.
        if fatal.is_some() {
            for entry in &lock.managed {
                if !new_entries.iter().any(|e| e.path == entry.path) {
                    new_entries.push(entry.clone());
                }
            }
        }
        new_entries.sort_by(|a, b| a.path.cmp(&b.path));
        new_entries.dedup_by(|a, b| a.path == b.path);
        let lock_written = write_lockfile(
            root,
            &Lockfile {
                version: 1,
                managed: new_entries,
            },
        );
        // The original failure is the one worth reporting; a lockfile write that
        // also fails only matters if nothing else did.
        match (&fatal, lock_written) {
            (Some(_), _) => {}
            (None, Err(e)) => return Err(e),
            (None, Ok(())) => {}
        }
    }

    if let Some(e) = fatal {
        return Err(e);
    }

    changes.sort_by(|a, b| a.path.cmp(&b.path));
    blocked.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ApplyReport {
        dry_run,
        changes,
        deferred: plan.deferred.clone(),
        conflicts,
        blocked,
    })
}

/// Remove open-harness's footprint from `root`: every file the lockfile records
/// as ours, and the lockfile itself. A file the developer shares with us has our
/// contribution subtracted instead of being deleted, and a file edited since we
/// wrote it is left alone and reported — uninstalling never destroys content
/// open-harness did not write.
pub fn uninstall(root: &Path, dry_run: bool) -> std::io::Result<ApplyReport> {
    let (lock, mut blocked) = Lockfile::load(root);
    let mut changes = Vec::new();
    let mut kept: Vec<LockEntry> = Vec::new();
    for entry in &lock.managed {
        match remove_entry(root, entry, dry_run)? {
            Removal::Done(action) => changes.push(ApplyChange {
                path: entry.path.clone(),
                action,
                harnesses: entry.harnesses.clone(),
                sources: entry.sources.clone(),
                shared: entry.shared,
            }),
            Removal::Refused(reason) => {
                blocked.push(Blocked {
                    path: entry.path.clone(),
                    reason,
                });
                kept.push(entry.clone());
            }
        }
    }
    if !dry_run {
        if kept.is_empty() {
            // Nothing left to track: drop the lockfile (and a legacy JSON one
            // left by a pre-YAML sync) so the project is clean.
            for rel in [LOCK_REL, LEGACY_LOCK_REL] {
                remove_path(root, rel)?;
            }
        } else {
            // Keep a record of what we could not remove, so it is not orphaned.
            kept.sort_by(|a, b| a.path.cmp(&b.path));
            write_lockfile(
                root,
                &Lockfile {
                    version: 1,
                    managed: kept,
                },
            )?;
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    blocked.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ApplyReport {
        dry_run,
        changes,
        deferred: Vec::new(),
        conflicts: Vec::new(),
        blocked,
    })
}

/// The outcome of trying to take one managed file back off disk.
enum Removal {
    Done(ChangeAction),
    /// Left in place, with the reason — the developer resolves it.
    Refused(String),
}

/// Remove one lockfile-managed file, honoring ownership: a file that no longer
/// matches the fingerprint we recorded is not ours to delete, and a `shared`
/// file has our contribution subtracted rather than being deleted.
fn remove_entry(root: &Path, entry: &LockEntry, dry_run: bool) -> std::io::Result<Removal> {
    let Some(safe) = safe_rel(&entry.path) else {
        return Ok(Removal::Refused(
            "lockfile entry points outside the project — refusing to remove it".to_string(),
        ));
    };
    let on_disk = match std::fs::read_to_string(root.join(&safe)) {
        Ok(text) => text,
        // Already gone: the desired end state, nothing to do.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Removal::Done(ChangeAction::Prune))
        }
        Err(e) => return Err(e),
    };

    if fingerprint(&on_disk) != entry.fingerprint {
        return Ok(Removal::Refused(format!(
            "{} has been edited since open-harness wrote it — leaving it in place (delete it by \
             hand if the edit is not worth keeping)",
            entry.path
        )));
    }

    // A file we merged into belongs to the developer too: take back only what we
    // contributed and leave the rest.
    if entry.shared && !entry.contribution.is_empty() {
        if let (Ok(current), Ok(contribution)) = (
            serde_json::from_str::<Value>(&on_disk),
            serde_json::from_str::<Value>(&entry.contribution),
        ) {
            let remainder = subtract_json(current, &contribution);
            if !is_empty_container(&remainder) {
                if !dry_run {
                    let text = serde_json::to_string_pretty(&remainder)
                        .map(|s| format!("{s}\n"))
                        .unwrap_or_else(|_| remainder.to_string());
                    write_file(root, &entry.path, &text)?;
                }
                return Ok(Removal::Done(ChangeAction::Reduce));
            }
            // Nothing of theirs left — the file was effectively ours after all.
        }
    }

    if !dry_run {
        remove_path(root, &entry.path)?;
    }
    Ok(Removal::Done(ChangeAction::Prune))
}

fn classify(root: &Path, rel: &str, contents: &str) -> std::io::Result<ChangeAction> {
    let Some(safe) = safe_rel(rel) else {
        return Ok(ChangeAction::Unchanged);
    };
    match std::fs::read_to_string(root.join(safe)) {
        Ok(on_disk) if on_disk == contents => Ok(ChangeAction::Unchanged),
        Ok(_) => Ok(ChangeAction::Update),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ChangeAction::Create),
        Err(e) => Err(e),
    }
}

fn write_file(root: &Path, rel: &str, contents: &str) -> std::io::Result<()> {
    let safe = safe_rel(rel).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to write outside the project: {rel}"),
        )
    })?;
    let full = root.join(safe);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(full, contents)
}

fn write_lockfile(root: &Path, lock: &Lockfile) -> std::io::Result<()> {
    let text = crate::config::to_yaml(lock).unwrap_or_else(|_| "{}\n".to_string());
    write_file(root, LOCK_REL, &text)?;
    // Writing the YAML lockfile supersedes the legacy JSON one; leaving it
    // behind would let a later run load stale managed paths.
    remove_path(root, LEGACY_LOCK_REL)
}

/// Delete one project-relative path, refusing anything that could resolve
/// outside `root`. The backstop under [`remove_entry`]; also used directly for
/// open-harness's own lockfiles, whose paths are constants.
fn remove_path(root: &Path, rel: &str) -> std::io::Result<()> {
    let safe = safe_rel(rel).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to remove outside the project: {rel}"),
        )
    })?;
    let full = root.join(&safe);
    if full.exists() {
        std::fs::remove_file(&full)?;
        prune_empty_dirs(root, &safe);
    }
    Ok(())
}

/// Walk up from a just-removed file removing now-empty directories, stopping at
/// the project root. `rel` is a normalized project-relative path (see
/// [`safe_rel`]), so only genuine subdirectories of `root` are ever joined —
/// containment is structural rather than a lexical prefix test, which `..` would
/// defeat. Best-effort: a non-empty or unreadable directory ends the walk.
fn prune_empty_dirs(root: &Path, rel: &Path) {
    let mut cur = rel.parent();
    while let Some(sub) = cur {
        if sub.as_os_str().is_empty() {
            break; // reached the project root — never remove it
        }
        let dir = root.join(sub);
        if !matches!(
            std::fs::read_dir(&dir).map(|mut e| e.next().is_none()),
            Ok(true)
        ) {
            break;
        }
        if std::fs::remove_dir(&dir).is_err() {
            break;
        }
        cur = sub.parent();
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
    /// Targets open-harness could not realize at all, with the reason. These
    /// count as drift: the desired state is not on disk.
    pub blocked: Vec<Blocked>,
}

impl DriftReport {
    /// Any missing / modified / stale file, or any blocked target — the CI
    /// failure condition.
    pub fn has_drift(&self) -> bool {
        self.items.iter().any(|i| i.kind.is_drift()) || !self.blocked.is_empty()
    }
    pub fn count(&self, kind: DriftKind) -> usize {
        self.items.iter().filter(|i| i.kind == kind).count()
    }
}

/// Compare a plan against the current on-disk state of `root`. Read-only.
///
/// Resolves each target exactly as [`apply`] would — including merging into a
/// file the developer owns — so "in sync" means a sync really would be a no-op.
pub fn check(root: &Path, plan: &SyncPlan) -> std::io::Result<DriftReport> {
    let (lock, mut blocked) = Lockfile::load(root);
    let desired: Vec<String> = plan.files.iter().map(|f| f.path.clone()).collect();
    let mut items = Vec::new();

    for f in &plan.files {
        let resolved = resolve_target(root, &f.path, &f.contents, lock.entry(&f.path))?;
        // A blocked target is drift twice over: the file on disk is not what the
        // plan wants, and a sync would not fix it. Report both facts.
        let contents = match &resolved.contents {
            Some(c) => c.clone(),
            None => {
                blocked.push(Blocked {
                    path: f.path.clone(),
                    reason: resolved.blocked.clone().unwrap_or_default(),
                });
                f.contents.clone()
            }
        };
        let kind = match classify(root, &f.path, &contents)? {
            ChangeAction::Unchanged => DriftKind::Ok,
            ChangeAction::Create => DriftKind::Missing,
            _ => DriftKind::Modified,
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
        let still_there = safe_rel(&entry.path).is_some_and(|p| root.join(p).exists());
        if !desired.contains(&entry.path) && still_there {
            items.push(DriftItem {
                path: entry.path.clone(),
                kind: DriftKind::Stale,
                harnesses: entry.harnesses.clone(),
                sources: entry.sources.clone(),
            });
        }
    }

    items.sort_by(|a, b| a.path.cmp(&b.path));
    blocked.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(DriftReport {
        items,
        deferred: plan.deferred.clone(),
        blocked,
    })
}

/// Convenience: discover capabilities under `caps_dir`, plan them for `harnesses`.
pub fn plan_from_dir(caps_dir: &Path, harnesses: &[Harness]) -> Result<SyncPlan, String> {
    let caps = crate::manifest::discover(caps_dir)?;
    Ok(plan_sync(&caps, harnesses))
}

/// The lockfile-managed paths currently recorded under `root` (for reporting).
pub fn managed_paths(root: &Path) -> Vec<String> {
    Lockfile::load(root).0.managed_paths()
}
