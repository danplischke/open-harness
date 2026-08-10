//! Tests for the capability kinds and shared vocabulary added to close the
//! `harness`-adoption gaps: the canonical tool table (gap 6), the agent kind
//! (gap 1), the enriched skill kind (gap 2), the instructions kind (gap 4), and
//! the Antigravity harness (gap 3).

use open_harness::adapters::Harness;
use open_harness::kind::{kind_impl, Artifact, Installability, KindId};
use open_harness::manifest::LoadedCapability;
use open_harness::tools;
use serde_json::{json, Value};

fn cap(manifest: Value) -> LoadedCapability {
    LoadedCapability {
        manifest: serde_json::from_value(manifest).expect("valid manifest"),
        dir: std::path::PathBuf::from("."),
    }
}

/// The single File artifact a generative kind plans to, or panic.
fn file(cap: &LoadedCapability, harness: Harness) -> (String, String) {
    let plan = kind_impl(cap.manifest.kind).plan(cap, harness);
    match plan.artifacts.into_iter().next() {
        Some(Artifact::File { path, contents }) => (path, contents),
        other => panic!("{}: expected a File artifact, got {other:?}", harness.id()),
    }
}

// ---- gap 6: canonical tool vocabulary -------------------------------------

#[test]
fn tool_vocabulary_maps_each_harness() {
    assert_eq!(tools::native_tool("read", Harness::Claude), "Read");
    assert_eq!(tools::native_tool("read", Harness::OpenCode), "read");
    assert_eq!(tools::native_tool("read", Harness::Copilot), "readFile");
    assert_eq!(tools::native_tool("read", Harness::Gemini), "read_file");
    assert_eq!(
        tools::native_tool("bash", Harness::Copilot),
        "runInTerminal"
    );
}

#[test]
fn tool_vocabulary_encodes_the_documented_gotchas() {
    // Claude renamed Task -> Agent (alias kept).
    assert_eq!(tools::native_tool("task", Harness::Claude), "Agent");
    // Copilot folds edit + write onto editFiles, grep + glob onto search.
    assert_eq!(tools::native_tool("edit", Harness::Copilot), "editFiles");
    assert_eq!(tools::native_tool("write", Harness::Copilot), "editFiles");
    assert_eq!(tools::native_tool("grep", Harness::Copilot), "search");
    assert_eq!(tools::native_tool("glob", Harness::Copilot), "search");
}

#[test]
fn tool_vocabulary_normalizes_spellings_and_passes_unknowns_through() {
    // web-fetch / web_fetch / webfetch all resolve.
    for spelling in ["web-fetch", "web_fetch", "webfetch"] {
        assert_eq!(tools::native_tool(spelling, Harness::Copilot), "fetch");
    }
    // Copilot has no web search — unmapped names pass through verbatim.
    assert_eq!(
        tools::native_tool("web-search", Harness::Copilot),
        "web-search"
    );
    // Unknown name: title-cased for Claude (PascalCase convention), verbatim else.
    assert_eq!(tools::native_tool("myTool", Harness::Claude), "MyTool");
    assert_eq!(tools::native_tool("myTool", Harness::OpenCode), "myTool");
}

#[test]
fn tool_list_dedups_after_mapping() {
    // grep + glob both -> search on Copilot; the list collapses.
    let mapped = tools::native_tools(
        &["read".into(), "grep".into(), "glob".into()],
        Harness::Copilot,
    );
    assert_eq!(mapped, vec!["readFile".to_string(), "search".to_string()]);
}

#[test]
fn tool_string_splits_respecting_parens_and_separators() {
    // A parenthesized spec stays whole; comma OR whitespace separates.
    assert_eq!(
        tools::split_tool_string("Read Grep Bash(git diff:*)"),
        vec!["Read", "Grep", "Bash(git diff:*)"]
    );
    assert_eq!(tools::split_tool_string("Read, Grep"), vec!["Read", "Grep"]);
    assert_eq!(
        tools::split_tool_string("Bash(git add:*), Bash(git status:*)"),
        vec!["Bash(git add:*)", "Bash(git status:*)"]
    );
    assert!(tools::split_tool_string("   ").is_empty());
}

