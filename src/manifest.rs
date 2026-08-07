//! Capability manifest: what an OSS author ships so their capability can be
//! installed into any harness. Language-agnostic: `run` is just a command line.

use crate::event::{Boundary, NormEvent, Phase, SubjectKind, TaskKind, ToolClass};
use crate::kind::KindId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct RunSpec {
    /// Interpreter/binary, e.g. `python3`, `node`, `./guard` (a compiled bin) —
    /// or, when `interpreter` is set, the **script** to run through it.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional logical interpreter (`python` / `node` / `bash` / …). When set,
    /// `command` is the script and the runtime resolves the interpreter per-OS,
    /// so the capability never relies on a `#!` shebang (Windows has none).
    #[serde(default)]
    pub interpreter: Option<String>,
}

/// What the dispatcher does when a bound capability *fails to produce a
/// decision* (spawn error, timeout, non-zero exit, bad JSON, protocol mismatch).
/// Chosen per binding so a hard guard and a soft advisor can share a runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailPolicy {
    /// Fail-closed on blocking events (deny), fail-open otherwise. The default —
    /// a crashing guard must never silently allow the action it was guarding.
    #[default]
    Auto,
    /// Always treat an error as a deny, even on non-blocking events.
    FailClosed,
    /// Always treat an error as an allow — the capability contributes nothing.
    FailOpen,
    /// Like fail-open, but reported distinctly: "no opinion", not "allow".
    PassThrough,
}

impl FailPolicy {
    /// Resolve to the concrete action for an event of the given blocking-ness.
    /// `true` = contribute a deny; `false` = contribute nothing.
    pub fn denies(&self, blocking: bool) -> bool {
        match self {
            FailPolicy::Auto => blocking,
            FailPolicy::FailClosed => true,
            FailPolicy::FailOpen | FailPolicy::PassThrough => false,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            FailPolicy::Auto => "auto",
            FailPolicy::FailClosed => "fail-closed",
            FailPolicy::FailOpen => "fail-open",
            FailPolicy::PassThrough => "pass-through",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventBinding {
    pub phase: Phase,
    pub subject: SubjectKind,
    #[serde(default)]
    pub tool_class: Option<ToolClass>,
    #[serde(default)]
    pub boundary: Option<Boundary>,
    #[serde(default)]
    pub task_kind: Option<TaskKind>,
    /// If true, a harness that cannot host this event makes the whole capability
    /// uninstallable there (loud failure). If false, it's silently skipped on
    /// harnesses that lack the event.
    #[serde(default)]
    pub required: bool,
    /// What to do if this capability errors on this event. Defaults to `auto`.
    #[serde(default)]
    pub on_error: FailPolicy,
}

impl EventBinding {
    pub fn event(&self) -> NormEvent {
        NormEvent {
            phase: self.phase,
            subject: self.subject,
            tool_class: self.tool_class,
            boundary: self.boundary,
            task_kind: self.task_kind,
        }
    }
}

/// A capability's declared permission manifest (#17) — what it expects to read,
/// execute, and reach over the network. Surfaced for consent on install and
/// checked against a [`PermissionPolicy`] (advisory).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Permissions {
    /// Path globs the capability expects to read.
    #[serde(default)]
    pub read: Vec<String>,
    /// Commands/binaries the capability expects to execute.
    #[serde(default)]
    pub exec: Vec<String>,
    /// Network reach: `[]` = none, `["*"]` = any host, else specific hosts.
    #[serde(default)]
    pub network: Vec<String>,
}

impl Permissions {
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.exec.is_empty() && self.network.is_empty()
    }
    pub fn requests_network(&self) -> bool {
        !self.network.is_empty()
    }
    pub fn network_summary(&self) -> String {
        if self.network.is_empty() {
            "none".to_string()
        } else if self.network.iter().any(|h| h == "*") {
            "any".to_string()
        } else {
            self.network.join(",")
        }
    }
    pub fn summary(&self) -> String {
        format!(
            "read={:?} exec={:?} network={}",
            self.read,
            self.exec,
            self.network_summary()
        )
    }
}

/// What the host permits capabilities to do. A violation is advisory by default.
#[derive(Debug, Clone, Copy)]
pub struct PermissionPolicy {
    pub allow_network: bool,
    pub allow_exec: bool,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        PermissionPolicy {
            allow_network: true,
            allow_exec: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Capability kind. Defaults to `hook` so pre-existing hook manifests load
    /// unchanged.
    #[serde(default)]
    pub kind: KindId,
    /// Hook execution entrypoint. Required for `hook` capabilities; absent for
    /// generative kinds (skills, rules, ...).
    #[serde(default)]
    pub run: Option<RunSpec>,
    #[serde(default)]
    pub events: Vec<EventBinding>,
    /// Wire-protocol version this capability speaks, e.g. `"hook@1"`. Optional;
    /// when present the dispatcher refuses a major-version mismatch.
    #[serde(default)]
    pub protocol: Option<String>,
    /// Per-capability execution timeout (ms). Overrides the runtime default;
    /// the capability is killed and its failure policy applied if exceeded.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Package version (semver-ish string). Recorded in the profile lockfile so
    /// a resolved capability set is pinned + reproducible. Defaults to `0.0.0`.
    #[serde(default = "default_version")]
    pub version: String,
    /// Other capability ids this one expects to be composed alongside. The
    /// resolver warns if a declared dependency is absent from the profile.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Declared permission manifest (read / exec / network). Surfaced for
    /// consent and checked against the host policy (advisory).
    #[serde(default)]
    pub permissions: Permissions,
    /// Per-harness overrides: `{"<harness-id>": { "path": …, "tools": […],
    /// "frontmatter": { … } }}`. Lets one canonical capability tune its own
    /// output for a specific target without forking it (adopted by the skill and
    /// agent kinds). See [`LoadedCapability::harness_override`].
    #[serde(default)]
    pub overrides: std::collections::BTreeMap<String, serde_json::Value>,
    /// Kind-specific configuration (e.g. the `skill` block). Each kind parses
    /// what it needs from here; unknown keys are ignored.
    #[serde(flatten)]
    pub config: serde_json::Map<String, serde_json::Value>,
}

fn default_version() -> String {
    "0.0.0".to_string()
}

/// A per-harness override block, resolved from [`Manifest::overrides`]. Every
/// field is optional; an absent block yields the default (no overrides).
#[derive(Debug, Clone, Default)]
pub struct HarnessOverride {
    /// Replace the kind's default output path for this harness.
    pub path: Option<String>,
    /// Replace the kind's tool list for this harness (native tokens, verbatim).
    pub tools: Option<Vec<String>>,
    /// Extra/replacement frontmatter keys merged into the rendered block.
    pub frontmatter: serde_json::Map<String, serde_json::Value>,
}

/// A manifest plus the directory it was loaded from (the cwd used when the
/// capability process is spawned, so relative `run.args` paths resolve).
#[derive(Debug, Clone)]
pub struct LoadedCapability {
    pub manifest: Manifest,
    pub dir: PathBuf,
}

impl LoadedCapability {
    pub fn load(manifest_path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(manifest_path)
            .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
        let manifest: Manifest = serde_json::from_str(&text)
            .map_err(|e| format!("parse {}: {e}", manifest_path.display()))?;
        let dir = manifest_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(LoadedCapability { manifest, dir })
    }

