//! open-harness's own config files: **YAML on the way out, YAML-or-JSON on the
//! way in**.
//!
//! Everything open-harness itself defines — the capability manifest, the profile,
//! both lockfiles, the registry index, the trust store, keyrings, key files, and
//! detached signatures — is a YAML document, loaded and written through here.
//!
//! ## What this module does *not* touch
//!
//! A harness's own config is written in *that harness's* format, not ours:
//! `.claude/settings.json`, `opencode.json`, `.mcp.json`,
//! `cline_mcp_settings.json`, `.vscode/settings.json`, Codex/Gemini TOML.
//! Emitting YAML there would produce files no harness can read, so
//! [`crate::sync`]'s JSON deep-merge composition stays exactly as it is. Also
//! unchanged: the frozen `hook@1` wire protocol (JSON on stdin/stdout), the
//! JSON Schemas under `spec/`, `oh capture` fixtures (recordings of *native*
//! payloads), and [`crate::api`]'s JSON-string FFI boundary.
//!
//! The split is the point: this module is the boundary between open-harness's
//! config language and everyone else's.
//!
//! ## Reading
//!
//! Deserialization goes through `serde_json::Value`, which
//! [`crate::yaml::parse_value`] produces from YAML — so every `Deserialize`
//! derive in the crate works against YAML input unchanged, including
//! `#[serde(flatten)]` and `deny_unknown_fields`.
//!
//! JSON is still read, so a pre-YAML checkout keeps working: a file with a
//! `.json` extension is parsed as JSON, and an extension-less file (`*.lock`,
//! `capability.sig`) is tried as YAML and then as JSON. `oh migrate` rewrites
//! them in place.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The serialization format of a config file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Yaml,
    Json,
    /// No (or an unrecognized) extension — try YAML, then JSON.
    Either,
}

impl Format {
    /// The format implied by a path's extension.
    pub fn of(path: &Path) -> Format {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("yaml") | Some("yml") => Format::Yaml,
            Some("json") => Format::Json,
            _ => Format::Either,
        }
    }
}

/// Config file extensions in **precedence order**: YAML first, JSON last, so a
/// migrated file always wins over a leftover one.
pub const EXTENSIONS: &[&str] = &["yaml", "yml", "json"];

/// The first existing `<stem>.<ext>` in [`EXTENSIONS`] order, if any.
///
/// `stem` is a path *without* an extension (e.g. `…/capability`). This is how a
/// directory is probed for a config that may be authored either way.
pub fn find(stem: &Path) -> Option<PathBuf> {
    EXTENSIONS
        .iter()
        .map(|ext| stem.with_extension(ext))
        .find(|p| p.is_file())
}

/// Parse config text into a [`Value`] according to `format`.
pub fn parse(text: &str, format: Format) -> Result<Value, String> {
    match format {
        Format::Yaml => crate::yaml::parse_value(text),
        Format::Json => serde_json::from_str(text).map_err(|e| e.to_string()),
        // YAML 1.2 is very nearly a JSON superset, but "very nearly" is not a
        // guarantee to rest a config loader on — so JSON gets a real second
        // attempt, and the YAML error is what surfaces if both fail (YAML is
        // the format we tell people to write).
        Format::Either => match crate::yaml::parse_value(text) {
            Ok(v) => Ok(v),
            Err(yaml_err) => serde_json::from_str(text).map_err(|_| yaml_err),
        },
    }
}

/// Deserialize `text` as a config document of type `T`.
pub fn from_str<T: DeserializeOwned>(text: &str, format: Format) -> Result<T, String> {
    let value = parse(text, format)?;
    serde_json::from_value(value).map_err(|e| e.to_string())
}

/// Read and deserialize the config file at `path`, choosing the format from its
/// extension. Errors name the file, so a bad config says *which* one.
pub fn load<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    from_str(&text, Format::of(path)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Render a value as a YAML config document (newline-terminated).
pub fn to_yaml<T: Serialize>(value: &T) -> Result<String, String> {
    let v = serde_json::to_value(value).map_err(|e| e.to_string())?;
    Ok(crate::yaml::to_document(&v))
}

/// Write `value` to `path` as YAML, creating parent directories as needed.
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = to_yaml(value)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

/// The YAML path a legacy config should migrate to: `.json` becomes `.yaml`, and
/// an extension-less file (`*.lock`, `capability.sig`) keeps its name — the
/// contents change format, the filename doesn't.
pub fn migrated_path(path: &Path) -> PathBuf {
    match Format::of(path) {
        Format::Json => path.with_extension("yaml"),
        _ => path.to_path_buf(),
    }
}

/// Rewrite one config file from JSON to YAML, preserving key order.
///
/// Returns the path written, or `None` if the file was already YAML (parsing it
/// as JSON failed), so a re-run is a no-op rather than a churned file.
pub fn migrate_file(path: &Path) -> Result<Option<PathBuf>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Ok(None); // not JSON — already migrated
    };
    let target = migrated_path(path);
    std::fs::write(&target, crate::yaml::to_document(&value))
        .map_err(|e| format!("write {}: {e}", target.display()))?;
    if target != path {
        std::fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))?;
    }
    Ok(Some(target))
}
