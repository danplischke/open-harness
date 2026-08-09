//! Adapter provenance: what is *supported* versus what has been *verified*.
//!
//! The support matrix says what each normalized event becomes on each harness.
//! It cannot say whether that mapping was ever exercised against the real
//! harness — and for eight of eleven adapters it has not been. A claim like
//! that is worthless if it can quietly overstate itself, so these tests hold
//! `Harness::provenance()` against the fixture corpus actually committed to
//! this repo: declaring a recorded payload requires one to exist, and
//! declaring the strongest form (`live-captured`) requires the sidecar that
//! only `oh capture` writes.

use open_harness::adapters::{Harness, Provenance, ALL};
use open_harness::capture;
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture_dir(h: Harness) -> PathBuf {
    fixtures_root().join(h.id())
}

/// The recorded native payloads committed for a harness.
fn recorded_payloads(h: Harness) -> Vec<PathBuf> {
    let dir = fixture_dir(h);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".stdin.json"))
        })
        .collect();
    out.sort();
    out
}

/// The `oh capture` sidecars committed for a harness — the only evidence that
/// distinguishes a live recording from a payload transcribed out of vendor docs.
fn live_sidecars(h: Harness) -> Vec<PathBuf> {
    let dir = fixture_dir(h);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(capture::PROVENANCE_SUFFIX))
        })
        .filter(|p| {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| {
                    v.get("kind")
                        .and_then(|k| k.as_str())
                        .map(|k| k == capture::LIVE_CAPTURED)
                })
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

/// The core gate. An adapter may not claim more evidence than exists on disk.
#[test]
fn no_adapter_claims_more_evidence_than_the_repo_holds() {
    for h in ALL {
        let payloads = recorded_payloads(h);
        let sidecars = live_sidecars(h);
        match h.provenance() {
            Provenance::LiveCaptured(_) => {
                assert!(
                    !payloads.is_empty(),
                    "{} claims live-captured but tests/fixtures/{}/ holds no *.stdin.json",
                    h.id(),
                    h.id()
                );
                assert!(
                    !sidecars.is_empty(),
                    "{} claims live-captured but no `oh capture` sidecar records a live run — \
                     a fixture alone cannot tell a recording from a transcription",
                    h.id()
                );
            }
            Provenance::DocFixture(_) => {
                assert!(
                    !payloads.is_empty(),
                    "{} claims doc-fixture but tests/fixtures/{}/ holds no *.stdin.json",
                    h.id(),
                    h.id()
                );
                assert!(
                    sidecars.is_empty(),
                    "{} holds a live-capture sidecar but still claims doc-fixture — \
                     upgrade it to Provenance::LiveCaptured",
                    h.id()
                );
            }
            Provenance::DocOnly(_) => {
                assert!(
                    payloads.is_empty(),
                    "{} claims doc-only but {:?} are committed — the declaration understates \
                     the evidence; upgrade it to DocFixture (or LiveCaptured with a sidecar)",
                    h.id(),
                    payloads
                        .iter()
                        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}

/// Every fixture directory belongs to a harness. A stray one means a fixture
/// nothing decodes, which is worse than no fixture: it looks like coverage.
#[test]
fn every_fixture_directory_maps_to_a_harness() {
    let Ok(entries) = std::fs::read_dir(fixtures_root()) else {
        panic!("tests/fixtures is missing");
    };
    for e in entries.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        assert!(
            Harness::from_id(&name).is_some(),
            "tests/fixtures/{name}/ is not a harness id — nothing decodes it"
        );
    }
}

/// Provenance must be stated for every harness, and must say what it was
/// established against. An empty source is a claim with no basis.
#[test]
fn every_adapter_states_what_it_was_established_against() {
    for h in ALL {
        let p = h.provenance();
        assert!(
            p.source().len() > 10,
            "{} gives no meaningful provenance source: {:?}",
            h.id(),
            p.source()
        );
        assert!(!p.label().is_empty());
        assert!(!p.caveat().is_empty());
    }
}

/// The honest headline: today nothing is live-captured. This is not a wish —
/// it fails the moment a real capture lands, which is the prompt to update the
/// README's claim along with it.
#[test]
fn the_current_state_is_that_no_adapter_is_live_captured_yet() {
    let live: Vec<&str> = ALL
        .iter()
        .filter(|h| matches!(h.provenance(), Provenance::LiveCaptured(_)))
        .map(|h| h.id())
        .collect();
    assert!(
        live.is_empty(),
        "{live:?} are now live-captured — good; update this test and the README's \
         \"encoded from documentation\" caveat to match"
    );
    let backed = ALL.iter().filter(|h| h.provenance().has_fixture()).count();
    assert_eq!(
        backed, 2,
        "the number of fixture-backed adapters changed; update the README count"
    );
}

/// `oh capture` must write the sidecar, or the upgrade path to `live-captured`
/// does not exist.
#[test]
fn capturing_a_payload_records_its_provenance() {
    let dir = std::env::temp_dir().join(format!("oh-prov-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fixture = dir.join("beforeShellExecution.stdin.json");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_oh"))
        .args([
            "capture",
            "--harness",
            "cursor",
            "--event",
            "pre.tool.shell",
            "--label",
            "Cursor 1.7.44",
            "--out",
        ])
        .arg(&fixture)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin
                .as_mut()
                .unwrap()
                .write_all(br#"{"command":"ls","hook_event_name":"beforeShellExecution"}"#)?;
            c.wait_with_output()
        })
        .expect("run oh capture");
    assert!(out.status.success(), "capture must allow (exit 0)");

    let sidecar = dir.join("beforeShellExecution.provenance.json");
    assert!(
        sidecar.is_file(),
        "no sidecar at {} — stderr: {}",
        sidecar.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(doc["kind"], capture::LIVE_CAPTURED);
    assert_eq!(doc["harness"], "cursor");
    assert_eq!(doc["event"], "pre.tool.shell");
    assert_eq!(doc["harness_version"], "Cursor 1.7.44");
    assert_eq!(doc["fixture"], "beforeShellExecution.stdin.json");
    // The age of a capture is part of what it is worth, so it must be dated.
    let at = doc["recorded_at"].as_str().expect("recorded_at");
    assert!(
        at.len() == 20 && at.ends_with('Z') && at.contains('T'),
        "recorded_at should be an RFC 3339 UTC instant, got {at:?}"
    );
    let year: i32 = at[..4].parse().expect("a year");
    assert!(
        (2024..2100).contains(&year),
        "the clock conversion is wrong: {at}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sidecar sits next to its fixture and is named for it, so the pair is
/// obvious in a directory listing and in a diff.
#[test]
fn the_sidecar_is_named_for_the_fixture_it_describes() {
    let p = capture::provenance_path(Path::new("tests/fixtures/cursor/beforeReadFile.stdin.json"));
    assert_eq!(
        p,
        Path::new("tests/fixtures/cursor/beforeReadFile.provenance.json")
    );
    // A fixture that is not `.stdin.json` still gets a sensible sidecar.
    let p = capture::provenance_path(Path::new("captured.json"));
    assert_eq!(p, Path::new("captured.provenance.json"));
}

/// The report has to actually reach the user, in both renderings.
#[test]
fn the_matrix_reports_provenance_alongside_support() {
    let md = open_harness::matrix::markdown();
    assert!(
        md.contains("## Adapter provenance"),
        "the markdown matrix must carry the provenance table"
    );
    for h in ALL {
        assert!(
            md.contains(h.provenance().label()),
            "{} provenance missing from the markdown matrix",
            h.id()
        );
    }
    let text = open_harness::matrix::provenance_text();
    for h in ALL {
        assert!(
            text.contains(h.id()),
            "{} missing from the text report",
            h.id()
        );
    }
}

/// `oh emit` output gets pasted into a real config; that is precisely where an
/// unverified mapping needs to say so.
#[test]
fn emit_warns_that_a_doc_only_adapter_is_unverified() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(["emit", "--harness", "windsurf"])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("run oh emit");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("no recorded payload") && text.contains("windsurf"),
        "emit should flag windsurf as unverified:\n{text}"
    );
    // Comment-prefixed, so pasting the output into a config stays safe.
    for line in text.lines().take_while(|l| !l.trim().is_empty()) {
        assert!(
            line.starts_with('#'),
            "the note must be commented out: {line}"
        );
    }
}

/// ...and a fixture-backed adapter must not be dragged into that warning.
#[test]
fn a_fixture_backed_adapter_is_not_flagged_as_unverified() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(["emit", "--harness", "cursor"])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("run oh emit");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("no recorded payload"),
        "cursor is fixture-backed and should not be flagged:\n{}",
        text.lines().take(4).collect::<Vec<_>>().join("\n")
    );
}
