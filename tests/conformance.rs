//! Conformance tests.
//!
//! These encode each harness's *real* hook contract as assertions, and run the
//! actual Python and Node capabilities end-to-end through the dispatcher. They
//! are the executable form of the feasibility finding: they pass where the
//! normalization holds, and they document (via the matrix tests) exactly where
//! it does not.

use open_harness::adapters::{DenyStyle, Harness, Support};
use open_harness::event::{Boundary, NormEvent, Phase, SubjectKind, TaskKind, ToolClass};
use open_harness::kind::{kind_impl, Artifact, Installability, KindId};
use open_harness::manifest::{discover, LoadedCapability};
use open_harness::model::{merge, negotiate, parse_decision, Decision, Verdict};
use open_harness::sync::{self, plan_sync, ChangeAction, DeferReason, DriftKind, SyncPlan};
use serde_json::json;
use std::path::PathBuf;

fn caps() -> Vec<LoadedCapability> {
    discover(std::path::Path::new("capabilities")).expect("capabilities/ should load")
}

// ---- event model round-trips --------------------------------------------

#[test]
fn event_id_roundtrip() {
    for id in [
        "pre.tool.any",
        "pre.tool.shell",
        "post.tool.file_edit",
        "pre.model",
        "pre.prompt",
        "pre.session.start",
        "post.session.end",
        "pre.task.cancel",
    ] {
        let ev: NormEvent = id.parse().unwrap();
        assert_eq!(ev.id(), id, "round-trip failed for {id}");
    }
}

#[test]
fn only_pre_tool_model_prompt_are_blocking() {
    assert!(NormEvent::tool(Phase::Pre, ToolClass::Shell).blocking());
    assert!(NormEvent::simple(Phase::Pre, SubjectKind::Model).blocking());
    assert!(!NormEvent::tool(Phase::Post, ToolClass::Shell).blocking());
    assert!(!NormEvent {
        phase: Phase::Pre,
        subject: SubjectKind::Session,
        tool_class: None,
        boundary: Some(Boundary::Start),
        task_kind: None,
    }
    .blocking());
}

// ---- the support matrix: where it maps and where it breaks ---------------

#[test]
fn claude_maps_tool_natively() {
    match Harness::Claude.support(&NormEvent::tool(Phase::Pre, ToolClass::Any)) {
        Support::Native(name, _) => assert_eq!(name, "PreToolUse"),
        other => panic!("expected native, got {other:?}"),
    }
}

#[test]
fn cursor_fans_out_generic_tool_to_three_events() {
    match Harness::Cursor.support(&NormEvent::tool(Phase::Pre, ToolClass::Any)) {
        Support::Fanout(list) => assert_eq!(list.len(), 3),
        other => panic!("expected fan-out, got {other:?}"),
    }
}

#[test]
fn windsurf_fans_out_generic_tool_to_four_events() {
    match Harness::Windsurf.support(&NormEvent::tool(Phase::Pre, ToolClass::Any)) {
        Support::Fanout(list) => assert_eq!(list.len(), 4),
        other => panic!("expected fan-out, got {other:?}"),
    }
}

#[test]
fn only_gemini_has_a_model_hook() {
    assert!(matches!(
        Harness::Gemini.support(&NormEvent::simple(Phase::Pre, SubjectKind::Model)),
        Support::Native(_, _)
    ));
    for h in [
        Harness::Claude,
        Harness::Codex,
        Harness::Cursor,
        Harness::Cline,
    ] {
        assert!(matches!(
            h.support(&NormEvent::simple(Phase::Pre, SubjectKind::Model)),
            Support::Unsupported(_)
        ));
    }
}

#[test]
fn task_lifecycle_is_cline_only() {
    let ev = NormEvent {
        phase: Phase::Pre,
        subject: SubjectKind::Task,
        tool_class: None,
        boundary: None,
        task_kind: Some(TaskKind::Start),
    };
    assert!(matches!(Harness::Cline.support(&ev), Support::Native(_, _)));
    assert!(matches!(
        Harness::Claude.support(&ev),
        Support::Unsupported(_)
    ));
}

#[test]
fn aider_and_copilot_have_no_hook_target() {
    let ev = NormEvent::tool(Phase::Pre, ToolClass::Any);
    assert!(matches!(
        Harness::Aider.support(&ev),
        Support::Unsupported(_)
    ));
    assert!(matches!(
        Harness::Copilot.support(&ev),
        Support::Unsupported(_)
    ));
}

// ---- decode: per-harness field translation into the canonical shape ------

#[test]
fn decode_translates_divergent_native_field_names() {
    let ev = NormEvent::tool(Phase::Pre, ToolClass::Shell);

    let claude = Harness::Claude.decode(
        &json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
        &ev,
    );
    assert_eq!(claude.tool.unwrap().name, "Bash");

    // Cursor's beforeShellExecution carries the command as a top-level string
    // (verified) — normalized to a `shell` tool whose input holds the command.
    let cursor = Harness::Cursor.decode(
        &json!({"command": "ls", "hook_event_name": "beforeShellExecution"}),
        &ev,
    );
    let ctool = cursor.tool.unwrap();
    assert_eq!(ctool.name, "shell");
    assert_eq!(ctool.input["command"], json!("ls"));

    let windsurf = Harness::Windsurf.decode(
        &json!({"tool": {"name": "Bash", "args": {"command": "ls"}}}),
        &ev,
    );
    assert_eq!(windsurf.tool.unwrap().name, "Bash");
}

// ---- encode: the deny-signal fragmentation, made concrete ----------------

#[test]
fn deny_styles_are_grouped_correctly() {
    assert_eq!(Harness::Claude.deny_style(), DenyStyle::Exit2);
    assert_eq!(Harness::Windsurf.deny_style(), DenyStyle::Exit2);
    assert_eq!(Harness::Cursor.deny_style(), DenyStyle::CursorJson);
    // Verified (#5): Codex signals via stdout, not exit 2.
    assert_eq!(Harness::Codex.deny_style(), DenyStyle::CodexJson);
    assert_eq!(Harness::Cline.deny_style(), DenyStyle::ClineJson);
    assert_eq!(Harness::OpenCode.deny_style(), DenyStyle::InProcess);
}

#[test]
fn encode_deny_uses_each_harness_native_signal() {
    let ev = NormEvent::tool(Phase::Pre, ToolClass::Shell);
    let deny = Decision {
        decision: Verdict::Deny,
        reason: "nope".into(),
        context_append: None,
        modified_input: None,
    };

    // exit-2 family
    let r = Harness::Claude.encode(&deny, &ev);
    assert_eq!(r.exit_code, 2);
    assert!(r.stderr.contains("nope"));

    // cursor stdout-json permission
    let r = Harness::Cursor.encode(&deny, &ev);
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("\"permission\":\"deny\""));

    // cline stdout-json cancel
    let r = Harness::Cline.encode(&deny, &ev);
    assert!(r.stdout.contains("\"cancel\":true"));

    // in-process passes the canonical decision through
    let r = Harness::OpenCode.encode(&deny, &ev);
    assert!(r.stdout.contains("\"decision\":\"deny\""));
}

#[test]
fn non_blocking_event_cannot_deny() {
    // A deny returned on a post event must not turn into a block.
    let ev = NormEvent::tool(Phase::Post, ToolClass::Shell);
    let deny = Decision {
        decision: Verdict::Deny,
        reason: "too late".into(),
        context_append: None,
        modified_input: None,
    };
    let r = Harness::Claude.encode(&deny, &ev);
    assert_eq!(r.exit_code, 0, "post-phase deny must not block");
}