#[test]
fn skill_accepts_scalar_allowed_tools_and_keeps_body() {
    // The native Agent-Skills spelling is a string, not a list. It must parse —
    // and crucially must NOT void the body (the old failure emitted the
    // description as the body).
    let m = json!({
        "id": "s", "name": "S", "description": "a description", "kind": "skill",
        "skill": { "body": "THE REAL BODY", "allowed_tools": "Read Grep Bash(git diff:*)" }
    });
    let plan = kind_impl(KindId::Skill).plan(&cap(m), Harness::Claude);
    let Some(Artifact::File { contents, .. }) = plan.artifacts.first() else {
        panic!("expected a file");
    };
    // A space-separated input re-emits space-separated (idempotent), and the
    // parenthesized spec stays whole.
    assert!(
        contents.contains("allowed-tools: Read Grep Bash(git diff:*)"),
        "string allowed-tools must parse + round-trip:\n{contents}"
    );
    // If the body were voided, it would be the description ("a description");
    // the real body appearing proves it survived.
    assert!(
        contents.contains("THE REAL BODY"),
        "the real body must survive, not be replaced by the description:\n{contents}"
    );
}

#[test]
fn malformed_kind_config_is_reported_not_silently_voided() {
    // A type mismatch in a config block must surface as a loud Unsupported with
    // the reason — never a silent default that ships a plausible-but-wrong file.
    let m = json!({
        "id": "r", "name": "R", "description": "d", "kind": "rule",
        "rule": { "activation": "glob", "globs": "not-a-list" }
    });
    let plan = kind_impl(KindId::Rule).plan(&cap(m), Harness::Cursor);
    match plan.installability {
        Installability::Unsupported(reason) => {
            assert!(reason.contains("invalid `rule` config"), "reason: {reason}");
        }
        other => panic!("expected Unsupported for a malformed config, got {other:?}"),
    }
}

// ---- gap 1: agent kind ----------------------------------------------------

fn agent_manifest() -> Value {
    json!({
        "id": "db-engineer",
        "name": "Database Engineer",
        "description": "A subagent.",
        "kind": "agent",
        "agent": {
            "description": "Use for schema changes: migrations and indexes.",
            "model": "claude-sonnet-5",
            "tools": ["read", "grep", "bash", "write"],
            "permissions": { "bash": "ask" },
            "mode": "subagent",
            "temperature": 0.2,
            "skills": ["postgres-review"],
            "body": "You are a database engineer."
        }
    })
}

#[test]
fn agent_renders_claude_with_list_tools_and_quoted_description() {
    let (path, contents) = file(&cap(agent_manifest()), Harness::Claude);
    assert_eq!(path, ".claude/agents/db-engineer.md");
    assert!(contents.contains("name: Database Engineer"));
    // The description has a colon -> must be YAML-quoted (gap 2d), not raw.
    assert!(
        contents.contains(r#"description: "Use for schema changes: migrations and indexes.""#),
        "description must be quoted:\n{contents}"
    );
    assert!(contents.contains("model: claude-sonnet-5"));
    assert!(contents.contains("tools: [Read, Grep, Bash, Write]"));
    assert!(contents.contains("skills: [postgres-review]"));
    assert!(contents
        .trim_end()
        .ends_with("You are a database engineer."));
}

#[test]
fn agent_opencode_lowers_tools_to_permission_map_with_write_folded() {
    let (path, contents) = file(&cap(agent_manifest()), Harness::OpenCode);
    assert_eq!(path, ".opencode/agents/db-engineer.md");
    // tools: is deprecated on OpenCode — it must NOT appear.
    assert!(
        !contents.contains("tools:"),
        "opencode must not emit tools:\n{contents}"
    );
    assert!(contents.contains("mode: subagent"));
    assert!(contents.contains("temperature: 0.2"));
    // write folds into edit; the explicit bash=ask wins over the tools-list allow.
    assert!(contents.contains("permission: {"));
    assert!(
        contents.contains("bash: ask"),
        "explicit verdict wins:\n{contents}"
    );
    assert!(
        contents.contains("edit: allow"),
        "write folds to edit:\n{contents}"
    );
    assert!(
        !contents.contains("write: "),
        "no standalone write key:\n{contents}"
    );
}

#[test]
fn agent_copilot_path_and_list() {
    let (path, contents) = file(&cap(agent_manifest()), Harness::Copilot);
    assert_eq!(path, ".github/agents/db-engineer.agent.md");
    // read->readFile, grep->search, bash->runInTerminal, write->editFiles.
    assert!(contents.contains("tools: [readFile, search, runInTerminal, editFiles]"));
}

#[test]
fn agent_passes_unknown_frontmatter_through() {
    // Like the skill kind, an agent carries forward-compat / portable frontmatter
    // keys it doesn't model, so an AGENT.md doesn't lose keys a SKILL.md keeps.
    let m = json!({
        "id": "a", "name": "A", "description": "d", "kind": "agent",
        "agent": { "prerequisite-skills": ["x"], "custom-key": "v", "body": "b" }
    });
    let plan = kind_impl(KindId::Agent).plan(&cap(m), Harness::Claude);
    let Some(Artifact::File { contents, .. }) = plan.artifacts.first() else {
        panic!("expected a file");
    };
    assert!(contents.contains("prerequisite-skills: [x]"), "{contents}");
    assert!(contents.contains("custom-key: v"), "{contents}");
}

#[test]
fn sandbox_permissions_reject_a_wrong_shaped_value() {
    // A tool→verdict map at the manifest's top-level `permissions` (the sandbox
    // key) is a loud error, not silently absorbed into an all-empty manifest.
    let r: Result<open_harness::manifest::Manifest, _> =
        serde_json::from_str(r#"{"id":"h","kind":"hook","permissions":{"bash":"ask"}}"#);
    assert!(
        r.is_err(),
        "a tool→verdict map at the sandbox `permissions` must be rejected"
    );
}

#[test]
fn agent_unsupported_where_no_file_defined_subagents() {
    let plan = kind_impl(KindId::Agent).plan(&cap(agent_manifest()), Harness::Cursor);
    assert!(matches!(
        plan.installability,
        Installability::Unsupported(_)
    ));
}

#[test]
fn agent_model_tier_is_carried_but_not_resolved() {
    let m = json!({
        "id": "a", "name": "A", "description": "d", "kind": "agent",
        "agent": { "description": "d", "model_tier": "l", "body": "b" }
    });
    let plan = kind_impl(KindId::Agent).plan(&cap(m), Harness::Claude);
    let Some(Artifact::File { contents, .. }) = plan.artifacts.first() else {
        panic!("expected file");
    };
    // No model line emitted (a tier is not a concrete model)...
    assert!(
        !contents.contains("model:"),
        "tier must not emit a model line:\n{contents}"
    );
    // ...and a loud note explains why.
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("model_tier") && n.contains("not resolved")),
        "expected an unresolved-tier note, got {:?}",
        plan.notes
    );
}

