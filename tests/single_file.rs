//! Single-file capabilities: a document (e.g. `SKILL.md`) whose YAML frontmatter
//! *is* the manifest — no sidecar `capability.json`. Covers the frontmatter
//! reader, discovery, id/kind inference, precedence, and single-file scaffolding.

use open_harness::adapters::Harness;
use open_harness::kind::{kind_impl, Artifact, KindId};
use open_harness::manifest;
use open_harness::scaffold::{self, Lang};
use open_harness::yaml;
use serde_json::json;
use std::path::{Path, PathBuf};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("oh-sf-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(path: &Path, contents: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

// ---- frontmatter reader ---------------------------------------------------

#[test]
fn parses_scalars_lists_and_flow_maps() {
    let (fm, body) = yaml::parse_document(
        "---\nname: Hi\nn: 3\nflag: true\ntools: [read, grep]\nperm: {bash: ask, edit: allow}\n---\n\nBody here.\n",
    )
    .unwrap();
    assert_eq!(fm.get("name").unwrap(), &json!("Hi"));
    assert_eq!(fm.get("n").unwrap(), &json!(3));
    assert_eq!(fm.get("flag").unwrap(), &json!(true));
    assert_eq!(fm.get("tools").unwrap(), &json!(["read", "grep"]));
    assert_eq!(
        fm.get("perm").unwrap(),
        &json!({"bash": "ask", "edit": "allow"})
    );
    assert_eq!(body.trim(), "Body here.");
}

#[test]
fn parses_block_lists_and_nested_flow() {
    let text = "---\ntools:\n  - read\n  - grep\noverrides: {copilot: {tools: [search]}}\n---\nx\n";
    let (fm, _) = yaml::parse_document(text).unwrap();
    assert_eq!(fm.get("tools").unwrap(), &json!(["read", "grep"]));
    assert_eq!(
        fm.get("overrides").unwrap(),
        &json!({"copilot": {"tools": ["search"]}})
    );
}

#[test]
fn quoted_scalar_keeps_colon_and_hash_and_bare_markdown_is_all_body() {
    let (fm, _) = yaml::parse_document("---\ndescription: \"a: b # c\"\n---\nx").unwrap();
    assert_eq!(fm.get("description").unwrap(), &json!("a: b # c"));
    // A document with no `---` fence is entirely body.
    let (fm2, body) = yaml::parse_document("# Just markdown\n").unwrap();
    assert!(fm2.is_empty());
    assert_eq!(body, "# Just markdown\n");
}

#[test]
fn parses_a_map_of_list_in_both_pyyaml_and_indented_styles() {
    // PyYAML's default: the sequence sits at its key's indent.
    let (fm, _) =
        yaml::parse_document("---\ntriggers:\n  keywords:\n  - Aurora DSQL\n  - DSQL\n---\nx")
            .unwrap();
    assert_eq!(
        fm.get("triggers").unwrap(),
        &json!({"keywords": ["Aurora DSQL", "DSQL"]})
    );
    // The same content, sequence indented further, parses identically.
    let (fm2, _) =
        yaml::parse_document("---\ntriggers:\n  keywords:\n    - Aurora DSQL\n    - DSQL\n---\nx")
            .unwrap();
    assert_eq!(fm.get("triggers"), fm2.get("triggers"));
}

#[test]
fn parses_a_deeply_nested_block_map() {
    let (fm, _) =
        yaml::parse_document("---\noverrides:\n  claude-code:\n    path: CUSTOM/PATH.md\n---\nx")
            .unwrap();
    assert_eq!(
        fm.get("overrides").unwrap(),
        &json!({"claude-code": {"path": "CUSTOM/PATH.md"}})
    );
}

#[test]
fn parses_a_block_sequence_of_maps() {
    let (fm, _) = yaml::parse_document(
        "---\nsteps:\n- name: build\n  run: cargo build\n- name: test\n  run: cargo test\n---\nx",
    )
    .unwrap();
    assert_eq!(
        fm.get("steps").unwrap(),
        &json!([
            {"name": "build", "run": "cargo build"},
            {"name": "test", "run": "cargo test"},
        ])
    );
}

// ---- discovery ------------------------------------------------------------

#[test]
fn discovers_a_single_file_skill_and_renders_it() {
    let dir = tmp("skill");
    write(
        &dir.join("commit-style/SKILL.md"),
        "---\ndescription: Repo commit conventions.\nallowed-tools: [Read]\n---\n# Commit style\nUse conventional commits.\n",
    );
    let caps = manifest::discover(&dir).unwrap();
    assert_eq!(caps.len(), 1);
    let cap = &caps[0];
    assert_eq!(cap.manifest.id, "commit-style"); // from the directory name
    assert_eq!(cap.manifest.kind, KindId::Skill); // from the filename

    let plan = kind_impl(cap.manifest.kind).plan(cap, Harness::Claude);
    let Some(Artifact::File { path, contents }) = plan.artifacts.first() else {
        panic!("expected a file");
    };
    assert_eq!(path, ".claude/skills/commit-style/SKILL.md");
    assert!(contents.contains("allowed-tools: Read"));
    assert!(contents.contains("Use conventional commits."));
}

#[test]
fn single_file_agent_lowers_tools_to_the_permission_map() {
    let dir = tmp("agent");
    write(
        &dir.join("db/AGENT.md"),
        "---\ndescription: \"Schema work: migrations.\"\ntools: [read, write]\npermissions: {bash: ask}\nmode: subagent\n---\nYou review schemas.\n",
    );
    let cap = manifest::discover(&dir)
        .unwrap()
        .into_iter()
        .find(|c| c.manifest.id == "db")
        .unwrap();
    assert_eq!(cap.manifest.kind, KindId::Agent);
    let plan = kind_impl(KindId::Agent).plan(&cap, Harness::OpenCode);
    let Some(Artifact::File { contents, .. }) = plan.artifacts.first() else {
        panic!("expected a file");
    };
    assert!(contents.contains("mode: subagent"));
    assert!(
        contents.contains("edit: allow"),
        "write folds to edit:\n{contents}"
    );
}

#[test]
fn explicit_frontmatter_kind_and_id_win_over_conventions() {
    let dir = tmp("explicit");
    write(
        &dir.join("whatever/INSTRUCTIONS.md"),
        "---\nid: house-rules\nkind: instructions\n---\nBe consistent.\n",
    );
    let caps = manifest::discover(&dir).unwrap();
    assert_eq!(caps[0].manifest.id, "house-rules");
    assert_eq!(caps[0].manifest.kind, KindId::Instructions);
}

#[test]
fn single_file_nested_override_is_applied_not_dropped() {
    // gap 2c must compose with single-file authoring: a nested `overrides:` block
    // in frontmatter has to reach the kind, not be flattened away.
    let dir = tmp("override");
    write(
        &dir.join("nested/AGENT.md"),
        "---\ndescription: An agent.\ntools: [read]\noverrides:\n  claude-code:\n    path: CUSTOM/OVERRIDE/PATH.md\n---\nBody.\n",
    );
    let cap = manifest::discover(&dir)
        .unwrap()
        .into_iter()
        .find(|c| c.manifest.id == "nested")
        .unwrap();
    let plan = kind_impl(KindId::Agent).plan(&cap, Harness::Claude);
    let Some(Artifact::File { path, .. }) = plan.artifacts.first() else {
        panic!("expected a file");
    };
    assert_eq!(
        path, "CUSTOM/OVERRIDE/PATH.md",
        "the single-file per-harness override must be applied"
    );
}

#[test]
fn capability_json_wins_when_both_are_present() {
    let dir = tmp("both");
    write(
        &dir.join("c/capability.json"),
        r#"{"id":"c","kind":"skill","description":"json wins","skill":{"body":"J"}}"#,
    );
    write(
        &dir.join("c/SKILL.md"),
        "---\ndescription: md loses\n---\nM\n",
    );
    let caps = manifest::discover(&dir).unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].manifest.description, "json wins");
}