// ---- merge: composition semantics ----------------------------------------

#[test]
fn merge_is_deny_wins_and_concatenates_context() {
    let decisions = vec![
        Decision {
            decision: Verdict::Allow,
            reason: String::new(),
            context_append: Some("a".into()),
            modified_input: None,
        },
        Decision {
            decision: Verdict::Deny,
            reason: "bad".into(),
            context_append: Some("b".into()),
            modified_input: None,
        },
    ];
    let m = merge(&decisions);
    assert_eq!(m.decision, Verdict::Deny);
    assert_eq!(m.context_append.as_deref(), Some("a\nb"));
    assert!(m.reason.contains("bad"));
}

// ---- end-to-end: real Python + Node capabilities through the dispatcher ---

#[test]
fn e2e_safe_input_allows_everywhere() {
    let ev = NormEvent::tool(Phase::Pre, ToolClass::Any);
    let safe = json!({"tool_name": "Bash", "tool_input": {"command": "ls -la"}});
    for h in [Harness::Claude, Harness::Cline, Harness::OpenCode] {
        let out = open_harness::dispatch(h, &ev, &caps(), &safe);
        assert_eq!(
            out.response.exit_code,
            0,
            "{} should allow safe input",
            h.id()
        );
        assert!(out.ran.contains(&"secret-guard".to_string()));
        assert!(out.ran.contains(&"audit-note".to_string()));
    }
}

#[test]
fn e2e_secret_input_is_blocked_via_each_native_signal() {
    let ev = NormEvent::tool(Phase::Pre, ToolClass::Any);
    let secret =
        json!({"tool_name": "Bash", "tool_input": {"command": "echo AKIAIOSFODNN7EXAMPLE"}});

    // Claude: exit 2
    let out = open_harness::dispatch(Harness::Claude, &ev, &caps(), &secret);
    assert_eq!(out.response.exit_code, 2);
    assert!(out.response.stderr.contains("secret"));

    // Cline: stdout cancel:true
    let out = open_harness::dispatch(Harness::Cline, &ev, &caps(), &secret);
    assert!(out.response.stdout.contains("\"cancel\":true"));
}

#[test]
fn e2e_capability_crash_fails_closed_on_blocking_event() {
    // A capability that exits non-zero must DENY on a blocking event, never
    // silently allow.
    // Built from JSON so new manifest fields never break this fixture.
    let manifest: open_harness::manifest::Manifest = serde_json::from_value(json!({
        "id": "crasher",
        "name": "Crasher",
        "run": { "command": "python3", "args": ["-c", "import sys; sys.exit(3)"] },
        "events": [{ "phase": "pre", "subject": "tool", "tool_class": "any", "required": true }]
    }))
    .unwrap();
    let crashing = LoadedCapability {
        manifest,
        dir: std::path::PathBuf::from("."),
    };
    let ev = NormEvent::tool(Phase::Pre, ToolClass::Shell);
    let safe = json!({"tool_name": "Bash", "tool_input": {"command": "ls"}});
    let out = open_harness::dispatch(Harness::Claude, &ev, &[crashing], &safe);
    assert_eq!(out.response.exit_code, 2, "crash must fail closed (deny)");
    assert!(!out.errored.is_empty());
}

// ---- protocol (hook@1): validation + version negotiation -----------------

