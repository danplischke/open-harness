//! `select`: taking part of a source rather than all of it.
//!
//! A source is usually somebody else's repository. Before this, the only lever
//! was `subdir` — an accident of how they laid the repo out, not a choice about
//! what you need. A plugin shipping nineteen skills was all nineteen or none.

use open_harness::profile::{self, Profile};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-select-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir temp");
    p
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn cap_yaml(id: &str, kind: &str, extra: serde_json::Value) -> String {
    let mut m = json!({ "id": id, "name": id, "description": "x", "kind": kind });
    if let (Some(mo), Some(eo)) = (m.as_object_mut(), extra.as_object()) {
        for (k, v) in eo {
            mo.insert(k.clone(), v.clone());
        }
    }
    open_harness::config::to_yaml(&m).expect("render manifest")
}

/// A source shaped like a real plugin: several skills, a hook, a rule.
fn plugin_source(tag: &str) -> PathBuf {
    let wd = tmp(tag);
    for id in [
        "mem-search",
        "timeline-report",
        "smart-explore",
        "wowerpoint",
    ] {
        write(
            &wd.join(format!("caps/{id}/capability.yaml")),
            &cap_yaml(id, "skill", json!({ "skill": { "body": id } })),
        );
    }
    write(
        &wd.join("caps/session-hook/capability.yaml"),
        &cap_yaml(
            "session-hook",
            "hook",
            json!({
                "run": { "command": "python3", "args": ["h.py"] },
                "events": [{ "phase": "pre", "subject": "tool", "tool_class": "any" }],
            }),
        ),
    );
    write(
        &wd.join("caps/house-style/capability.yaml"),
        &cap_yaml("house-style", "rule", json!({ "rule": { "body": "r" } })),
    );
    wd
}

fn resolve_with(wd: &Path, select: serde_json::Value) -> profile::Resolved {
    let mut local = json!({ "path": "caps" });
    if let (Some(o), Some(s)) = (local.as_object_mut(), select.as_object()) {
        if !s.is_empty() {
            o.insert("select".into(), select.clone());
        }
    }
    let p: Profile = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "local": local }],
        })
        .to_string(),
    )
    .expect("valid profile");
    profile::resolve(&p, wd, None).expect("resolves")
}

fn ids(r: &profile::Resolved) -> Vec<String> {
    let mut v: Vec<String> = r
        .capabilities
        .iter()
        .map(|c| c.manifest.id.clone())
        .collect();
    v.sort();
    v
}

// ---- the default is unchanged ----------------------------------------------

