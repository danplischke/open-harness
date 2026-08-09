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
//!
//! ## The provenance sidecar
//!
//! A recorded payload and one transcribed from a vendor's documentation are
//! indistinguishable as bytes, so the fixture alone cannot support the claim
//! that an adapter was verified against the real thing. Every capture therefore
//! writes a [`Provenance`] sidecar next to the fixture recording that it came
//! off a live install, when, and with which harness version. That sidecar is
//! what lets [`crate::adapters::Provenance::LiveCaptured`] be asserted for a
//! harness — `tests/provenance.rs` refuses the claim without one.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The suffix of the sidecar written next to a captured fixture.
pub const PROVENANCE_SUFFIX: &str = ".provenance.json";

/// The `kind` a sidecar carries when the fixture came off a real install. This
/// is the string `tests/provenance.rs` looks for when deciding whether an
/// adapter may claim [`crate::adapters::Provenance::LiveCaptured`].
pub const LIVE_CAPTURED: &str = "live-captured";

/// What was recorded, alongside the payload itself.
///
/// A fixture on its own cannot say where it came from — a payload transcribed
/// from a vendor's docs and one recorded off a live install are the same bytes.
/// The sidecar is what separates them, and it is what lets the support matrix
/// claim `live-captured` for a harness without anyone having to take that on
/// trust.
pub struct Provenance<'a> {
    pub harness: Option<&'a str>,
    pub event: Option<&'a str>,
    /// Free text from `--label`, for the harness version the capture came from.
    pub label: Option<&'a str>,
}

/// The sidecar path for a fixture: `x.stdin.json` → `x.provenance.json`.
///
/// The `.stdin` segment is dropped so the pair reads as one record rather than
/// as `x.stdin.provenance.json`.
pub fn provenance_path(fixture: &Path) -> PathBuf {
    let name = fixture
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("captured");
    let stem = name
        .strip_suffix(".json")
        .unwrap_or(name)
        .strip_suffix(".stdin")
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.strip_suffix(".json").unwrap_or(name).to_string());
    fixture.with_file_name(format!("{stem}{PROVENANCE_SUFFIX}"))
}

/// Write the sidecar recording that `fixture` came from a live capture.
pub fn record_provenance(fixture: &Path, p: &Provenance) -> Result<PathBuf, String> {
    let path = provenance_path(fixture);
    let mut doc = serde_json::Map::new();
    doc.insert("kind".into(), serde_json::json!(LIVE_CAPTURED));
    doc.insert(
        "fixture".into(),
        serde_json::json!(fixture
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()),
    );
    if let Some(h) = p.harness {
        doc.insert("harness".into(), serde_json::json!(h));
    }
    if let Some(e) = p.event {
        doc.insert("event".into(), serde_json::json!(e));
    }
    if let Some(l) = p.label {
        doc.insert("harness_version".into(), serde_json::json!(l));
    }
    doc.insert(
        "recorded_by".into(),
        serde_json::json!(format!("oh {}", env!("CARGO_PKG_VERSION"))),
    );
    // A capture's age is part of what it is worth: a payload recorded two years
    // ago is weak evidence about a harness that ships weekly.
    doc.insert("recorded_at".into(), serde_json::json!(now_utc()));

    let text =
        serde_json::to_string_pretty(&serde_json::Value::Object(doc)).map_err(|e| e.to_string())?;
    std::fs::write(&path, format!("{text}\n"))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// The current time as an RFC 3339 UTC timestamp.
///
/// Hand-rolled rather than pulling a date crate for one string: this is the
/// same line the project draws elsewhere — simple, fully-specified formats are
/// written here, genuinely complex ones (YAML, TLS) get a library.
fn now_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Days since the Unix epoch → (year, month, day). Hinnant's civil-from-days:
/// shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

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
