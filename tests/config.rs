//! The YAML config layer: the block emitter, the reader, and the round-trip
//! between them. These lock down the two things the rest of the conversion
//! rests on — that emitted YAML parses back to the value it came from, and that
//! JSON stays readable so a pre-YAML checkout still loads.

use open_harness::config::{self, Format};
use open_harness::yaml;
use serde_json::{json, Value};

/// Emit a value, read it back, and assert it survived unchanged.
fn round_trip(v: Value) -> String {
    let text = yaml::to_document(&v);
    let back = yaml::parse_value(&text)
        .unwrap_or_else(|e| panic!("re-parsing emitted YAML failed: {e}\n---\n{text}"));
    assert_eq!(back, v, "round-trip changed the value\n---\n{text}");
    text
}

#[test]
fn emits_block_style_mappings() {
    let text = round_trip(json!({
        "name": "my-setup",
        "harnesses": ["claude-code", "cursor"],
    }));
    assert_eq!(
        text,
        "name: my-setup\nharnesses:\n  - claude-code\n  - cursor\n"
    );
}

#[test]
fn emits_sequences_of_mappings_with_the_first_key_on_the_dash_line() {
    let text = round_trip(json!({
        "sources": [
            { "git": { "url": "https://example.com/caps", "rev": "main" } },
            { "local": { "path": "capabilities" } },
        ]
    }));
    assert_eq!(
        text,
        "sources:\n  - git:\n      url: https://example.com/caps\n      rev: main\n  \
         - local:\n      path: capabilities\n"
    );
}

#[test]
fn key_order_survives_the_round_trip() {
    // `serde_json/preserve_order`: emitted order is authored order, not
    // alphabetical — otherwise every migrated file would be reshuffled.
    let text = round_trip(json!({ "zebra": 1, "apple": 2, "mango": 3 }));
    assert_eq!(text, "zebra: 1\napple: 2\nmango: 3\n");
}

#[test]
fn empty_collections_and_scalars_stay_inline() {
    let text = round_trip(json!({
        "read": [],
        "overrides": {},
        "timeout_ms": 5000,
        "required": true,
        "note": null,
    }));
    assert_eq!(
        text,
        "read: []\noverrides: {}\ntimeout_ms: 5000\nrequired: true\nnote: null\n"
    );
}

#[test]
fn quotes_scalars_that_would_otherwise_change_type() {
    // The YAML 1.1 bool traps and number/null lookalikes must stay strings, or a
    // version like "1.0" or a Codex matcher like "on" silently changes meaning.
    let text = round_trip(json!({
        "a": "yes", "b": "no", "c": "on", "d": "off",
        "e": "true", "f": "null", "g": "1.0", "h": "007",
    }));
    for token in [
        "\"yes\"", "\"no\"", "\"on\"", "\"off\"", "\"true\"", "\"null\"", "\"1.0\"",
    ] {
        assert!(
            text.contains(token),
            "expected {token} to be quoted in:\n{text}"
        );
    }
}

#[test]
fn quotes_keys_that_need_it_too() {
    // Keys go through the same quoting rules as values — a mapping key like
    // `on` or `1.0` would otherwise come back a bool or a number, and one
    // containing `: ` would reparse as a nested mapping.
    let text = round_trip(json!({
        "on": 1,
        "1.0": 2,
        "key: with colon": 3,
        "": 4,
        "plain-key": 5,
    }));
    assert!(
        text.contains("\"on\": 1"),
        "bool-lookalike key quoted:\n{text}"
    );
    assert!(
        text.contains("\"1.0\": 2"),
        "number-lookalike key quoted:\n{text}"
    );
    assert!(
        text.contains("\"key: with colon\": 3"),
        "a key containing `: ` is quoted:\n{text}"
    );
    assert!(text.contains("\"\": 4"), "the empty key is quoted:\n{text}");
    assert!(
        text.contains("plain-key: 5"),
        "and an ordinary key is left bare:\n{text}"
    );
}

#[test]
fn quotes_scalars_with_yaml_indicators() {
    round_trip(json!({
        "colon": "key: value",
        "hash": "trailing # comment",
        "dash": "-leading",
        "brace": "{flow}",
        "newline": "two\nlines",
        "empty": "",
        "padded": "  spaced  ",
    }));
}

#[test]
fn nested_structures_round_trip() {
    round_trip(json!({
        "version": 1,
        "profile": "my-setup",
        "sources": [{
            "kind": "git",
            "origin": "https://example.com/caps",
            "capabilities": [
                { "id": "guard", "version": "1.2.0", "dependencies": ["shared"] },
                { "id": "shared", "version": "0.1.0", "dependencies": [] },
            ],
        }],
    }));
}

#[test]
fn deeply_nested_sequences_round_trip() {
    round_trip(json!({ "matrix": [[1, 2], [3, 4]], "empty_rows": [[], []] }));
}

