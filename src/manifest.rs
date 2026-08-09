//! Capability manifest: what an OSS author ships so their capability can be
//! installed into any harness. Language-agnostic: `run` is just a command line.
//!
//! The manifest is a YAML document (`capability.yaml`) loaded through
//! [`crate::config`], which also still reads a legacy `capability.json`.

use crate::event::{Boundary, NormEvent, Phase, SubjectKind, TaskKind, ToolClass};
use crate::kind::KindId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The manifest filename without its extension. [`crate::config::find`] probes
/// `capability.yaml`, `capability.yml`, then `capability.json`.
pub const MANIFEST_STEM: &str = "capability";

/// The canonical manifest filename, for messages and scaffolding.
pub const MANIFEST_NAME: &str = "capability.yaml";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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
///
/// `deny_unknown_fields` so a wrong-shaped value (e.g. an agent's tool→verdict
/// map `{bash: ask}` mistakenly placed at the manifest's top-level `permissions`)
/// is a loud error, not silently absorbed into an all-empty manifest.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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

/// What must already exist on the host for a capability's `run` entrypoint to
/// work — the *runtime* it needs, as opposed to the capabilities it depends on.
///
/// open-harness deliberately does not install any of this. A capability's
/// dependencies are its ecosystem's problem, and there are better answers than a
/// package manager we would have to write: ship a compiled binary, vendor the
/// dependencies into the capability (which puts them inside the content digest,
/// so the author's signature covers them), or declare them the language-native
/// way and let `uv` / `npm` provision. See `docs/src/runtimes.md`.
///
/// What open-harness owes you is to *notice* — before a missing interpreter
/// turns into a denied tool call.
///
/// `deny_unknown_fields` so `require:` or `requires_bin:` is a loud error rather
/// than a block that silently checks nothing.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    /// Executables that must be resolvable on `PATH`, e.g. `[uv]` or `[node]`.
    /// Checked before spawning, and reported by `oh check` / `oh doctor`.
    #[serde(default)]
    pub requires: Vec<String>,
    /// How to make this capability's dependencies ready — `uv sync --script`,
    /// `npm ci`, or whatever its ecosystem uses.
    ///
    /// **Never run by `sync`.** Only `oh install --runtimes` runs it, and only
    /// when explicitly confirmed. Installing config and executing a package
    /// manager are different acts and do not share a verb: `pip`/`npm` run
    /// arbitrary code at install time (build backends, `postinstall`), which is
    /// a larger hole than the one [`crate::profile::TransitiveTrust`] closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision: Option<Provision>,
}

/// A provisioning command. Deliberately not a [`RunSpec`]: there is no
/// interpreter indirection here, just an executable and its arguments, run in
/// the capability's own directory.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provision {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl Provision {
    /// The command as a human would type it — what `oh install --runtimes`
    /// shows before running anything.
    pub fn display(&self) -> String {
        if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }
}