#[test]
fn agent_antigravity_installs_a_workflow_shim_degraded() {
    let (path, _) = file(&cap(agent_manifest()), Harness::Antigravity);
    assert_eq!(path, ".agents/workflows/subagent-db-engineer.md");
    let plan = kind_impl(KindId::Agent).plan(&cap(agent_manifest()), Harness::Antigravity);
    assert!(matches!(plan.installability, Installability::Degraded(_)));
}

#[test]
fn agent_per_harness_override_replaces_tools_and_merges_frontmatter() {
    let mut m = agent_manifest();
    m["overrides"] = json!({
        "copilot": {
            "path": ".github/agents/custom.agent.md",
            "tools": ["readFile", "search"],
            "frontmatter": { "user-invocable": true, "model": "GPT-5" }
        }
    });
    let (path, contents) = file(&cap(m), Harness::Copilot);
    assert_eq!(path, ".github/agents/custom.agent.md");
    assert!(contents.contains("tools: [readFile, search]"));
    assert!(contents.contains("user-invocable: true"));
    // An override key replaces the base value (model) in place.
    assert!(contents.contains("model: GPT-5"));
    assert!(!contents.contains("claude-sonnet-5"));
}

// ---- gap 4: instructions kind ---------------------------------------------

fn instructions_manifest() -> Value {
    json!({
        "id": "house", "name": "House", "description": "fallback",
        "kind": "instructions",
        "instructions": { "body": "# Rules\n\nBe consistent.\n" }
    })
}

#[test]
fn instructions_targets_the_right_file_per_harness() {
    let expected = [
        (Harness::Claude, "CLAUDE.md"),
        (Harness::Codex, "AGENTS.md"),
        (Harness::OpenCode, "AGENTS.md"),
        (Harness::Cursor, "AGENTS.md"),
        (Harness::Copilot, ".github/copilot-instructions.md"),
        (Harness::Gemini, "GEMINI.md"),
        (Harness::Antigravity, "GEMINI.md"),
    ];
    for (h, path) in expected {
        let (got, contents) = file(&cap(instructions_manifest()), h);
        assert_eq!(got, path, "{}", h.id());
        assert!(contents.contains("Be consistent."));
    }
}

#[test]
fn instructions_unsupported_on_pi() {
    let plan = kind_impl(KindId::Instructions).plan(&cap(instructions_manifest()), Harness::Pi);
    assert!(matches!(
        plan.installability,
        Installability::Unsupported(_)
    ));
}

