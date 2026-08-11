//! Capability manifest: what an OSS author ships so their capability can be
//! installed into any harness. Language-agnostic: `run` is just a command line.
//!
//! The manifest is a YAML document (`capability.yaml`) loaded through
//! [`crate::config`], which also still reads a legacy `capability.json`.

use crate::event::{Boundary, NormEvent, Phase, SubjectKind, TaskKind, ToolClass};
use crate::kind::KindId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    /// Path globs the capability expects to **write**.
    ///
    /// Anything that persists state writes to disk — a memory store's SQLite
    /// file, a phase gate's JSON, a transcript recorder's JSONL — and without
    /// this scope a manifest showed a truthful read/exec/network picture with no
    /// hint that the thing also touches your filesystem. For a project that
    /// makes trust a headline, silent write access was the conspicuous hole in
    /// the vocabulary, and no amount of sandboxing could close it: enforcement
    /// needs something to enforce.
    ///
    /// Declarative, like its siblings — see SECURITY.md for what is enforced
    /// versus surfaced.
    #[serde(default)]
    pub write: Vec<String>,
    /// Commands/binaries the capability expects to execute.
    #[serde(default)]
    pub exec: Vec<String>,
    /// Network reach: `[]` = none, `["*"]` = any host, else specific hosts.
    #[serde(default)]
    pub network: Vec<String>,
}

