//! `oh capture` — record a harness's raw native hook payload into a fixture (#24).
//!
//! **Honest scope.** This is the *tooling* for live capture, not the captured
//! corpus itself. Wire `oh capture` as a harness's hook entrypoint and it records
//! the exact native payload that harness sends on stdin into a fixture file, then
//! allows (exit 0) so nothing is disrupted:
//!
//! ```text
//! # e.g. as Cursor's beforeShellExecution hook:
//! oh capture --harness cursor --event pre.tool.shell \
//!            --out tests/fixtures/cursor/beforeShellExecution.stdin.json
//! ```
//!
//! Running it against each *real* harness needs those harnesses installed, which
//! is a manual step this repo can't perform in CI. The captured fixtures are the
//! same raw-native shape as the hand-recorded ones under `tests/fixtures/`, so
//! they drop straight into the conformance suite (which decodes each fixture
//! through its adapter). The Codex + Cursor fixtures were hand-recorded (#5);
//! `oh capture` is how the rest get recorded from live runs.

use std::path::Path;

/// Record a raw native payload (as received on a harness's hook stdin) into `out`
/// as a pretty-printed JSON fixture. The payload is validated as JSON so the
/// fixture is adapter-consumable; empty input is rejected.
pub fn record_fixture(native: &str, out: &Path) -> Result<(), String> {
    let trimmed = native.trim();
    if trimmed.is_empty() {
        return Err("no native payload on stdin to capture".to_string());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("captured payload is not valid JSON: {e}"))?;
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    std::fs::write(out, format!("{text}\n")).map_err(|e| format!("write {}: {e}", out.display()))
}