// ---- recursive discovery --------------------------------------------------

#[test]
fn discovers_capabilities_nested_in_category_directories() {
    let dir = tmp("nested");
    // A deep category tree (harness-style registry) — must be found, not skipped.
    write(
        &dir.join("skills/gcp/cloud-run-basics/SKILL.md"),
        "---\ndescription: Deploy to Cloud Run.\n---\n# Cloud Run\nSteps…\n",
    );
    // A capability directly under the root.
    write(
        &dir.join("commit-style/SKILL.md"),
        "---\ndescription: Commits.\n---\n# X\n",
    );
    // A nested capability.json, under its own category.
    write(
        &dir.join("agents/db/capability.json"),
        r#"{"id":"db","kind":"agent","agent":{"body":"b"}}"#,
    );
    let ids: Vec<String> = manifest::discover(&dir)
        .unwrap()
        .into_iter()
        .map(|c| c.manifest.id)
        .collect();
    assert!(
        ids.contains(&"cloud-run-basics".to_string()),
        "the deeply-nested skill must be found: {ids:?}"
    );
    assert!(ids.contains(&"commit-style".to_string()));
    assert!(ids.contains(&"db".to_string()));
    assert_eq!(ids.len(), 3);
}

#[test]
fn does_not_descend_into_a_capability_directory() {
    let dir = tmp("nodescend");
    // `outer` is a capability; a look-alike file inside it is an asset, not a
    // second capability — the walk must stop at `outer`.
    write(
        &dir.join("outer/SKILL.md"),
        "---\ndescription: outer.\n---\n# outer\n",
    );
    write(
        &dir.join("outer/examples/SKILL.md"),
        "---\ndescription: an asset.\n---\n# inner\n",
    );
    let caps = manifest::discover(&dir).unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].manifest.id, "outer");
}

#[test]
fn skips_hidden_directories() {
    let dir = tmp("hidden");
    write(
        &dir.join(".git/SKILL.md"),
        "---\ndescription: not a cap.\n---\n# x\n",
    );
    write(
        &dir.join("real/SKILL.md"),
        "---\ndescription: real.\n---\n# r\n",
    );
    let ids: Vec<String> = manifest::discover(&dir)
        .unwrap()
        .into_iter()
        .map(|c| c.manifest.id)
        .collect();
    assert_eq!(ids, vec!["real".to_string()]);
}

// ---- scaffold -------------------------------------------------------------

#[test]
fn scaffold_skill_is_single_file_and_immediately_discoverable() {
    let dir = tmp("scaffold");
    let files = scaffold::scaffold(KindId::Skill, Lang::Python, "my-skill", &dir).unwrap();
    assert!(files.iter().any(|f| f.ends_with("SKILL.md")));
    assert!(
        !files.iter().any(|f| f.ends_with("capability.json")),
        "a document kind must not scaffold a sidecar capability.json"
    );
    let caps = manifest::discover(&dir).unwrap();
    assert_eq!(caps[0].manifest.id, "my-skill");
    assert_eq!(caps[0].manifest.kind, KindId::Skill);
}
