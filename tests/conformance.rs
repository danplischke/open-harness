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
use serde_json::json;

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

    let cursor = Harness::Cursor.decode(
        &json!({"toolName": "Bash", "toolInput": {"command": "ls"}}),
        &ev,
    );
    assert_eq!(cursor.tool.unwrap().name, "Bash");

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
    assert!(plan
        .notes
        .iter()
        .any(|n| n.contains("#19") && n.contains("still runs")));
    assert!(matches!(
        plan.artifacts.first(),
        Some(Artifact::File { .. })
    ));
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

// ---- the stable embeddable API (the surface bindings call) ----------------

use open_harness::api;

#[test]
fn api_lists_harnesses_and_kinds() {
    let hs = api::harnesses();
    assert_eq!(hs.len(), 10);
    assert!(hs.contains(&"claude-code".to_string()));
    let ks = api::kinds();
    assert!(
        ks.contains(&"hook".to_string())
            && ks.contains(&"tool".to_string())
            && ks.contains(&"command".to_string())
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
    assert_eq!(api::plan_all(hook, ".").unwrap().len(), 10);
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