impl Permissions {
    pub fn is_empty(&self) -> bool {
        self.read.is_empty()
            && self.write.is_empty()
            && self.exec.is_empty()
            && self.network.is_empty()
    }
    pub fn requests_write(&self) -> bool {
        !self.write.is_empty()
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
            "read={:?} write={:?} exec={:?} network={}",
            self.read,
            self.write,
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
    pub allow_write: bool,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        PermissionPolicy {
            allow_network: true,
            allow_exec: true,
            allow_write: true,
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
    /// Per-harness overrides: `{"<harness-id>": { … }}`. Lets one canonical
    /// capability tune its own output for a specific target without forking it
    /// (adopted by the skill and agent kinds).
    ///
    /// `path`, `tools` and `frontmatter` are structural; every other key in the
    /// block is frontmatter. See [`LoadedCapability::harness_override`].
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
    /// Anything the resolution had to point out — a key that looks like a
    /// misspelling of a reserved one, say. Surfaced through the kind's plan.
    pub notes: Vec<String>,
}

/// The three keys an override block gives a structural meaning. Everything else
/// in the block is frontmatter.
const OVERRIDE_KEYS: [&str; 3] = ["path", "tools", "frontmatter"];

/// The reserved override key `key` is probably a misspelling of, if any.
///
/// Deliberately tight (one edit, and never an exact match of another reserved
/// key) so a real frontmatter key is not accused of being a typo.
fn near_miss(key: &str) -> Option<&'static str> {
    OVERRIDE_KEYS
        .iter()
        .find(|reserved| edit_distance(key, reserved) == 1)
        .copied()
}

/// Optimal string alignment distance — Levenshtein plus an adjacent
/// **transposition** at cost 1.
///
/// The transposition is the point: `paht` is how `path` actually gets mistyped,
/// and plain Levenshtein scores that 2, which is indistinguishable from an
/// unrelated key. (`crate::help` has a sibling of this for command names; if a
/// third caller appears it is worth extracting.)
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// A JSON value's type, for an error message that says what was found.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "a mapping",
    }
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
        Self::from_doc_named(doc_path, kind_hint, None)
    }

    /// As [`from_doc`](Self::from_doc), but `id_hint` replaces the directory-name
    /// fallback for the capability's id. Used when the containing directory is a
    /// checkout cache slug rather than a name the author chose.
    pub fn from_doc_named(
        doc_path: &Path,
        kind_hint: KindId,
        id_hint: Option<&str>,
    ) -> Result<Self, String> {
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
            .or_else(|| id_hint.map(|s| s.to_string()))
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
    pub fn harness_override(&self, harness_id: &str) -> Result<HarnessOverride, String> {
        let Some(value) = self.manifest.overrides.get(harness_id) else {
            return Ok(HarnessOverride::default());
        };
        let Some(obj) = value.as_object() else {
            return Err(format!(
                "`overrides.{harness_id}` must be a mapping, got {}",
                type_name(value)
            ));
        };

        let mut out = HarnessOverride::default();
        for (key, v) in obj {
            match key.as_str() {
                "path" => match v.as_str() {
                    Some(s) => out.path = Some(s.to_string()),
                    // Silently ignoring this relocated nothing while reporting
                    // success — the file lands where the kind chose and the
                    // author is never told their override did nothing.
                    None => {
                        return Err(format!(
                            "`overrides.{harness_id}.path` must be a string, got {}",
                            type_name(v)
                        ))
                    }
                },
                "tools" => match v {
                    Value::Array(a) => {
                        out.tools = Some(
                            a.iter()
                                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                                .collect(),
                        )
                    }
                    // The native scalar spelling (`"Read Grep Bash(git diff:*)"`)
                    // is what these tokens look like in a harness's own file, and
                    // it is accepted everywhere else a tool list is read.
                    Value::String(s) => out.tools = Some(crate::tools::split_tool_string(s)),
                    _ => {
                        return Err(format!(
                            "`overrides.{harness_id}.tools` must be a list or a string, got {}",
                            type_name(v)
                        ))
                    }
                },
                "frontmatter" => match v.as_object() {
                    Some(m) => {
                        // Merged after the loop so an explicit block wins over a
                        // flat key of the same name.
                        for (k, val) in m {
                            out.frontmatter.insert(k.clone(), val.clone());
                        }
                    }
                    None => {
                        return Err(format!(
                            "`overrides.{harness_id}.frontmatter` must be a mapping, got {}",
                            type_name(v)
                        ))
                    }
                },
                // Anything else is frontmatter. This is the form authors
                // actually write — `overrides: {opencode: {mode: primary}}` —
                // and requiring the `frontmatter:` nesting meant those keys were
                // dropped with no warning and the capability still reported
                // "installable — clean". The nested form is now sugar for saying
                // the same thing explicitly, not a requirement.
                other => {
                    if !out.frontmatter.contains_key(other) {
                        out.frontmatter.insert(other.to_string(), v.clone());
                    }
                    // A near-miss of a reserved key is worth saying out loud: it
                    // will be emitted as frontmatter, which is *probably* not
                    // what `paht:` meant. It is only a note, never an error —
                    // `paths:` is a real frontmatter key on Cline and Claude
                    // rules, so refusing it would break a legitimate override.
                    if let Some(meant) = near_miss(other) {
                        out.notes.push(format!(
                            "`overrides.{harness_id}.{other}` was emitted as frontmatter; if you \
                             meant the `{meant}` override, spell it exactly"
                        ));
                    }
                }
            }
        }
        // The explicit block wins on conflict — re-apply it over the flat keys.
        if let Some(m) = obj.get("frontmatter").and_then(|v| v.as_object()) {
            for (k, val) in m {
                out.frontmatter.insert(k.clone(), val.clone());
            }
        }
        Ok(out)
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
    discover_named(dir, None)
}

/// Like [`discover`], but `dir` may itself **be** a single capability rather than
/// a container of them — the shape you get when a whole repository is one
/// capability (`git+https://github.com/me/my-skill`), the way a Python package is
/// installed straight from a repo. Without this, that layout finds nothing, and
/// finding nothing looks exactly like there being nothing wrong.
///
/// `id_hint` names that root capability when its document carries no explicit
/// `id`. Normally the id falls back to the directory name, but the directory
/// here is a checkout cache slug that means nothing to the author, so the caller
/// passes something meaningful (a git source passes the repository name).
pub fn discover_named(dir: &Path, id_hint: Option<&str>) -> Result<Vec<LoadedCapability>, String> {
    if let Some(cap) = load_capability_dir_named(dir, id_hint)? {
        return Ok(vec![cap]);
    }
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
    load_capability_dir_named(dir, None)
}

/// `id_hint` supplies the id for a single-file capability whose document has no
/// explicit `id` and whose directory name is not meaningful — see
/// [`discover_named`]. A manifest file always carries its own id, so the hint
/// never applies there.
fn load_capability_dir_named(
    dir: &Path,
    id_hint: Option<&str>,
) -> Result<Option<LoadedCapability>, String> {
    if let Some(manifest) = crate::config::find(&dir.join(MANIFEST_STEM)) {
        return Ok(Some(LoadedCapability::load(&manifest)?));
    }
    for (name, kind) in DOC_FILES {
        let doc = dir.join(name);
        if doc.is_file() {
            return Ok(Some(LoadedCapability::from_doc_named(
                &doc, *kind, id_hint,
            )?));
        }
    }
    Ok(None)
}