#[test]
fn strict_validation_rejects_malformed_decisions() {
    assert!(parse_decision("").is_ok(), "empty output means allow");
    assert!(parse_decision("{}").is_ok(), "empty object means allow");
    assert!(parse_decision(r#"{"decision":"deny","reason":"x"}"#).is_ok());
    // forward-compat: unknown fields are ignored, not rejected
    assert!(parse_decision(r#"{"decision":"allow","future_field":true}"#).is_ok());
    // rejected: unknown verdict and wrong types
    assert!(parse_decision(r#"{"decision":"banana"}"#).is_err());
    assert!(parse_decision(r#"{"decision":123}"#).is_err());
    assert!(parse_decision(r#"{"reason":42}"#).is_err());
}

#[test]
fn protocol_negotiation_refuses_major_mismatch() {
    assert!(negotiate(None).is_ok(), "no declaration => compatible");
    assert!(negotiate(Some("hook@1")).is_ok());
    assert!(
        negotiate(Some("hook@1.4")).is_ok(),
        "minor bumps are compatible"
    );
    assert!(negotiate(Some("open-harness/hook@1")).is_ok());
    assert!(negotiate(Some("hook@2")).is_err(), "major mismatch refused");
    assert!(negotiate(Some("garbage")).is_err());
}

// ---- kind abstraction: hooks are now the first kind behind the trait ------

#[test]
fn manifest_defaults_to_hook_kind() {
    let m: open_harness::manifest::Manifest =
        serde_json::from_value(json!({ "id": "x", "run": { "command": "true" } })).unwrap();
    assert_eq!(m.kind, KindId::Hook);
    assert!(m.run.is_some());
}

#[test]
fn hook_kind_plans_via_the_abstraction() {
    let caps = caps();
    let guard = caps
        .iter()
        .find(|c| c.manifest.id == "secret-guard")
        .expect("secret-guard capability present");
    assert_eq!(guard.manifest.kind, KindId::Hook);

    let k = kind_impl(guard.manifest.kind);

    // Claude: clean install, exactly one registration artifact mentioning the event.
    let plan = k.plan(guard, Harness::Claude);
    assert!(matches!(plan.installability, Installability::Clean));
    match plan.artifacts.first() {
        Some(Artifact::Registration { text }) => assert!(text.contains("PreToolUse")),
        other => panic!("expected a registration artifact, got {other:?}"),
    }

    // Cursor: degraded (the pre.tool.any fan-out).
    assert!(matches!(
        k.plan(guard, Harness::Cursor).installability,
        Installability::Degraded(_)
    ));

    // Aider: required event with no hook mechanism -> BLOCKED, no artifacts.
    let aider = k.plan(guard, Harness::Aider);
    assert!(matches!(
        aider.installability,
        Installability::Unsupported(_)
    ));
    assert!(aider.artifacts.is_empty());
}

// ---- skills kind: SKILL.md unified across vendors (a generative kind) -----

fn skill_cap() -> LoadedCapability {
    caps()
        .into_iter()
        .find(|c| c.manifest.id == "commit-style")
        .expect("commit-style skill capability present")
}

#[test]
fn skill_kind_generates_skill_md_for_supporting_harnesses() {
    let cap = skill_cap();
    assert_eq!(cap.manifest.kind, KindId::Skill);
    let k = kind_impl(cap.manifest.kind);

    let expected_paths = [
        (Harness::Claude, ".claude/skills/commit-style/SKILL.md"),
        (Harness::Codex, ".agents/skills/commit-style/SKILL.md"),
        (Harness::Cursor, ".cursor/skills/commit-style/SKILL.md"),
        (Harness::Windsurf, ".windsurf/skills/commit-style/SKILL.md"),
    ];
    for (h, expected) in expected_paths {
        let plan = k.plan(&cap, h);
        assert!(
            matches!(plan.installability, Installability::Clean),
            "{}",
            h.id()
        );
        match plan.artifacts.first() {
            Some(Artifact::File { path, contents }) => {
                assert_eq!(path, expected);
                assert!(contents.contains("name: Commit Style"));
                assert!(contents.contains("allowed-tools: Read"));
                assert!(contents.contains("Conventional Commits"));
            }
            other => panic!("{}: expected a File artifact, got {other:?}", h.id()),
        }
    }

    // A harness with no SKILL.md directory is Unsupported, not silently dropped.
    assert!(matches!(
        k.plan(&cap, Harness::Gemini).installability,
        Installability::Unsupported(_)
    ));
}

#[test]
fn skill_roundtrip_import() {
    let cap = skill_cap();
    let plan = kind_impl(KindId::Skill).plan(&cap, Harness::Claude);
    let Some(Artifact::File { contents, .. }) = plan.artifacts.first() else {
        panic!("expected a File artifact");
    };
    let imported = open_harness::kinds::skill::import_skill_md(contents).unwrap();
    assert_eq!(imported.name, "Commit Style");
    assert_eq!(
        imported.description,
        "Teaches the agent this repo's commit message conventions."
    );
    assert_eq!(imported.allowed_tools, vec!["Read".to_string()]);
    assert!(imported.body.contains("Conventional Commits"));
}

// ---- rules kind: same "glob-scoped rule" concept, five native spellings ---

fn rule_cap() -> LoadedCapability {
    caps()
        .into_iter()
        .find(|c| c.manifest.id == "rpc-conventions")
        .expect("rpc-conventions rule capability present")
}

fn inline_rule(activation: &str) -> LoadedCapability {
    let manifest: open_harness::manifest::Manifest = serde_json::from_value(json!({
        "id": "naming",
        "name": "Naming",
        "description": "Use descriptive identifiers.",
        "kind": "rule",
        "rule": { "activation": activation, "body": "Prefer descriptive names." }
    }))
    .unwrap();
    LoadedCapability {
        manifest,
        dir: std::path::PathBuf::from("."),
    }
}

#[test]
fn rule_kind_generates_each_native_spelling_for_a_glob_rule() {
    let cap = rule_cap();
    assert_eq!(cap.manifest.kind, KindId::Rule);
    let k = kind_impl(cap.manifest.kind);
    let csv = "src/rpc/**/*.rs,src/rpc/**/*.ts";

    // (harness, expected path, expected frontmatter substring)
    let cases: [(Harness, &str, &str); 5] = [
        (
            Harness::Cursor,
            ".cursor/rules/rpc-conventions.mdc",
            "alwaysApply: false",
        ),
        (
            Harness::Copilot,
            ".github/instructions/rpc-conventions.instructions.md",
            "applyTo: src/rpc/**/*.rs,src/rpc/**/*.ts",
        ),
        (
            Harness::Windsurf,
            ".windsurf/rules/rpc-conventions.md",
            "trigger: glob",
        ),
        (Harness::Cline, ".clinerules/rpc-conventions.md", "paths:"),
        (
            Harness::Claude,
            ".claude/rules/rpc-conventions.md",
            "paths:",
        ),
    ];
    for (h, path, needle) in cases {
        let plan = k.plan(&cap, h);
        assert!(
            matches!(plan.installability, Installability::Clean),
            "{}",
            h.id()
        );
        match plan.artifacts.first() {
            Some(Artifact::File { path: p, contents }) => {
                assert_eq!(p, path, "{}", h.id());
                assert!(
                    contents.contains(needle),
                    "{}: missing {needle}\n{contents}",
                    h.id()
                );
            }
            other => panic!("{}: expected a File artifact, got {other:?}", h.id()),
        }
    }
    // Cursor and Copilot spell globs as a comma string.
    let cursor = k.plan(&cap, Harness::Cursor);
    if let Some(Artifact::File { contents, .. }) = cursor.artifacts.first() {
        assert!(contents.contains(&format!("globs: {csv}")));
    }
    // Cline spells them as a YAML array.
    let cline = k.plan(&cap, Harness::Cline);
    if let Some(Artifact::File { contents, .. }) = cline.artifacts.first() {
        assert!(contents.contains("- \"src/rpc/**/*.rs\""));
    }
}

#[test]
fn rule_agent_requested_degrades_where_unsupported() {
    let cap = inline_rule("agent_requested");
    let k = kind_impl(KindId::Rule);
    // Native on Cursor and Windsurf.
    assert!(matches!(
        k.plan(&cap, Harness::Cursor).installability,
        Installability::Clean
    ));
    assert!(matches!(
        k.plan(&cap, Harness::Windsurf).installability,
        Installability::Clean
    ));
    // Degraded (with a note) on the three that have no model-decision mode.
    for h in [Harness::Copilot, Harness::Cline, Harness::Claude] {
        let plan = k.plan(&cap, h);
        assert!(
            matches!(plan.installability, Installability::Degraded(_)),
            "{}",
            h.id()
        );
        assert!(
            !plan.notes.is_empty(),
            "{} should carry a degradation note",
            h.id()
        );
        // Still emitted, never silently dropped.
        assert!(matches!(
            plan.artifacts.first(),
            Some(Artifact::File { .. })
        ));
    }
}

#[test]
fn rule_unsupported_on_agents_md_only_harnesses() {
    let cap = rule_cap();
    let k = kind_impl(KindId::Rule);
    for h in [
        Harness::Gemini,
        Harness::OpenCode,
        Harness::Pi,
        Harness::Aider,
    ] {
        assert!(
            matches!(
                k.plan(&cap, h).installability,
                Installability::Unsupported(_)
            ),
            "{} has no structured rules dir",
            h.id()
        );
    }
}

// ---- tools kind: MCP config divergence, and an MCP-free CLI delivery ------

fn cap_from(value: serde_json::Value) -> LoadedCapability {
    LoadedCapability {
        manifest: serde_json::from_value(value).unwrap(),
        dir: std::path::PathBuf::from("."),
    }
}

fn tool_cap(id: &str) -> LoadedCapability {
    caps()
        .into_iter()
        .find(|c| c.manifest.id == id)
        .unwrap_or_else(|| panic!("{id} tool capability present"))
}

#[test]
fn tool_mcp_generates_key_and_format_divergences() {
    let cap = tool_cap("web-search");
    assert_eq!(cap.manifest.kind, KindId::Tool);
    let k = kind_impl(cap.manifest.kind);
    let file = |h| match k.plan(&cap, h).artifacts.into_iter().next() {
        Some(Artifact::File { path, contents }) => (path, contents),
        other => panic!("{:?}: expected a File", other),
    };

    let (p, c) = file(Harness::Claude);
    assert_eq!(p, ".mcp.json");
    assert!(c.contains("\"mcpServers\"") && c.contains("@example/web-search-mcp"));

    // The key divergence: VS Code / Copilot use `servers`, not `mcpServers`.
    let (p, c) = file(Harness::Copilot);
    assert_eq!(p, ".vscode/mcp.json");
    assert!(c.contains("\"servers\"") && !c.contains("\"mcpServers\""));

    // Codex is TOML.
    let (p, c) = file(Harness::Codex);
    assert_eq!(p, ".codex/config.toml");
    assert!(c.contains("[mcp_servers.web-search]") && c.contains("command = \"npx\""));

    // OpenCode: type:local, command-as-array, `environment` not `env`.
    let (_p, c) = file(Harness::OpenCode);
    assert!(c.contains("\"type\": \"local\"") && c.contains("\"environment\""));
}

#[test]
fn tool_mcp_http_transport_field_diverges() {
    let cap = cap_from(json!({
        "id": "remote", "name": "Remote", "description": "d", "kind": "tool",
        "tool": { "provider": "mcp", "delivery": "mcp",
                  "server": { "transport": "http", "url": "https://mcp.example.com",
                              "headers": { "Authorization": "Bearer x" } } }
    }));
    let k = kind_impl(KindId::Tool);
    let contents = |h| match k.plan(&cap, h).artifacts.into_iter().next() {
        Some(Artifact::File { contents, .. }) => contents,
        _ => panic!("expected a File"),
    };
    // Gemini spells streamable HTTP as `httpUrl`; Claude uses `type:http`+`url`.
    assert!(contents(Harness::Gemini).contains("\"httpUrl\""));
    let claude = contents(Harness::Claude);
    assert!(claude.contains("\"type\": \"http\"") && !claude.contains("httpUrl"));
    // Cursor infers remote from `url`, with no `type`.
    let cursor = contents(Harness::Cursor);
    assert!(cursor.contains("\"url\"") && !cursor.contains("\"type\""));
}

#[test]
fn tool_exec_delivers_cli_mcp_free_everywhere() {
    let cap = tool_cap("repo-grep");
    let k = kind_impl(cap.manifest.kind);
    // Works even on harnesses with no MCP at all (Aider) — that's the point.
    for h in [Harness::Claude, Harness::Aider] {
        let plan = k.plan(&cap, h);
        assert!(
            matches!(plan.installability, Installability::Clean),
            "{}",
            h.id()
        );
        match plan.artifacts.first() {
            Some(Artifact::File { path, contents }) => {
                assert_eq!(path, ".open-harness/tools/repo-grep.md");
                assert!(contents.contains("no MCP") && contents.contains("rg --json"));
            }
            other => panic!("{}: expected a File, got {other:?}", h.id()),
        }
    }
}

#[test]
fn tool_mcp_over_cli_is_bridged_and_flagged() {
    let cap = cap_from(json!({
        "id": "search", "name": "Search", "description": "d", "kind": "tool",
        "tool": { "provider": "mcp", "delivery": "cli",
                  "server": { "transport": "stdio", "command": "npx", "args": ["-y", "x"] },
                  "tools": ["query"] }
    }));
    let plan = kind_impl(KindId::Tool).plan(&cap, Harness::Claude);
    assert!(matches!(plan.installability, Installability::Degraded(_)));
    // The honest caveat rides on every bridge (note + artifact).
    assert!(plan.notes.iter().any(|n| n.contains("still runs")));
    // A real bridge emits a wrapper script + an instructions artifact.
    let paths: Vec<&str> = plan
        .artifacts
        .iter()
        .filter_map(|a| match a {
            Artifact::File { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("search-bridge.sh")));
    assert!(paths.iter().any(|p| p.ends_with("search.md")));
}

#[test]
fn tool_mcp_unsupported_on_aider() {
    // web-search resolves delivery=auto -> mcp; Aider has no MCP.
    let cap = tool_cap("web-search");
    assert!(matches!(
        kind_impl(KindId::Tool)
            .plan(&cap, Harness::Aider)
            .installability,
        Installability::Unsupported(_)
    ));
}

/// #19: a `provider: mcp`, `delivery: cli` tool emits a real bridge — a shell
/// wrapper + an instructions artifact naming each tool + the "server still runs"
/// caveat (in the artifact and the notes).
#[test]
fn tool_mcp_cli_bridge_emits_wrapper_instructions_and_caveat() {
    let cap = tool_cap("echo-bridge");
    let plan = kind_impl(KindId::Tool).plan(&cap, Harness::Claude);
    assert!(
        matches!(plan.installability, Installability::Degraded(_)),
        "the bridge is an approximation of native MCP → Degraded"
    );

    let files: Vec<(String, String)> = plan
        .artifacts
        .iter()
        .filter_map(|a| match a {
            Artifact::File { path, contents } => Some((path.clone(), contents.clone())),
            _ => None,
        })
        .collect();

    // A runnable shell wrapper backed by `oh mcp call`.
    let (_, wrapper) = files
        .iter()
        .find(|(p, _)| p.ends_with("echo-bridge-bridge.sh"))
        .expect("a bridge wrapper script");
    assert!(wrapper.contains("oh mcp call") && wrapper.contains("--id echo-bridge"));

    // Instructions naming each tool + the caveat.
    let (_, md) = files
        .iter()
        .find(|(p, _)| p.ends_with(".md"))
        .expect("an instructions artifact");
    assert!(
        md.contains("`echo`") && md.contains("`add`"),
        "names each tool"
    );
    assert!(
        md.contains("still starts the MCP server"),
        "states the caveat"
    );

    // The caveat is also in the plan notes.
    assert!(plan.notes.iter().any(|n| n.contains("still runs")));
}

// ---- the stable embeddable API (the surface bindings call) ----------------

use open_harness::api;

#[test]
fn api_lists_harnesses_and_kinds() {
    let hs = api::harnesses();
    assert_eq!(hs.len(), 11);
    assert!(hs.contains(&"claude-code".to_string()));
    assert!(hs.contains(&"antigravity".to_string()));
    let ks = api::kinds();
    assert!(
        ks.contains(&"hook".to_string())
            && ks.contains(&"tool".to_string())
            && ks.contains(&"command".to_string())
            && ks.contains(&"agent".to_string())
            && ks.contains(&"instructions".to_string())
    );
    assert_eq!(api::protocol_version(), "hook@1");
}

#[test]
fn api_plan_maps_installability_and_artifacts() {
    // A hook plans to a registration on Claude, BLOCKED on Aider.
    let hook = r#"{"id":"g","kind":"hook","run":{"command":"true"},
        "events":[{"phase":"pre","subject":"tool","tool_class":"any","required":true}]}"#;
    let claude = api::plan(hook, ".", "claude-code").unwrap();
    assert_eq!(claude.installability, "clean");
    assert_eq!(claude.artifacts.first().unwrap().kind, "registration");
    let aider = api::plan(hook, ".", "aider").unwrap();
    assert_eq!(aider.installability, "unsupported");

    // An MCP-free exec tool plans to a file even on Aider.
    let tool = r#"{"id":"t","name":"T","description":"d","kind":"tool",
        "tool":{"provider":"exec","delivery":"cli","exec":{"command":"rg"}}}"#;
    let p = api::plan(tool, ".", "aider").unwrap();
    assert_eq!(p.installability, "clean");
    assert_eq!(p.artifacts.first().unwrap().kind, "file");
    assert_eq!(
        p.artifacts.first().unwrap().path,
        ".open-harness/tools/t.md"
    );

    // plan_all covers every harness in one call.
    assert_eq!(api::plan_all(hook, ".").unwrap().len(), 11);
}

#[test]
fn api_dispatch_over_json_boundary() {
    let secret = r#"{"tool_name":"Bash","tool_input":{"command":"echo AKIAIOSFODNN7EXAMPLE"}}"#;
    let blocked = api::dispatch("claude-code", "pre.tool.any", secret, "capabilities").unwrap();
    assert_eq!(blocked.exit_code, 2);
    assert!(blocked.ran.contains(&"secret-guard".to_string()));

    let safe = r#"{"tool_name":"Bash","tool_input":{"command":"ls -la"}}"#;
    let allowed = api::dispatch("claude-code", "pre.tool.any", safe, "capabilities").unwrap();
    assert_eq!(allowed.exit_code, 0);
}

#[test]
fn api_returns_errors_not_panics() {
    let valid = r#"{"id":"x","kind":"hook","run":{"command":"true"}}"#;
    assert!(matches!(
        api::plan(valid, ".", "not-a-harness"),
        Err(api::ApiError::Harness(_))
    ));
    assert!(matches!(
        api::plan("{ not json", ".", "claude-code"),
        Err(api::ApiError::Manifest(_))
    ));
    assert!(matches!(
        api::dispatch("claude-code", "bogus.event", "{}", "capabilities"),
        Err(api::ApiError::Event(_))
    ));
    assert!(api::negotiate(Some("hook@2".to_string())).is_err());
}

// ---- commands kind: one command -> each harness's native file + arg syntax -

fn command_cap() -> LoadedCapability {
    caps()
        .into_iter()
        .find(|c| c.manifest.id == "review-pr")
        .expect("review-pr command capability present")
}

#[test]
fn command_generates_native_format_per_harness() {
    let cap = command_cap();
    assert_eq!(cap.manifest.kind, KindId::Command);
    let k = kind_impl(cap.manifest.kind);
    let file = |h| match k.plan(&cap, h).artifacts.into_iter().next() {
        Some(Artifact::File { path, contents }) => (path, contents),
        other => panic!("{:?}: expected a File", other),
    };

    // Markdown+YAML with $ARGUMENTS on the dollar family.
    let (p, c) = file(Harness::Claude);
    assert_eq!(p, ".claude/commands/review-pr.md");
    assert!(c.contains("description:") && c.contains("$ARGUMENTS"));

    // Gemini is TOML with a {{args}} prompt.
    let (p, c) = file(Harness::Gemini);
    assert_eq!(p, ".gemini/commands/review-pr.toml");
    assert!(c.contains("prompt = ") && c.contains("{{args}}"));

    let (p, c) = file(Harness::Copilot);
    assert_eq!(p, ".github/prompts/review-pr.prompt.md");
    assert!(c.contains("$ARGUMENTS"));

    assert_eq!(file(Harness::OpenCode).0, ".opencode/commands/review-pr.md");

    // Codex custom prompts are global -> degraded (but still emitted).
    assert!(matches!(
        k.plan(&cap, Harness::Codex).installability,
        Installability::Degraded(_)
    ));
}

#[test]
fn command_positional_args_degrade_on_gemini() {
    let cap = cap_from(json!({
        "id": "deploy", "name": "Deploy", "description": "d", "kind": "command",
        "command": { "body": "deploy service {{arg1}} to {{arg2}}" }
    }));
    let k = kind_impl(KindId::Command);

    // The dollar family substitutes positional args.
    let claude = match k.plan(&cap, Harness::Claude).artifacts.into_iter().next() {
        Some(Artifact::File { contents, .. }) => contents,
        _ => panic!("expected a File"),
    };
    assert!(claude.contains("$1") && claude.contains("$2"));

    // Gemini has no positional args -> degraded, still emitted with a note.
    let g = k.plan(&cap, Harness::Gemini);
    assert!(matches!(g.installability, Installability::Degraded(_)));
    assert!(!g.notes.is_empty());
    assert!(matches!(g.artifacts.first(), Some(Artifact::File { .. })));
}

#[test]
fn command_unsupported_on_aider() {
    assert!(matches!(
        kind_impl(KindId::Command)
            .plan(&command_cap(), Harness::Aider)
            .installability,
        Installability::Unsupported(_)
    ));
}

// ---- permissions kind: faithful on 2, approximated on 5, unsupported on 3 --

fn permission_cap() -> LoadedCapability {
    caps()
        .into_iter()
        .find(|c| c.manifest.id == "safe-shell")
        .expect("safe-shell permission capability present")
}

#[test]
fn permission_maps_faithfully_to_claude_and_opencode() {
    let cap = permission_cap();
    assert_eq!(cap.manifest.kind, KindId::Permission);
    let k = kind_impl(cap.manifest.kind);

    let claude = match k.plan(&cap, Harness::Claude).artifacts.into_iter().next() {
        Some(Artifact::File { path, contents }) => {
            assert_eq!(path, ".claude/settings.json");
            contents
        }
        _ => panic!("expected a File"),
    };
    assert!(claude.contains("\"Bash(git *)\"")); // allow
    assert!(claude.contains("\"Bash(rm *)\"")); // deny
    assert!(claude.contains("\"Read(./.env)\"")); // deny, non-shell tool
    assert!(matches!(
        k.plan(&cap, Harness::Claude).installability,
        Installability::Clean
    ));

    let oc = match k.plan(&cap, Harness::OpenCode).artifacts.into_iter().next() {
        Some(Artifact::File { path, contents }) => {
            assert_eq!(path, "opencode.json");
            contents
        }
        _ => panic!("expected a File"),
    };
    assert!(oc.contains("\"permission\"") && oc.contains("\"git *\": \"allow\""));
    assert!(oc.contains("\"rm *\": \"deny\""));
}

#[test]
fn permission_approximated_with_loss_reports() {
    let cap = permission_cap();
    let k = kind_impl(cap.manifest.kind);

    // Codex collapses to a coarse approval policy (has denies -> on-request).
    let codex = k.plan(&cap, Harness::Codex);
    assert!(matches!(codex.installability, Installability::Degraded(_)));
    assert!(!codex.notes.is_empty());
    if let Some(Artifact::File { contents, .. }) = codex.artifacts.first() {
        assert!(contents.contains("approval_policy = \"on-request\""));
    }

    // Gemini approximates shell allows via tools.allowed with command specificity.
    let gemini = k.plan(&cap, Harness::Gemini);
    assert!(matches!(gemini.installability, Installability::Degraded(_)));
    if let Some(Artifact::File { contents, .. }) = gemini.artifacts.first() {
        assert!(contents.contains("run_shell_command(git)"));
    }

    // Cursor / Windsurf / Copilot are shell-only approximations, all Degraded.
    for h in [Harness::Cursor, Harness::Windsurf, Harness::Copilot] {
        assert!(
            matches!(k.plan(&cap, h).installability, Installability::Degraded(_)),
            "{}",
            h.id()
        );
    }
}

#[test]
fn permission_unsupported_where_no_committed_file() {
    let cap = permission_cap();
    let k = kind_impl(cap.manifest.kind);
    for h in [Harness::Pi, Harness::Cline, Harness::Aider] {
        assert!(
            matches!(
                k.plan(&cap, h).installability,
                Installability::Unsupported(_)
            ),
            "{} should be Unsupported (no committed permission file)",
            h.id()
        );
    }
}

// ---- sync + composition (#16): compose a set, converge on disk, detect drift -

fn tmp_project(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-sync-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create temp project");
    p
}

fn desired<'a>(plan: &'a SyncPlan, path: &str) -> &'a open_harness::sync::DesiredFile {
    plan.files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no desired file at {path}"))
}

fn only(ids: &[&str]) -> Vec<LoadedCapability> {
    caps()
        .into_iter()
        .filter(|c| ids.contains(&c.manifest.id.as_str()))
        .collect()
}

/// `.vscode/settings.json` is one real file that both Windsurf and Copilot
/// target — composition merges them into a single file, keyed by path (not by
/// harness), so neither clobbers the other.
#[test]
fn sync_composes_shared_file_across_harnesses() {
    let plan = plan_sync(&caps(), &[Harness::Windsurf, Harness::Copilot]);
    let f = desired(&plan, ".vscode/settings.json");
    assert_eq!(f.harnesses, vec!["copilot", "windsurf"]); // sorted, both present
    assert!(f.contents.contains("windsurf.cascadeCommandsAllowList")); // Windsurf's keys
    assert!(f.contents.contains("chat.tools.terminal.autoApprove")); // Copilot's keys
}

/// Two permission capabilities that overlap on `git *` (allow vs ask) compose:
/// on OpenCode's scalar verdict map the most-restrictive wins (ask > allow),
/// and the clash is recorded as a conflict — the hook deny-wins rule, extended.
#[test]
fn sync_permission_merge_is_most_restrictive() {
    let plan = plan_sync(&only(&["safe-shell", "strict-ci"]), &[Harness::OpenCode]);
    let f = desired(&plan, "opencode.json");
    assert_eq!(f.sources, vec!["safe-shell", "strict-ci"]);
    let v: serde_json::Value = serde_json::from_str(&f.contents).unwrap();
    assert_eq!(v["permission"]["bash"]["git *"], json!("ask")); // ask beats allow
    assert_eq!(v["permission"]["bash"]["git push --force *"], json!("deny")); // unioned
    assert!(
        f.conflicts.iter().any(|c| c.contains("git *")),
        "the verdict clash must be reported, got {:?}",
        f.conflicts
    );
}

/// Claude represents verdicts as array membership, so an overlapping pattern
/// lands in both `allow` and `ask`; Claude's own precedence (ask/deny > allow)
/// resolves it. We union honestly rather than silently pruning.
#[test]
fn sync_permission_merge_unions_claude_arrays() {
    let plan = plan_sync(&only(&["safe-shell", "strict-ci"]), &[Harness::Claude]);
    let v: serde_json::Value =
        serde_json::from_str(&desired(&plan, ".claude/settings.json").contents).unwrap();
    let has = |arr: &serde_json::Value, s: &str| arr.as_array().unwrap().iter().any(|e| e == s);
    assert!(has(&v["permissions"]["allow"], "Bash(git *)")); // from safe-shell
    assert!(has(&v["permissions"]["ask"], "Bash(git *)")); // from strict-ci
    assert!(has(&v["permissions"]["deny"], "Bash(git push --force *)"));
}

/// The composition is a pure function of its inputs: same caps + harnesses →
/// byte-identical desired files, in a stable order.
#[test]
fn sync_plan_is_deterministic() {
    let a = plan_sync(&caps(), &ALL_HARNESSES);
    let b = plan_sync(&caps(), &ALL_HARNESSES);
    assert_eq!(a.files, b.files);
    let mut sorted = a.files.clone();
    sorted.sort_by(|x, y| x.path.cmp(&y.path));
    assert_eq!(a.files, sorted, "files must be path-sorted");
}

const ALL_HARNESSES: [Harness; 10] = [
    Harness::Claude,
    Harness::Codex,
    Harness::Gemini,
    Harness::Cursor,
    Harness::Windsurf,
    Harness::Cline,
    Harness::OpenCode,
    Harness::Pi,
    Harness::Aider,
    Harness::Copilot,
];

/// Nothing is silently dropped: a hook becomes a deferred *registration*, a
/// command whose only target is a global `~/…` path a *global-path*, and a rule
/// with no home on the harness a *blocked* (unsupported) — all reported.
#[test]
fn sync_defers_registrations_global_paths_and_blocks() {
    // Codex exercises all three: hooks (registration), commands (~/.codex global
    // path), and rules (no structured rules dir -> unsupported).
    let plan = plan_sync(&caps(), &[Harness::Codex]);
    let reason = |r: DeferReason| {
        plan.deferred
            .iter()
            .filter(|d| d.reason == r)
            .map(|d| d.source.as_str())
            .collect::<Vec<_>>()
    };
    assert!(
        reason(DeferReason::Registration).contains(&"secret-guard"),
        "hook must defer as a registration"
    );
    assert!(
        reason(DeferReason::GlobalPath).contains(&"review-pr"),
        "~/.codex/prompts command must defer as a global path"
    );
    assert!(
        reason(DeferReason::Unsupported).contains(&"rpc-conventions"),
        "a rule with no Codex target must be blocked"
    );
}

/// A TOML target can't be structurally merged; two producers with different
/// contents keep the first (by id order) and record a loud conflict.
#[test]
fn sync_non_json_conflict_keeps_first_loudly() {
    // On Codex, `.codex/config.toml` is produced by both the permission kind
    // (coarse approval policy) and the tool kind (mcp_servers) — different TOML.
    let plan = plan_sync(&caps(), &[Harness::Codex]);
    let f = desired(&plan, ".codex/config.toml");
    assert!(
        !f.conflicts.is_empty(),
        "non-mergeable TOML clash must be reported"
    );
    assert!(
        f.contents.contains("approval_policy"),
        "first producer by id order (permission) wins"
    );
}

/// `apply` writes the composed set; a follow-up `check` sees no drift; a second
/// `apply` is a pure no-op (idempotent convergence).
#[test]
fn sync_apply_is_idempotent_and_checks_clean() {
    let root = tmp_project("idem");
    let plan = plan_sync(&caps(), &[Harness::Claude]);

    let first = sync::apply(&root, &plan, false).unwrap();
    assert!(first.mutated());
    assert_eq!(first.count(ChangeAction::Create), plan.files.len());

    let drift = sync::check(&root, &plan).unwrap();
    assert!(!drift.has_drift(), "freshly synced project must be in sync");

    let second = sync::apply(&root, &plan, false).unwrap();
    assert!(!second.mutated(), "re-apply must be a no-op");
    assert_eq!(second.count(ChangeAction::Unchanged), plan.files.len());

    let _ = std::fs::remove_dir_all(&root);
}

/// `check` distinguishes a modified file from a missing one and flags drift —
/// the CI failure condition.
#[test]
fn sync_check_detects_missing_and_modified() {
    let root = tmp_project("drift");
    let plan = plan_sync(&caps(), &[Harness::Claude]);
    sync::apply(&root, &plan, false).unwrap();

    std::fs::write(root.join(".claude/settings.json"), "tampered").unwrap();
    std::fs::remove_file(root.join(".claude/skills/commit-style/SKILL.md")).unwrap();

    let drift = sync::check(&root, &plan).unwrap();
    assert!(drift.has_drift());
    assert_eq!(drift.count(DriftKind::Modified), 1);
    assert_eq!(drift.count(DriftKind::Missing), 1);

    let _ = std::fs::remove_dir_all(&root);
}

/// `dry_run` computes the full report but touches nothing on disk.
#[test]
fn sync_dry_run_writes_nothing() {
    let root = tmp_project("dry");
    let plan = plan_sync(&caps(), &[Harness::Claude]);
    let report = sync::apply(&root, &plan, true).unwrap();
    assert!(report.dry_run);
    assert!(report.count(ChangeAction::Create) >= 1);
    assert!(!root.join(".claude/settings.json").exists());
    assert!(!root.join(sync::LOCK_REL).exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// Removing a capability from the set prunes the file it owned on the next
/// `apply`, but never touches a file open-harness didn't write.
#[test]
fn sync_prunes_removed_capabilities_and_spares_unmanaged() {
    let root = tmp_project("prune");
    sync::apply(&root, &plan_sync(&caps(), &[Harness::Claude]), false).unwrap();
    std::fs::write(root.join("UNMANAGED.txt"), "hand-written").unwrap();
    assert!(root.join(".claude/settings.json").exists());

    // Narrow the set to a single capability: the rest must be pruned.
    let narrow = plan_sync(&only(&["commit-style"]), &[Harness::Claude]);
    let report = sync::apply(&root, &narrow, false).unwrap();
    assert!(report.count(ChangeAction::Prune) >= 1);

    assert!(
        !root.join(".claude/settings.json").exists(),
        "the removed capability's file must be pruned"
    );
    assert!(
        root.join(".claude/skills/commit-style/SKILL.md").exists(),
        "the kept capability's file must remain"
    );
    assert!(
        root.join("UNMANAGED.txt").exists(),
        "an unmanaged file must never be pruned"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `uninstall` removes every managed file and the lockfile, leaving unmanaged
/// files untouched — a clean uninstall.
#[test]
fn sync_uninstall_is_clean() {
    let root = tmp_project("uninstall");
    sync::apply(&root, &plan_sync(&caps(), &[Harness::Claude]), false).unwrap();
    std::fs::write(root.join("UNMANAGED.txt"), "hand-written").unwrap();

    let report = sync::uninstall(&root, false).unwrap();
    assert!(report.count(ChangeAction::Prune) >= 1);
    assert!(!root.join(".claude/settings.json").exists());
    assert!(
        !root.join(sync::LOCK_REL).exists(),
        "lockfile must be cleared"
    );
    assert!(
        root.join("UNMANAGED.txt").exists(),
        "uninstall must spare unmanaged files"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---- runtime hardening (#3): timeouts, output caps, taxonomy, fail policy ----

use open_harness::runtime::RunLimits;

// (reuses the `cap_from` helper defined for the tools-kind tests)

/// A hook capability that runs a Python one-liner. `python3` is resolved to the
/// platform interpreter (`python` on Windows), so these fixtures are
/// cross-platform — no `sh`/`sleep`/`printf` assumptions.
fn py_hook(id: &str, script: &str, extra: serde_json::Value) -> LoadedCapability {
    let mut m = json!({
        "id": id,
        "run": { "command": "python3", "args": ["-c", script] },
        "events": [{ "phase": "pre", "subject": "tool", "tool_class": "any" }],
    });
    // merge extra top-level keys (e.g. timeout_ms) and binding overrides.
    if let (Some(mo), Some(eo)) = (m.as_object_mut(), extra.as_object()) {
        for (k, v) in eo {
            mo.insert(k.clone(), v.clone());
        }
    }
    cap_from(m)
}

fn blocking_tool() -> NormEvent {
    NormEvent::tool(Phase::Pre, ToolClass::Shell)
}

fn native() -> serde_json::Value {
    json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } })
}

/// A capability that hangs (a 30s Python sleep), so the killed process is the
/// direct child, cross-platform. `timeout_ms` defaults to 300ms.
fn hang_cap(id: &str, overrides: serde_json::Value) -> LoadedCapability {
    let mut m = json!({
        "id": id,
        "run": { "command": "python3", "args": ["-c", "import time; time.sleep(30)"] },
        "events": [{ "phase": "pre", "subject": "tool", "tool_class": "any" }],
        "timeout_ms": 300,
    });
    if let (Some(mo), Some(oo)) = (m.as_object_mut(), overrides.as_object()) {
        for (k, v) in oo {
            mo.insert(k.clone(), v.clone());
        }
    }
    cap_from(m)
}

/// A hung capability is killed at its timeout and, being blocking, fails closed
/// to a deny — the headline acceptance criterion.
#[test]
fn runtime_timeout_kills_and_fails_closed() {
    let caps = vec![hang_cap("hang", json!({}))];
    let start = std::time::Instant::now();
    let out = open_harness::dispatch(Harness::Claude, &blocking_tool(), &caps, &native());
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "the 30s sleep must be killed at the 150ms timeout, not run to completion"
    );
    assert_eq!(out.response.exit_code, 2, "blocking timeout must deny");
    assert!(
        out.errored.iter().any(|e| e.contains("timeout")),
        "taxonomy must surface the timeout: {:?}",
        out.errored
    );
}

/// The same timeout with a `fail_open` binding yields the policy-driven allow
/// instead — the failure policy is honored.
#[test]
fn runtime_timeout_fail_open_allows() {
    let caps = vec![hang_cap(
        "hang",
        json!({
            "events": [{ "phase": "pre", "subject": "tool", "tool_class": "any", "on_error": "fail_open" }],
        }),
    )];
    let out = open_harness::dispatch(Harness::Claude, &blocking_tool(), &caps, &native());
    assert_eq!(
        out.response.exit_code, 0,
        "fail_open must allow despite the timeout"
    );
    assert!(out.errored.iter().any(|e| e.contains("timeout")));
}

/// Each failure mode carries its stable taxonomy class, surfaced in `errored`.
#[test]
fn runtime_error_taxonomy_classes() {
    let cases = [
        (
            "nonzero",
            "import sys; sys.exit(3)",
            json!({}),
            "non-zero-exit",
        ),
        ("badjson", "print('nope')", json!({}), "bad-json"),
        (
            "proto",
            "print('{\"decision\":\"allow\"}')",
            json!({ "protocol": "hook@2" }),
            "protocol-mismatch",
        ),
    ];
    for (id, script, extra, class) in cases {
        let caps = vec![py_hook(id, script, extra)];
        let out = open_harness::dispatch(Harness::Claude, &blocking_tool(), &caps, &native());
        assert_eq!(out.response.exit_code, 2, "{id} should fail closed");
        assert!(
            out.errored.iter().any(|e| e.contains(class)),
            "{id}: expected class `{class}` in {:?}",
            out.errored
        );
    }
}

/// A missing interpreter is a classified spawn-error, not a panic.
#[test]
fn runtime_spawn_error_is_classified() {
    let caps = vec![cap_from(json!({
        "id": "nobin",
        "run": { "command": "definitely-not-a-real-binary-oh-xyz" },
        "events": [{ "phase": "pre", "subject": "tool", "tool_class": "any" }],
    }))];
    let out = open_harness::dispatch(Harness::Claude, &blocking_tool(), &caps, &native());
    assert_eq!(out.response.exit_code, 2);
    assert!(out.errored.iter().any(|e| e.contains("spawn-error")));
}

/// stdout beyond the cap is an `output-cap` error, not a truncated-JSON parse.
#[test]
fn runtime_output_cap_is_enforced() {
    let caps = vec![py_hook(
        "flood",
        "import sys; sys.stdout.write('x' * 100)",
        json!({}),
    )];
    let limits = RunLimits {
        timeout_ms: 5_000,
        max_output_bytes: 8,
        poll_interval_ms: 5,
    };
    let out = open_harness::dispatch_with_limits(
        Harness::Claude,
        &blocking_tool(),
        &caps,
        &native(),
        &limits,
    );
    assert!(
        out.errored.iter().any(|e| e.contains("output-cap")),
        "expected output-cap in {:?}",
        out.errored
    );
}

/// On a non-blocking event, an errored capability degrades to allow by default
/// (there is nothing to veto) but is still reported.
#[test]
fn runtime_non_blocking_error_degrades_to_allow() {
    let caps = vec![py_hook(
        "post-badjson",
        "print('nope')",
        json!({ "events": [{ "phase": "post", "subject": "tool", "tool_class": "any" }] }),
    )];
    let ev = NormEvent::tool(Phase::Post, ToolClass::Shell);
    let out = open_harness::dispatch(Harness::Claude, &ev, &caps, &native());
    assert_eq!(out.response.exit_code, 0, "post-phase error must not block");
    assert!(out.errored.iter().any(|e| e.contains("bad-json")));
}

/// Independent capabilities run concurrently: two ~500ms sleepers finish well
/// under the ~1s serial sum, and the merge order stays deterministic.
#[test]
fn runtime_runs_concurrently_and_merges_deterministically() {
    let mk = |id: &str, ctx: &str| {
        py_hook(
            id,
            &format!(
                "import time,sys; time.sleep(0.5); sys.stdout.write('{{\"decision\":\"allow\",\"context_append\":\"{ctx}\"}}')"
            ),
            json!({}),
        )
    };
    let caps = vec![mk("a", "A"), mk("b", "B")];
    let start = std::time::Instant::now();
    let out = open_harness::dispatch(Harness::OpenCode, &blocking_tool(), &caps, &native());
    let elapsed = start.elapsed();
    assert_eq!(out.ran.len(), 2);
    assert!(
        elapsed < std::time::Duration::from_millis(900),
        "two 500ms capabilities ran serially ({elapsed:?}); expected concurrent"
    );
    // OpenCode passes the canonical decision through; context is merged in
    // manifest order regardless of which subprocess finished first.
    assert!(out.response.stdout.contains("A\\nB") || out.response.stdout.contains("A\nB"));
}

// ---- cross-platform execution (#4): interpreter resolution, BOM tolerance ----

use open_harness::runtime::resolve_program;

fn run_spec(v: serde_json::Value) -> open_harness::manifest::RunSpec {
    serde_json::from_value(v).expect("valid run spec")
}

/// A script with no interpreter and no shebang is run through the interpreter
/// inferred from its extension — `.py` → python, cross-platform.
#[test]
fn resolve_infers_interpreter_from_extension() {
    let (program, argv) = resolve_program(&run_spec(json!({ "command": "guard.py" })));
    assert!(
        program.to_lowercase().contains("python"),
        "expected a python interpreter, got {program}"
    );
    assert_eq!(argv, vec!["guard.py".to_string()]);
}

/// An explicit `interpreter` runs `command` (the script) through it, ahead of
/// any extension inference.
#[test]
fn resolve_honors_explicit_interpreter() {
    let (program, argv) = resolve_program(&run_spec(
        json!({ "command": "hook.txt", "interpreter": "node" }),
    ));
    assert!(program.to_lowercase().contains("node"), "got {program}");
    assert_eq!(argv, vec!["hook.txt".to_string()]);
}

/// A bare interpreter alias is remapped across platforms (`python3` resolves to
/// the platform python) while its args are preserved.
#[test]
fn resolve_remaps_known_interpreter_alias() {
    let (program, argv) = resolve_program(&run_spec(
        json!({ "command": "python3", "args": ["guard.py"] }),
    ));
    assert!(program.to_lowercase().contains("python"), "got {program}");
    assert_eq!(argv, vec!["guard.py".to_string()]);
}

/// A non-interpreter command (a compiled binary / relative path) passes through
/// untouched — resolution never rewrites it.
#[test]
fn resolve_passes_through_plain_binary() {
    let (program, argv) = resolve_program(&run_spec(
        json!({ "command": "./guard", "args": ["--check"] }),
    ));
    assert_eq!(program, "./guard");
    assert_eq!(argv, vec!["--check".to_string()]);
}

/// A decision emitted with a UTF-8 BOM and CRLF (Windows PowerShell) still
/// parses — the contract is line-ending and BOM tolerant.
#[test]
fn parse_decision_tolerates_bom_and_crlf() {
    let out = "\u{feff}{\"decision\":\"deny\",\"reason\":\"x\"}\r\n";
    let decision = parse_decision(out).expect("BOM/CRLF-wrapped decision must parse");
    assert_eq!(decision.decision, Verdict::Deny);
}

// ---- fixture-driven adapter validation (#5): Codex + Cursor ----------------

/// Load a recorded native payload from `tests/fixtures/`.
fn fixture(rel: &str) -> serde_json::Value {
    let path = format!("tests/fixtures/{rel}");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

/// Cursor's `beforeShellExecution` carries the command as a top-level string —
/// verified, and different from the old `toolName`/`toolInput` assumption.
#[test]
fn cursor_shell_fixture_decodes_to_canonical() {
    let native = fixture("cursor/beforeShellExecution.stdin.json");
    let p = Harness::Cursor.decode(&native, &NormEvent::tool(Phase::Pre, ToolClass::Shell));
    let tool = p.tool.expect("shell event has a tool");
    assert_eq!(tool.name, "shell");
    assert!(tool.input["command"]
        .as_str()
        .unwrap()
        .contains("AKIAIOSFODNN7EXAMPLE"));
    assert_eq!(p.cwd.as_deref(), Some("/Users/me/project"));
}

/// Cursor's read/MCP events each have their own payload shape.
#[test]
fn cursor_read_and_mcp_fixtures_decode_event_specifically() {
    let read = Harness::Cursor.decode(
        &fixture("cursor/beforeReadFile.stdin.json"),
        &NormEvent::tool(Phase::Pre, ToolClass::FileRead),
    );
    let rt = read.tool.unwrap();
    assert_eq!(rt.name, "read");
    assert_eq!(rt.input["file_path"], json!("/Users/me/project/.env"));
    assert!(rt.input["content"].as_str().unwrap().contains("ghp_"));

    let mcp = Harness::Cursor.decode(
        &fixture("cursor/beforeMCPExecution.stdin.json"),
        &NormEvent::tool(Phase::Pre, ToolClass::Mcp),
    );
    let mt = mcp.tool.unwrap();
    assert_eq!(mt.name, "search_web");
    assert_eq!(mt.input["query"], json!("how to rotate an API key"));
}

/// Cursor's deny is a `permission` object with `continue:false` (verified).
#[test]
fn cursor_deny_uses_permission_object() {
    let deny = Decision {
        decision: Verdict::Deny,
        reason: "secret".into(),
        context_append: None,
        modified_input: None,
    };
    let r = Harness::Cursor.encode(&deny, &NormEvent::tool(Phase::Pre, ToolClass::Shell));
    assert_eq!(r.exit_code, 0);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(v["permission"], json!("deny"));
    assert_eq!(v["continue"], json!(false));
    assert_eq!(v["agentMessage"], json!("secret"));
}

/// The Codex correctness fix: a deny is a stdout `permissionDecision` object,
/// NOT exit code 2 as the old adapter assumed.
#[test]
fn codex_denies_via_stdout_permission_decision_not_exit2() {
    let native = fixture("codex/preToolUse.stdin.json");
    let ev = NormEvent::tool(Phase::Pre, ToolClass::Shell);
    let p = Harness::Codex.decode(&native, &ev);
    assert_eq!(p.tool.as_ref().unwrap().name, "shell");

    let deny = Decision {
        decision: Verdict::Deny,
        reason: "blocked secret".into(),
        context_append: None,
        modified_input: None,
    };
    let r = Harness::Codex.encode(&deny, &ev);
    assert_eq!(r.exit_code, 0, "Codex denies via stdout, not exit 2");
    let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(v["permissionDecision"], json!("deny"));
    assert_eq!(v["permissionDecisionReason"], json!("blocked secret"));
}

/// Codex registers via `hooks.json` (JSON), not a TOML `[[hooks…]]` table, and
/// the shell-only + opt-in caveats are surfaced.
#[test]
fn codex_registration_is_hooks_json() {
    let reg =
        Harness::Codex.emit_registration(&[NormEvent::tool(Phase::Pre, ToolClass::Shell)], "guard");
    assert!(reg.contains("hooks.json"), "Codex registers via hooks.json");
    assert!(reg.contains("\"PreToolUse\""));
    assert!(reg.contains("codex_hooks = true"), "notes the opt-in");
    assert!(
        !reg.contains("[[hooks"),
        "not the TOML array-of-tables form"
    );
    assert!(
        reg.contains("shell tool only"),
        "surfaces the shell-only caveat"
    );
}

/// The recorded Cursor shell payload, run through the dispatcher and the real
/// secret-guard capability, is blocked via Cursor's native permission object.
#[test]
fn cursor_shell_fixture_blocks_a_secret_end_to_end() {
    let native = fixture("cursor/beforeShellExecution.stdin.json");
    let out = open_harness::dispatch(
        Harness::Cursor,
        &NormEvent::tool(Phase::Pre, ToolClass::Shell),
        &caps(),
        &native,
    );
    assert!(
        out.response.stdout.contains("\"permission\":\"deny\""),
        "the secret in the recorded Cursor command must be blocked: {}",
        out.response.stdout
    );
}
