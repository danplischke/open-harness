//! Capability manifest: what an OSS author ships so their capability can be
//! installed into any harness. Language-agnostic: `run` is just a command line.

use crate::event::{Boundary, NormEvent, Phase, SubjectKind, TaskKind, ToolClass};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct RunSpec {
    /// Interpreter or binary, e.g. `python3`, `node`, `./guard` (a compiled bin).
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub run: RunSpec,
    #[serde(default)]
    pub events: Vec<EventBinding>,
    /// Wire-protocol version this capability speaks, e.g. `"hook@1"`. Optional;
    /// when present the dispatcher refuses a major-version mismatch.
    #[serde(default)]
    pub protocol: Option<String>,
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

    /// Does this capability bind the given normalized event? A `tool.any`
    /// binding matches any tool event; a specific class matches only its class.
    pub fn binds(&self, ev: &NormEvent) -> bool {
        self.manifest.events.iter().any(|b| {
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