impl Runtime {
    pub fn is_empty(&self) -> bool {
        self.requires.is_empty() && self.provision.is_none()
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
    /// Explicit namespace, e.g. `acme`. Usually left unset — the resolver
    /// derives one from the source (a git repo's `<owner>/<repo>`, a registry
    /// entry's name), and an author only sets this to pin their own. See
    /// [`LoadedCapability::qualified_name`].
    #[serde(default)]
    pub namespace: Option<String>,
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
    /// Other capabilities this one relates to — `requires` / `suggests` /
    /// `conflicts` / `replaces`, each with a version requirement. Accepts a bare
    /// list of names (any version, `requires`) or a name → requirement map. See
    /// [`crate::deps`].
    #[serde(default)]
    pub dependencies: crate::deps::Dependencies,
    /// Declared permission manifest (read / exec / network). Surfaced for
    /// consent and checked against the host policy (advisory).
    #[serde(default)]
    pub permissions: Permissions,
    /// Executables this capability's `run` entrypoint needs on `PATH`. Checked
    /// before spawning and reported by `oh check` / `oh doctor`; never installed.
    #[serde(default)]
    pub runtime: Runtime,
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
    /// The capability's **fully-qualified name**: `<namespace>/<id>`, or the
    /// bare `id` when it has no namespace.
    ///
    /// A bare `id` is only unique within one tree. The moment sources are other
    /// people's repositories, two authors both shipping `mem-search` are
    /// different capabilities, and "first source wins" would silently pick one.
    /// The qualified name is what dependencies resolve against and what the lock
    /// pins; `id` stays the local alias, and is still what the emitted native
    /// filenames use.
    pub fn qualified_name(&self) -> String {
        match &self.manifest.namespace {
            Some(ns) if !ns.is_empty() => format!("{ns}/{}", self.manifest.id),
            _ => self.manifest.id.clone(),
        }
    }

    /// Adopt `namespace` unless the manifest already declares one — an author's
    /// explicit namespace outranks whatever the source implies.
    pub fn adopt_namespace(&mut self, namespace: Option<&str>) {
        if self.manifest.namespace.is_none() {
            self.manifest.namespace = namespace.map(|s| s.to_string());
        }
    }

    pub fn load(manifest_path: &Path) -> Result<Self, String> {
        let manifest: Manifest = crate::config::load(manifest_path)?;
        let dir = manifest_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(LoadedCapability { manifest, dir })
    }

    /// Load a **single-file** capability: a document (e.g. `SKILL.md`) whose YAML
    /// frontmatter *is* the manifest and whose body is the content. `id` defaults
    /// to the directory name and `kind` to `kind_hint` (from the filename) unless
    /// the frontmatter sets them. The frontmatter's non-reserved keys plus the
    /// body become the kind's config block, so the same `Kind` impl runs unchanged.
    pub fn from_doc(doc_path: &Path, kind_hint: KindId) -> Result<Self, String> {
        let text = std::fs::read_to_string(doc_path)
            .map_err(|e| format!("read {}: {e}", doc_path.display()))?;
        let (mut fm, body) = crate::yaml::parse_document(&text)
            .map_err(|e| format!("{}: {e}", doc_path.display()))?;
        let dir = doc_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let id = fm
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
            .ok_or_else(|| format!("{}: cannot determine capability id", doc_path.display()))?;
        let kind = match fm.get("kind").and_then(|v| v.as_str()) {
            Some(k) => KindId::parse(k)
                .ok_or_else(|| format!("{}: unknown kind '{k}'", doc_path.display()))?,
            None => kind_hint,
        };
        let kind_str = kind.as_str();

        // Reserved keys populate the manifest header; everything else + the body
        // becomes the kind's config block. Build the JSON the deserializer wants.
        fm.remove("id");
        fm.remove("kind");
        let mut header = serde_json::Map::new();
        header.insert("id".into(), serde_json::Value::String(id));
        header.insert(
            "kind".into(),
            serde_json::Value::String(kind_str.to_string()),
        );
        // NB: `permissions` is deliberately NOT lifted. At the manifest level it
        // is the sandbox manifest (read/exec/network), but single-file authoring
        // is only for document kinds — none of which execute or need a sandbox —
        // and an agent's `permissions:` (tool→verdict) must reach its config, not
        // be captured as the sandbox and silently loosen `ask` to `allow`.
        for key in [
            "namespace",
            "runtime",
            "name",
            "description",
            "version",
            "dependencies",
            "overrides",
            "protocol",
            "timeout_ms",
            "run",
            "events",
        ] {
            if let Some(v) = fm.remove(key) {
                header.insert(key.to_string(), v);
            }
        }
        let mut config = fm; // whatever remains is kind-specific config
        if !body.trim().is_empty() {
            config.insert("body".into(), serde_json::Value::String(body));
        }
        header.insert(kind_str.to_string(), serde_json::Value::Object(config));

        let manifest: Manifest = serde_json::from_value(serde_json::Value::Object(header))
            .map_err(|e| format!("{}: {e}", doc_path.display()))?;
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

/// The single-file capability documents recognized by [`discover`], mapping the
/// canonical filename to the kind it implies (the frontmatter may override).
const DOC_FILES: &[(&str, KindId)] = &[
    ("SKILL.md", KindId::Skill),
    ("AGENT.md", KindId::Agent),
    ("COMMAND.md", KindId::Command),
    ("RULE.md", KindId::Rule),
    ("INSTRUCTIONS.md", KindId::Instructions),
];

/// Scan `dir` for capabilities, **recursing** through category subdirectories.
///
/// `dir` is a container. Each descendant directory that holds a `capability.yaml`
/// or a single-file document (`SKILL.md`, `AGENT.md`, …) is one capability,
/// authored either way (when both are present, `capability.yaml` wins). A
/// directory that *is* a capability is **not** descended into — its
/// subdirectories are its own assets — so a category tree like
/// `skills/gcp/cloud-run-basics/SKILL.md` is found while a capability's internals
/// are left alone.
///
/// Hidden directories (dot-prefixed, e.g. `.git` / `.open-harness`) and symlinks
/// are skipped (the latter keeps the walk cycle-safe). A malformed capability is
/// a hard error, never a silent skip — nothing is dropped without a reason.
pub fn discover(dir: &Path) -> Result<Vec<LoadedCapability>, String> {
    let mut out = Vec::new();
    scan_container(dir, &mut out)?;
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(out)
}

/// Treat `dir` as a container: recurse into each real, non-hidden subdirectory,
/// in a deterministic (name-sorted) order.
fn scan_container(dir: &Path, out: &mut Vec<LoadedCapability>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("scan {}: {e}", dir.display()))?;
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for e in entries.flatten() {
        // `file_type` does not traverse a symlink, so `is_dir()` is true only for
        // a real directory — this skips files and symlinked dirs (no cycles).
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        subdirs.push(e.path());
    }
    subdirs.sort();
    for sub in subdirs {
        scan_capability_or_category(&sub, out)?;
    }
    Ok(())
}

/// `dir` is either a capability (load it and stop) or a category (recurse).
fn scan_capability_or_category(dir: &Path, out: &mut Vec<LoadedCapability>) -> Result<(), String> {
    if let Some(cap) = load_capability_dir(dir)? {
        out.push(cap);
        return Ok(());
    }
    scan_container(dir, out)
}

/// Load the capability rooted directly at `dir`, if any: a `capability.yaml`
/// (or `.yml` / legacy `.json`) wins, otherwise the first recognized single-file
/// document.
fn load_capability_dir(dir: &Path) -> Result<Option<LoadedCapability>, String> {
    if let Some(manifest) = crate::config::find(&dir.join(MANIFEST_STEM)) {
        return Ok(Some(LoadedCapability::load(&manifest)?));
    }
    for (name, kind) in DOC_FILES {
        let doc = dir.join(name);
        if doc.is_file() {
            return Ok(Some(LoadedCapability::from_doc(&doc, *kind)?));
        }
    }
    Ok(None)
}