#[test]
fn no_select_block_takes_everything() {
    let wd = plugin_source("all");
    let r = resolve_with(&wd, json!({}));
    assert_eq!(ids(&r).len(), 6, "an absent select changes nothing");
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn an_empty_include_list_still_takes_everything() {
    let wd = plugin_source("empty");
    let r = resolve_with(&wd, json!({ "exclude": ["wowerpoint"] }));
    assert!(
        !ids(&r).contains(&"wowerpoint".to_string()) && ids(&r).len() == 5,
        "exclude works without an include: {:?}",
        ids(&r)
    );
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- include / exclude / kinds ---------------------------------------------

#[test]
fn include_takes_only_what_was_named() {
    let wd = plugin_source("include");
    let r = resolve_with(&wd, json!({ "include": ["mem-search", "timeline-report"] }));
    assert_eq!(ids(&r), vec!["mem-search", "timeline-report"]);
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn include_accepts_wildcards() {
    let wd = plugin_source("wildcard");
    let r = resolve_with(&wd, json!({ "include": ["*-report", "smart-*"] }));
    assert_eq!(ids(&r), vec!["smart-explore", "timeline-report"]);
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn exclude_beats_include() {
    let wd = plugin_source("exclude-wins");
    let r = resolve_with(
        &wd,
        json!({ "include": ["*"], "exclude": ["wowerpoint", "*-hook"] }),
    );
    assert_eq!(
        ids(&r),
        vec![
            "house-style",
            "mem-search",
            "smart-explore",
            "timeline-report"
        ]
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn kinds_takes_a_plugins_skills_without_its_hooks() {
    // The motivating case: a plugin's skills are portable, its hooks are
    // harness-specific. Taking one without the other should not require
    // enumerating every skill by name.
    let wd = plugin_source("kinds");
    let r = resolve_with(&wd, json!({ "kinds": ["skill"] }));
    assert_eq!(
        ids(&r),
        vec![
            "mem-search",
            "smart-explore",
            "timeline-report",
            "wowerpoint"
        ]
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn kinds_and_include_are_both_applied() {
    let wd = plugin_source("kinds-include");
    let r = resolve_with(&wd, json!({ "kinds": ["skill"], "include": ["*e*"] }));
    assert!(
        !ids(&r).contains(&"house-style".to_string()),
        "the rule is excluded by kind even though it matches the pattern: {:?}",
        ids(&r)
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn what_was_dropped_is_reported() {
    let wd = plugin_source("reported");
    let r = resolve_with(&wd, json!({ "include": ["mem-search"] }));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("select kept 1 of 6") && w.contains("wowerpoint")),
        "narrowing is visible, not silent: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- typo protection -------------------------------------------------------

#[test]
fn an_include_pattern_matching_nothing_is_an_error_listing_what_exists() {
    // A typo silently yielding zero capabilities is precisely the failure this
    // feature would otherwise introduce.
    let wd = plugin_source("typo");
    let p: Profile = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "local": { "path": "caps", "select": { "include": ["mem-serach"] } } }],
        })
        .to_string(),
    )
    .unwrap();

    let err = match profile::resolve(&p, &wd, None) {
        Err(e) => e,
        Ok(_) => panic!("a typo must not resolve to nothing"),
    };
    assert!(err.contains("mem-serach"), "names the bad pattern: {err}");
    assert!(
        err.contains("mem-search"),
        "and lists what is actually there: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn a_pattern_whose_match_is_then_excluded_is_not_an_error() {
    // Deliberate combinations must stay legal; only a pattern that matches
    // nothing in the source at all is a typo.
    let wd = plugin_source("not-typo");
    let r = resolve_with(
        &wd,
        json!({ "include": ["mem-search", "wowerpoint"], "exclude": ["wowerpoint"] }),
    );
    assert_eq!(ids(&r), vec!["mem-search"]);
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn a_misspelled_select_key_is_a_loud_error() {
    let err = Profile::from_text(
        &json!({
            "name": "p", "sources": [{ "local": { "path": "caps", "select": { "includes": ["x"] } } }],
        })
        .to_string(),
    )
    .unwrap_err();
    assert!(err.contains("includes"), "names the bad key: {err}");
}

// ---- dependencies ----------------------------------------------------------

/// A source where one selected capability requires an unselected sibling.
fn source_with_dependency(tag: &str) -> PathBuf {
    let wd = tmp(tag);
    write(
        &wd.join("caps/app/capability.yaml"),
        &cap_yaml(
            "app",
            "skill",
            json!({ "dependencies": ["shared"], "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/shared/capability.yaml"),
        &cap_yaml("shared", "skill", json!({ "skill": { "body": "s" } })),
    );
    wd
}

#[test]
fn excluding_a_dependency_is_reported_not_silently_repaired() {
    // The default: narrowing is your decision, and open-harness tells you what
    // it cost rather than quietly widening it back.
    let wd = source_with_dependency("dep-unmet");
    let r = resolve_with(&wd, json!({ "include": ["app"] }));
    assert_eq!(ids(&r), vec!["app"]);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("requires 'shared'") && w.contains("no source provides")),
        "the cost of the exclusion is reported: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn with_dependencies_pulls_the_required_sibling_back_in() {
    let wd = source_with_dependency("dep-pull");
    let r = resolve_with(
        &wd,
        json!({ "include": ["app"], "with_dependencies": true }),
    );
    assert_eq!(ids(&r), vec!["app", "shared"]);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("re-admitted 'shared'")),
        "and says it widened the selection: {:?}",
        r.warnings
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn with_dependencies_follows_a_chain_to_a_fixpoint() {
    // A pulled-in dependency may itself require another.
    let wd = tmp("dep-chain");
    write(
        &wd.join("caps/a/capability.yaml"),
        &cap_yaml(
            "a",
            "skill",
            json!({ "dependencies": ["b"], "skill": { "body": "a" } }),
        ),
    );
    write(
        &wd.join("caps/b/capability.yaml"),
        &cap_yaml(
            "b",
            "skill",
            json!({ "dependencies": ["c"], "skill": { "body": "b" } }),
        ),
    );
    write(
        &wd.join("caps/c/capability.yaml"),
        &cap_yaml("c", "skill", json!({ "skill": { "body": "c" } })),
    );
    let r = resolve_with(&wd, json!({ "include": ["a"], "with_dependencies": true }));
    assert_eq!(ids(&r), vec!["a", "b", "c"]);
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- the lockfile ----------------------------------------------------------

#[test]
fn the_lock_records_only_what_was_selected() {
    // So widening the selection later is a visible lock change that `--locked`
    // catches, rather than a silent one.
    let wd = plugin_source("locked");
    let r = resolve_with(&wd, json!({ "include": ["mem-search"] }));
    let locked: Vec<&str> = r.lock.sources[0]
        .capabilities
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(locked, vec!["mem-search"]);
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn selection_survives_a_profile_round_trip() {
    let text = "name: p\nharnesses: [claude-code]\nsources:\n  - local:\n      path: caps\n      \
                select:\n        include: [mem-search]\n        kinds: [skill]\n";
    let p = Profile::from_text(text).unwrap();
    let back = Profile::from_text(&open_harness::config::to_yaml(&p).unwrap()).unwrap();
    let (a, b) = (&p.sources[0], &back.sources[0]);
    match (a, b) {
        (profile::Source::Local(x), profile::Source::Local(y)) => assert_eq!(x.select, y.select),
        _ => panic!("expected local sources"),
    }
}

#[test]
fn a_long_dropped_list_is_summarized_rather_than_dumped() {
    // Selecting 2 of 19 should not produce a warning longer than the output it
    // explains.
    let wd = tmp("many");
    for i in 0..20 {
        let id = format!("cap{i:02}");
        write(
            &wd.join(format!("caps/{id}/capability.yaml")),
            &cap_yaml(&id, "skill", json!({ "skill": { "body": "b" } })),
        );
    }
    let r = resolve_with(&wd, json!({ "include": ["cap00"] }));
    let warning = r
        .warnings
        .iter()
        .find(|w| w.contains("select kept"))
        .expect("reports the drop");
    assert!(
        warning.contains("and 13 more"),
        "the list is capped: {warning}"
    );
    assert!(
        warning.contains("select kept 1 of 20"),
        "but the true counts are still stated: {warning}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

// ---- glob semantics --------------------------------------------------------

#[test]
fn patterns_support_character_classes() {
    // What the glob crate buys over a bare `*`/`?` matcher.
    let wd = plugin_source("classes");
    let r = resolve_with(&wd, json!({ "include": ["[ms]*"] }));
    assert_eq!(ids(&r), vec!["mem-search", "session-hook", "smart-explore"]);
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn a_negated_class_excludes() {
    let wd = plugin_source("negated");
    let r = resolve_with(&wd, json!({ "include": ["[!msth]*"] }));
    assert_eq!(ids(&r), vec!["wowerpoint"]);
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn a_star_spans_the_namespace_separator() {
    // `acme/*` should select a whole namespace, so `*` must not stop at `/`.
    let wd = tmp("ns-span");
    write(
        &wd.join("caps/one/capability.yaml"),
        &cap_yaml(
            "one",
            "skill",
            json!({ "namespace": "acme", "skill": { "body": "1" } }),
        ),
    );
    write(
        &wd.join("caps/two/capability.yaml"),
        &cap_yaml(
            "two",
            "skill",
            json!({ "namespace": "other", "skill": { "body": "2" } }),
        ),
    );
    let r = resolve_with(&wd, json!({ "include": ["acme/*"] }));
    assert_eq!(ids(&r), vec!["one"]);
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn a_malformed_pattern_is_an_error_not_a_literal() {
    // An unclosed class used to be matched literally, silently selecting
    // nothing that looks like it.
    let wd = plugin_source("bad-glob");
    let p: Profile = Profile::from_text(
        &json!({
            "name": "p", "harnesses": ["claude-code"],
            "sources": [{ "local": { "path": "caps", "select": { "include": ["mem-[search"] } } }],
        })
        .to_string(),
    )
    .unwrap();
    let err = match profile::resolve(&p, &wd, None) {
        Err(e) => e,
        Ok(_) => panic!("a malformed glob must not resolve"),
    };
    assert!(
        err.contains("mem-[search") && err.contains("not a valid glob"),
        "names the pattern and the problem: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}
