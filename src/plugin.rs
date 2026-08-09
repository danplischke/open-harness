//! Importing a **harness-native plugin bundle** as open-harness capabilities.
//!
//! Claude Code plugins are a real, populated ecosystem, and their layout is a
//! de-facto standard other tools have started copying. A plugin is a directory
//! with a `.claude-plugin/plugin.json` and some subset of:
//!
//! ```text
//! plugin-root/
//!   .claude-plugin/plugin.json    identity + version
//!   skills/<id>/SKILL.md          portable
//!   commands/<id>.md              portable
//!   agents/<id>.md                portable
//!   .mcp.json                     portable-ish (MCP is the shared standard)
//!   hooks/hooks.json              NOT portable — Claude's own hook schema
//! ```
//!
//! Importing one is reverse-compilation: the same job the adapters do forwards,
//! run backwards from one harness's dialect into the canonical model. Which
//! means the honesty rule applies with more force than usual, because a plugin
//! is *not* uniformly portable:
//!
//! * **skills / commands / agents** are already the shapes open-harness
//!   defines. They import as themselves and fan out to every harness that has
//!   the concept.
//! * **`.mcp.json`** imports as `tool` capabilities. MCP is the shared standard,
//!   so this genuinely travels — with the caveat that a plugin's server command
//!   often points into that harness's own install layout, which is recorded as
//!   a degradation note rather than pretended away.
//! * **`hooks/hooks.json`** does not travel at all. It is opaque shell wired to
//!   Claude's native hook schema, frequently hunting Claude's plugin cache at
//!   runtime. It imports as a [`crate::kinds::native`] capability: written
//!   verbatim on Claude Code, `Unsupported` with a reason on the other ten.
//!
//! Dropping the hooks silently would be the easy lie; claiming they compile
//! would be the worse one. Carrying them as native says exactly what you got.

use crate::kind::KindId;
use crate::manifest::{LoadedCapability, Manifest};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// A plugin's own manifest (`.claude-plugin/plugin.json`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
}

/// A marketplace index (`.claude-plugin/marketplace.json`) — one repository
/// publishing several plugins, each at its own subdirectory.
#[derive(Debug, Clone, Default, Deserialize)]
struct Marketplace {
    #[serde(default)]
    plugins: Vec<MarketplaceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceEntry {
    name: String,
    /// Where the plugin actually lives, relative to the repository root.
    #[serde(default)]
    source: Option<String>,
}

/// What a bundle produced, plus everything that was not straightforwardly
/// portable — reported, never dropped.
pub struct Imported {
    pub manifest: PluginManifest,
    pub capabilities: Vec<LoadedCapability>,
    pub notes: Vec<String>,
}

/// Locate a plugin root inside `repo`.
///
/// A repository is either a plugin itself (`.claude-plugin/plugin.json` at the
/// root) or a marketplace publishing several. `want` picks one by name from a
/// marketplace; without it a single-plugin marketplace resolves unambiguously
/// and a multi-plugin one is an error listing the choices, because guessing
/// which of somebody's plugins you meant is not a decision to make silently.
pub fn find_plugin_root(repo: &Path, want: Option<&str>) -> Result<PathBuf, String> {
    let direct = repo.join(".claude-plugin/plugin.json");
    let market = repo.join(".claude-plugin/marketplace.json");

    if market.is_file() {
        let text = std::fs::read_to_string(&market)
            .map_err(|e| format!("read {}: {e}", market.display()))?;
        let index: Marketplace =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", market.display()))?;

        let entry = match (want, index.plugins.len()) {
            (Some(name), _) => index
                .plugins
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| {
                    let names: Vec<&str> = index.plugins.iter().map(|p| p.name.as_str()).collect();
                    format!(
                        "marketplace at {} publishes no plugin '{name}'. Available: {}",
                        repo.display(),
                        if names.is_empty() {
                            "(none)".to_string()
                        } else {
                            names.join(", ")
                        }
                    )
                })?,
            (None, 1) => &index.plugins[0],
            (None, 0) => {
                // An empty marketplace beside a real plugin manifest is fine.
                if direct.is_file() {
                    return Ok(repo.to_path_buf());
                }
                return Err(format!(
                    "marketplace at {} publishes no plugins",
                    repo.display()
                ));
            }
            (None, _) => {
                let names: Vec<&str> = index.plugins.iter().map(|p| p.name.as_str()).collect();
                return Err(format!(
                    "marketplace at {} publishes {} plugins ({}) — name the one you want \
                     with `plugin: {{ name: … }}`",
                    repo.display(),
                    names.len(),
                    names.join(", ")
                ));
            }
        };
        let rel = entry.source.as_deref().unwrap_or(".");
        return Ok(repo.join(rel.trim_start_matches("./")));
    }