// ---- the reader ------------------------------------------------------------

#[test]
fn a_blank_document_is_an_empty_mapping() {
    // A config file that exists but is empty means "all defaults", not an error.
    assert_eq!(yaml::parse_value("").unwrap(), json!({}));
    assert_eq!(
        yaml::parse_value("\n# just a comment\n").unwrap(),
        json!({})
    );
}

#[test]
fn a_multi_document_stream_is_refused() {
    let err = yaml::parse_value("a: 1\n---\nb: 2\n").unwrap_err();
    assert!(err.contains("single YAML document"), "got: {err}");
}

#[test]
fn json_is_still_readable() {
    let v: Value = config::from_str(r#"{"name": "x", "n": 1}"#, Format::Json).unwrap();
    assert_eq!(v, json!({ "name": "x", "n": 1 }));
}

#[test]
fn an_extensionless_file_accepts_either_format() {
    // `open-harness.lock` and `capability.sig` carry no extension, so both the
    // migrated YAML and a leftover JSON file must load.
    let yaml_v: Value = config::from_str("name: x\nn: 1\n", Format::Either).unwrap();
    let json_v: Value = config::from_str(r#"{"name": "x", "n": 1}"#, Format::Either).unwrap();
    assert_eq!(yaml_v, json_v);
}

#[test]
fn a_yaml_error_names_the_problem_not_the_json_fallback() {
    // Both parses fail; the YAML error is what surfaces, since YAML is the
    // format we tell people to write.
    let err = config::from_str::<Value>("a: [1, 2\nb: }", Format::Either).unwrap_err();
    assert!(err.contains("invalid YAML"), "got: {err}");
}

// ---- migration + back-compat ----------------------------------------------

fn tmp(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-config-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir temp");
    p
}

#[test]
fn a_legacy_json_manifest_still_loads_and_discovers() {
    // The whole point of dual-read: a checkout from before the switch works.
    let dir = tmp("legacy");
    std::fs::create_dir_all(dir.join("guard")).unwrap();
    std::fs::write(
        dir.join("guard/capability.json"),
        r#"{ "id": "guard", "kind": "skill", "skill": { "body": "hi" } }"#,
    )
    .unwrap();
    let caps = open_harness::manifest::discover(&dir).unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].manifest.id, "guard");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn yaml_wins_over_a_leftover_json_manifest() {
    // Precedence matters during a partial migration: the migrated file is the
    // real one, and the stale JSON beside it must not shadow it.
    let dir = tmp("precedence");
    std::fs::create_dir_all(dir.join("guard")).unwrap();
    std::fs::write(
        dir.join("guard/capability.json"),
        r#"{ "id": "stale", "kind": "skill", "skill": { "body": "old" } }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("guard/capability.yaml"),
        "id: fresh\nkind: skill\nskill:\n  body: new\n",
    )
    .unwrap();
    let caps = open_harness::manifest::discover(&dir).unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].manifest.id, "fresh");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_rewrites_json_as_yaml_and_removes_the_original() {
    let dir = tmp("migrate");
    let json = dir.join("capability.json");
    std::fs::write(&json, r#"{"id": "g", "kind": "hook", "events": []}"#).unwrap();

    let written = config::migrate_file(&json).unwrap().expect("migrated");
    assert_eq!(written, dir.join("capability.yaml"));
    assert!(
        !json.exists(),
        "the JSON original is removed, not left behind"
    );
    assert_eq!(
        std::fs::read_to_string(&written).unwrap(),
        "id: g\nkind: hook\nevents: []\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrating_an_already_yaml_file_is_a_no_op() {
    // Re-running `oh migrate` must not churn files it already converted.
    let dir = tmp("migrate-idem");
    let path = dir.join("open-harness.lock");
    std::fs::write(&path, "version: 1\nprofile: p\n").unwrap();
    assert!(config::migrate_file(&path).unwrap().is_none());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "version: 1\nprofile: p\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_extensionless_lockfile_migrates_in_place() {
    // `open-harness.lock` keeps its name; only the contents change format.
    let dir = tmp("migrate-lock");
    let path = dir.join("open-harness.lock");
    std::fs::write(&path, r#"{"version": 1, "profile": "p", "sources": []}"#).unwrap();
    let written = config::migrate_file(&path).unwrap().expect("migrated");
    assert_eq!(written, path);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "version: 1\nprofile: p\nsources: []\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_is_chosen_by_extension() {
    use std::path::Path;
    assert_eq!(Format::of(Path::new("a/b.yaml")), Format::Yaml);
    assert_eq!(Format::of(Path::new("a/b.yml")), Format::Yaml);
    assert_eq!(Format::of(Path::new("a/b.json")), Format::Json);
    assert_eq!(Format::of(Path::new("open-harness.lock")), Format::Either);
    assert_eq!(Format::of(Path::new("capability.sig")), Format::Either);
}
