//! `oh capture` fixture-recording tests (#24). Cross-platform: the capture
//! tooling is exercised through the library, so no live harness is needed.

use open_harness::adapters::Harness;
use open_harness::capture::record_fixture;
use open_harness::event::NormEvent;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-capture-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A captured native payload is written verbatim and decodes through its adapter
/// exactly like a hand-recorded fixture — so live capture feeds the same suite.
#[test]
fn captured_payload_is_an_adapter_consumable_fixture() {
    let dir = tmp("roundtrip");
    // Cursor's beforeShellExecution native shape.
    let native = r#"{ "command": "echo AKIAIOSFODNN7EXAMPLE", "cwd": "/repo" }"#;
    let out = dir.join("cursor/beforeShellExecution.stdin.json");
    record_fixture(native, &out).unwrap();

    let text = std::fs::read_to_string(&out).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let ev: NormEvent = "pre.tool.shell".parse().unwrap();
    let payload = Harness::Cursor.decode(&value, &ev);
    let tool = payload.tool.expect("a shell tool was decoded");
    assert_eq!(
        tool.input.get("command").and_then(|c| c.as_str()),
        Some("echo AKIAIOSFODNN7EXAMPLE"),
        "the captured fixture decodes to the canonical command"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capture_rejects_empty_and_non_json_input() {
    let dir = tmp("bad");
    let out = dir.join("x.json");
    assert!(record_fixture("", &out).is_err(), "empty input is rejected");
    assert!(
        record_fixture("   ", &out).is_err(),
        "whitespace is rejected"
    );
    assert!(
        record_fixture("not json", &out).is_err(),
        "non-JSON is rejected"
    );
    assert!(
        !out.exists(),
        "nothing is written on a rejected capture: {}",
        out.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