#[test]
fn instructions_falls_back_to_description() {
    let m = json!({
        "id": "x", "name": "X", "description": "Only a description here.",
        "kind": "instructions"
    });
    let (_, contents) = file(&cap(m), Harness::Claude);
    assert!(contents.contains("Only a description here."));
}

// ---- gap 2: skill enrichment ----------------------------------------------

fn skill_manifest() -> Value {
    json!({
        "id": "pg-review", "name": "Postgres Review",
        "description": "Review changes: schema & queries.",
        "kind": "skill",
        "skill": {
            "body": "# Review\n\nCheck the migration.\n",
            "allowed_tools": ["read", "grep"],
            "license": "MIT", "triggers": ["**/*.sql"]
        }
    })
}

#[test]
fn skill_supports_the_added_harnesses() {
    let expected = [
        (Harness::Copilot, ".github/skills/pg-review/SKILL.md"),
        (Harness::OpenCode, ".opencode/skills/pg-review/SKILL.md"),
        (Harness::Antigravity, ".agents/skills/pg-review/SKILL.md"),
    ];
    for (h, path) in expected {
        let (got, _) = file(&cap(skill_manifest()), h);
        assert_eq!(got, path, "{}", h.id());
    }
}

#[test]
fn skill_quotes_a_colon_description_and_passes_frontmatter_through() {
    let (_, contents) = file(&cap(skill_manifest()), Harness::Claude);
    assert!(
        contents.contains(r#"description: "Review changes: schema & queries.""#),
        "colon description must be quoted:\n{contents}"
    );
    // allowed-tools mapped to native names.
    assert!(contents.contains("allowed-tools: Read, Grep"));
    // portable passthrough survives.
    assert!(contents.contains("license: MIT"));
    // a glob is quoted (a bare *.sql would be a YAML alias).
    assert!(contents.contains(r#"triggers: ["**/*.sql"]"#), "{contents}");
}

#[test]
fn skill_allowed_tools_are_harness_native() {
    let (_, oc) = file(&cap(skill_manifest()), Harness::OpenCode);
    assert!(oc.contains("allowed-tools: read, grep"));
}

#[test]
fn skill_roundtrips_a_quoted_colon_description() {
    let (_, contents) = file(&cap(skill_manifest()), Harness::Claude);
    let imported = open_harness::kinds::skill::import_skill_md(&contents).unwrap();
    assert_eq!(imported.name, "Postgres Review");
    assert_eq!(imported.description, "Review changes: schema & queries.");
    assert_eq!(imported.allowed_tools, vec!["Read", "Grep"]);
}

// ---- gap 3: Antigravity same-path collision with Codex --------------------

#[test]
fn skill_antigravity_and_codex_converge_on_one_shared_file() {
    let c = cap(skill_manifest());
    let plan = open_harness::sync::plan_sync(
        std::slice::from_ref(&c),
        &[Harness::Codex, Harness::Antigravity],
    );
    let shared: Vec<_> = plan
        .files
        .iter()
        .filter(|f| f.path == ".agents/skills/pg-review/SKILL.md")
        .collect();
    assert_eq!(shared.len(), 1, "the shared path must converge to one file");
    // Both harnesses are recorded as contributors — nothing silently dropped.
    assert!(shared[0].harnesses.contains(&"codex".to_string()));
    assert!(shared[0].harnesses.contains(&"antigravity".to_string()));
    assert!(
        shared[0].conflicts.is_empty(),
        "identical content is not a conflict"
    );
}

// ---- per-harness override blocks ------------------------------------------
//
// `HarnessOverride` reads three structural keys — `path`, `tools`,
// `frontmatter`. Everything else in the block used to be dropped without a
// word, while `oh check` still called the capability "installable — clean".
// That is the failure mode this project has now fixed three times in different
// clothes (`unwrap_or_default` voiding a config block, `Permissions` absorbing
// a wrong-shaped value): the run continues, having quietly done less than was
// asked. Authors write override keys flat, so flat is what the reader now
// accepts.

fn override_plan(mut m: Value, block: Value, h: Harness) -> open_harness::kind::KindPlan {
    m["overrides"] = block;
    let c = cap(m);
    kind_impl(c.manifest.kind).plan(&c, h)
}

/// The reported bug: an OpenCode agent whose `mode` and `permission` never
/// reached the emitted file.
#[test]
fn a_flat_override_block_is_applied_not_dropped() {
    let (_, contents) = file(
        &cap({
            let mut m = agent_manifest();
            m["overrides"] = json!({
                "opencode": { "mode": "primary", "permission": { "edit": "allow" } }
            });
            m
        }),
        Harness::OpenCode,
    );
    assert!(
        contents.contains("mode: primary"),
        "a flat override key must reach the file:\n{contents}"
    );
    assert!(contents.contains("permission:"), "{contents}");
    assert!(contents.contains("edit: allow"), "{contents}");
}

#[test]
fn a_flat_override_works_on_skills_too() {
    let m = json!({
        "id": "s", "name": "S", "description": "d", "kind": "skill",
        "skill": { "body": "b" },
        "overrides": { "claude-code": { "user-invocable": true } }
    });
    let (_, contents) = file(&cap(m), Harness::Claude);
    assert!(contents.contains("user-invocable: true"), "{contents}");
}

/// The nested form keeps working — it is now sugar for saying the same thing,
/// not the only spelling that works.
#[test]
fn the_nested_frontmatter_form_still_works_and_wins_on_conflict() {
    let plan = override_plan(
        agent_manifest(),
        json!({ "opencode": { "mode": "flat", "frontmatter": { "mode": "nested" } } }),
        Harness::OpenCode,
    );
    let Some(Artifact::File { contents, .. }) = plan.artifacts.into_iter().next() else {
        panic!("expected a file")
    };
    assert!(contents.contains("mode: nested"), "{contents}");
    assert!(!contents.contains("mode: flat"), "{contents}");
}

/// A reserved key with the wrong shape relocated nothing while reporting
/// success. It is now a blocked plan carrying what was found.
#[test]
fn a_reserved_override_key_with_the_wrong_shape_is_a_loud_error() {
    for (block, expected) in [
        (
            json!({"opencode": {"path": ["a", "b"]}}),
            "must be a string",
        ),
        (
            json!({"opencode": {"tools": {"a": 1}}}),
            "must be a list or a string",
        ),
        (
            json!({"opencode": {"frontmatter": "nope"}}),
            "must be a mapping",
        ),
        (json!({"opencode": "nope"}), "must be a mapping"),
    ] {
        let plan = override_plan(agent_manifest(), block.clone(), Harness::OpenCode);
        match plan.installability {
            Installability::Unsupported(reason) => assert!(
                reason.contains(expected),
                "expected `{expected}` in: {reason}"
            ),
            other => panic!("{block} should be blocked, got {other:?}"),
        }
    }
}

/// `tools` in a harness's own scalar spelling is what an author copying from a
/// native file will write, and it is accepted everywhere else a tool list is
/// read.
#[test]
fn a_tools_override_accepts_the_native_scalar_spelling() {
    let plan = override_plan(
        agent_manifest(),
        json!({ "claude-code": { "tools": "Read Grep Bash(git diff:*)" } }),
        Harness::Claude,
    );
    let Some(Artifact::File { contents, .. }) = plan.artifacts.into_iter().next() else {
        panic!("expected a file")
    };
    assert!(
        contents.contains("tools: [Read, Grep, Bash(git diff:*)]"),
        "the parenthesized spec must survive the split:\n{contents}"
    );
}

/// Treating unknown keys as frontmatter means a *typo* of a reserved key lands
/// as frontmatter too. That is recoverable and visible, where the old behaviour
/// was neither — but it is worth saying out loud. A note, never an error:
/// `paths` is a real frontmatter key on some harnesses, so refusing it would
/// break a legitimate override.
#[test]
fn a_near_miss_of_a_reserved_override_key_is_reported() {
    let plan = override_plan(
        agent_manifest(),
        json!({ "opencode": { "paht": ".opencode/agents/p.md", "tolos": ["read"] } }),
        Harness::OpenCode,
    );
    let notes = plan.notes.join("\n");
    assert!(
        notes.contains("overrides.opencode.paht`") && notes.contains("the `path` override"),
        "a transposition must be caught — plain Levenshtein scores it 2:\n{notes}"
    );
    assert!(
        notes.contains("overrides.opencode.tolos`") && notes.contains("the `tools` override"),
        "{notes}"
    );
}

#[test]
fn an_ordinary_frontmatter_key_is_not_accused_of_being_a_typo() {
    let plan = override_plan(
        agent_manifest(),
        json!({ "opencode": { "mode": "primary", "temperature": 0.2 } }),
        Harness::OpenCode,
    );
    assert!(
        plan.notes.is_empty(),
        "no note should fire for ordinary keys: {:?}",
        plan.notes
    );
}