    if direct.is_file() {
        return Ok(repo.to_path_buf());
    }
    Err(format!(
        "no .claude-plugin/plugin.json or marketplace.json under {} — not a plugin bundle",
        repo.display()
    ))
}

/// Import the plugin rooted at `root` into capabilities.
pub fn import(root: &Path) -> Result<Imported, String> {
    let manifest_path = root.join(".claude-plugin/plugin.json");
    let manifest: PluginManifest = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => {
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?
        }
        Err(e) => return Err(format!("read {}: {e}", manifest_path.display())),
    };
    if manifest.name.is_empty() {
        return Err(format!("{} declares no name", manifest_path.display()));
    }

    let mut out = Imported {
        capabilities: Vec::new(),
        notes: Vec::new(),
        manifest,
    };
    let version = if out.manifest.version.is_empty() {
        "0.0.0".to_string()
    } else {
        out.manifest.version.clone()
    };

    import_documents(root, &version, &mut out)?;
    import_mcp(root, &version, &mut out)?;
    import_hooks(root, &version, &mut out)?;

    if out.capabilities.is_empty() {
        out.notes.push(format!(
            "plugin '{}' contained nothing open-harness can import (no skills, \
             commands, agents, MCP servers or hooks)",
            out.manifest.name
        ));
    }
    Ok(out)
}

/// The directories whose documents map straight onto an open-harness kind, and
/// whether each is a directory-per-capability or a file-per-capability.
const DOC_DIRS: &[(&str, KindId, &str, bool)] = &[
    ("skills", KindId::Skill, "SKILL.md", true),
    ("commands", KindId::Command, "COMMAND.md", false),
    ("agents", KindId::Agent, "AGENT.md", false),
];

/// Skills, commands and agents — already the shapes open-harness defines, so
/// these import as themselves.
fn import_documents(root: &Path, version: &str, out: &mut Imported) -> Result<(), String> {
    for (dir_name, kind, doc_name, dir_per_cap) in DOC_DIRS {
        let dir = root.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| format!("scan {}: {e}", dir.display()))?
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();

        for entry in entries {
            let doc = if *dir_per_cap {
                if !entry.is_dir() {
                    continue;
                }
                entry.join(doc_name)
            } else {
                if entry.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                entry.clone()
            };
            if !doc.is_file() {
                continue;
            }
            // The document *is* the manifest — the same single-file authoring
            // path open-harness already supports, so this is a load, not a
            // translation.
            let mut cap = LoadedCapability::from_doc(&doc, *kind)?;
            if *dir_per_cap {
                // id defaults to the directory name, which is what we want.
            } else if let Some(stem) = entry.file_stem().and_then(|s| s.to_str()) {
                cap.manifest.id = stem.to_string();
            }
            if cap.manifest.version == "0.0.0" {
                cap.manifest.version = version.to_string();
            }
            out.capabilities.push(cap);
        }
    }
    Ok(())
}