    /// The first binding that matches this event, if any. A `tool.any` binding
    /// matches any tool event; a specific class matches only its class.
    pub fn matching_binding(&self, ev: &NormEvent) -> Option<&EventBinding> {
        self.manifest.events.iter().find(|b| {
            let be = b.event();
            if be.phase != ev.phase || be.subject != ev.subject {
                return false;
            }
            match (be.tool_class, ev.tool_class) {
                (Some(ToolClass::Any), Some(_)) | (Some(ToolClass::Any), None) => true,
                (Some(a), Some(b)) => a == b,
                (None, _) => be.boundary == ev.boundary && be.task_kind == ev.task_kind,
                (Some(_), None) => false,
            }
        })
    }

    /// Does this capability bind the given normalized event?
    pub fn binds(&self, ev: &NormEvent) -> bool {
        self.matching_binding(ev).is_some()
    }

    /// The failure policy for this event — the matching binding's `on_error`,
    /// or the `auto` default if unbound.
    pub fn fail_policy(&self, ev: &NormEvent) -> FailPolicy {
        self.matching_binding(ev)
            .map(|b| b.on_error)
            .unwrap_or_default()
    }

    /// The resolved per-harness override for `harness_id` (see
    /// [`Manifest::overrides`]). Returns the default (no overrides) when absent
    /// or malformed — an override is a convenience, never load-bearing.
    pub fn harness_override(&self, harness_id: &str) -> HarnessOverride {
        let Some(obj) = self
            .manifest
            .overrides
            .get(harness_id)
            .and_then(|v| v.as_object())
        else {
            return HarnessOverride::default();
        };
        let path = obj
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let tools = obj.get("tools").and_then(|v| v.as_array()).map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        });
        let frontmatter = obj
            .get("frontmatter")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        HarnessOverride {
            path,
            tools,
            frontmatter,
        }
    }
}

/// Scan a directory for `*/capability.json` files.
pub fn discover(dir: &Path) -> Result<Vec<LoadedCapability>, String> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("scan {}: {e}", dir.display()))?;
    for e in entries.flatten() {
        let p = e.path().join("capability.json");
        if p.is_file() {
            out.push(LoadedCapability::load(&p)?);
        }
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(out)
}