/// `.mcp.json` → one `tool` capability per server. MCP is the shared standard,
/// so these genuinely travel.
fn import_mcp(root: &Path, version: &str, out: &mut Imported) -> Result<(), String> {
    let path = root.join(".mcp.json");
    if !path.is_file() {
        return Ok(());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(servers) = doc.get("mcpServers").and_then(|v| v.as_object()) else {
        out.notes
            .push(format!("{}: no `mcpServers` object", path.display()));
        return Ok(());
    };

    for (name, spec) in servers {
        let command = spec.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let args = spec.get("args").cloned().unwrap_or_else(|| json!([]));
        let mut server = json!({ "transport": "stdio", "command": command, "args": args });
        if let (Some(url), Some(o)) = (spec.get("url"), server.as_object_mut()) {
            o.insert("transport".into(), json!("http"));
            o.insert("url".into(), url.clone());
        }
        if let (Some(env), Some(o)) = (spec.get("env"), server.as_object_mut()) {
            o.insert("env".into(), env.clone());
        }

        let manifest: Manifest = serde_json::from_value(json!({
            "id": name,
            "name": name,
            "description": format!("MCP server '{name}' imported from the {} plugin", out.manifest.name),
            "kind": "tool",
            "version": version,
            "tool": { "provider": "mcp", "delivery": "auto", "server": server },
        }))
        .map_err(|e| format!("{}: server '{name}': {e}", path.display()))?;

        // A plugin's server command routinely resolves against that harness's
        // own plugin cache. The MCP *config* is portable; whether the server
        // starts elsewhere is not something we can promise.
        out.notes.push(format!(
            "MCP server '{name}' imported as a portable tool, but its command \
             (`{command}`) may resolve against Claude Code's own plugin install — \
             check it before relying on it on another harness"
        ));
        out.capabilities.push(LoadedCapability {
            manifest,
            dir: root.to_path_buf(),
        });
    }
    Ok(())
}

/// `hooks/hooks.json` → a single `native` capability pinned to Claude Code.
///
/// This is the part that cannot be made portable, and the reason the `native`
/// kind exists. The file is carried byte-for-byte.
fn import_hooks(root: &Path, version: &str, out: &mut Imported) -> Result<(), String> {
    let path = root.join("hooks/hooks.json");
    if !path.is_file() {
        return Ok(());
    }
    let body =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    // Parse only to count and name the events, so the note is specific. The
    // body written out is the original text, unmodified.
    let events: Vec<String> = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("hooks")
                .and_then(|h| h.as_object())
                .map(|o| o.keys().cloned().collect())
        })
        .unwrap_or_default();

    let plugin = &out.manifest.name;
    let id = format!("{plugin}-hooks");
    let reason = format!(
        "Claude Code hook bundle from the '{plugin}' plugin — opaque commands against \
         Claude's native hook schema, not the open-harness `hook@1` contract, so it \
         has no equivalent here"
    );
    let manifest: Manifest = serde_json::from_value(json!({
        "id": id,
        "name": format!("{plugin} hooks"),
        "description": format!(
            "Native Claude Code hooks imported from the '{plugin}' plugin{}",
            if events.is_empty() { String::new() } else { format!(" ({})", events.join(", ")) }
        ),
        "kind": "native",
        "version": version,
        "native": {
            "harness": "claude-code",
            "reason": reason,
            "artifacts": [{ "path": format!(".claude/hooks/{plugin}.json"), "body": body }],
        },
    }))
    .map_err(|e| format!("{}: {e}", path.display()))?;

    out.notes.push(format!(
        "{} hook(s) ({}) imported as a claude-code-native capability — they are not \
         `hook@1` capabilities and will be reported Unsupported on every other harness",
        events.len().max(1),
        if events.is_empty() {
            "unparsed".to_string()
        } else {
            events.join(", ")
        }
    ));
    out.capabilities.push(LoadedCapability {
        manifest,
        dir: root.to_path_buf(),
    });
    Ok(())
}
