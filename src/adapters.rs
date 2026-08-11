//! Per-harness adapters.
//!
//! Each harness disagrees on three things, and this module encodes all three:
//!   1. **Which native event(s)** a normalized event maps to — 1→1 (Claude),
//!      1→many (Cursor/Windsurf fan-out), or 1→0 (no target).
//!   2. **How a capability's `deny` is signaled** — exit code 2, a stdout JSON
//!      `permission`/`cancel` object, or an in-process return via a shim.
//!   3. **How the capability is registered** — JSON, TOML, an executable file,
//!      or a generated plugin/extension.
//!
//! There is a fourth disagreement that shows up at *runtime*: how granular the
//! tool event is. Cursor and Windsurf fire a separate native event per tool
//! class, so a registration's class is the truth. Everyone else fires one event
//! for every tool, so it is only a claim about the binding — several normalized
//! events collapse onto that one native name, and the call itself could be
//! anything. [`Harness::refine`] closes the gap by classifying the reported tool
//! name ([`tool_class_of`]), so the dispatched event describes the call rather
//! than the registration.
//!
//! Confidence: Claude Code, Gemini, Windsurf, and Cline conventions are well
//! documented. Codex and Cursor were validated against primary docs and their
//! adapters corrected (#5) — see `tests/fixtures/{codex,cursor}/` for the
//! recorded payloads and sources. Cursor sends per-event snake_case payloads
//! (not a uniform `tool_name`/`tool_input`) and reads a `permission` object;
//! Codex signals a deny via a stdout `permissionDecision` object (not exit 2),
//! loads `hooks.json`, and fires PreToolUse for the shell tool only. The one
//! remaining model (not a verified capture) is Codex's exact stdin field names,
//! taken to mirror Claude's `tool_name`/`tool_input`.

use crate::event::{Boundary, NormEvent, Phase, SubjectKind, TaskKind, ToolClass};
use crate::model::{CanonicalPayload, Decision, NativeResponse, ToolInfo, Verdict, PROTOCOL};
use serde_json::{json, Value};

/// How a normalized event resolves onto a harness's native hook surface.
#[derive(Debug, Clone)]
pub enum Support {
    /// Exactly one native event. Carries the concrete normalized event to pass
    /// to the dispatcher at runtime.
    Native(&'static str, NormEvent),
    /// One normalized event that must register on several native events.
    Fanout(Vec<(&'static str, NormEvent)>),
    /// No native target. Carries the reason for the matrix / install warning.
    Unsupported(&'static str),
}

/// How this harness reads a capability's decision back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyStyle {
    /// Non-zero exit blocks; reason goes to stderr. (Claude, Gemini, Windsurf.)
    Exit2,
    /// stdout JSON `{"permission":"deny", ...}`. (Cursor.)
    CursorJson,
    /// stdout JSON `{"permissionDecision":"deny","permissionDecisionReason":...}`.
    /// (Codex — verified against docs; it is *not* exit-2 like Claude.)
    CodexJson,
    /// stdout JSON `{"cancel":true, ...}`. (Cline.)
    ClineJson,
    /// In-process: the generated shim reads the canonical decision and applies
    /// it via the harness's plugin API. (OpenCode, Pi.)
    InProcess,
}

/// How an adapter's encoding of a harness was established.
///
/// "Supported" and "verified" are different claims, and the support matrix used
/// to print only the first — every cell looked equally load-bearing whether it
/// came from a recorded payload or from reading a vendor's docs. For a tool that
/// asks you to route your agent's security hooks through it, that difference is
/// the credibility, so it is now a first-class part of the report.
///
/// Each variant carries what it was established against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// A payload recorded from a **live install** by `oh capture`, committed as
    /// a fixture the conformance suite decodes. The strongest claim available.
    LiveCaptured(&'static str),
    /// Encoded from the vendor's primary documentation, **with** a fixture built
    /// from those docs that the conformance suite decodes. The shape is pinned
    /// and regression-tested; what is unproven is that the vendor really sends
    /// it.
    DocFixture(&'static str),
    /// Encoded from the vendor's documentation, with no recorded payload at all.
    DocOnly(&'static str),
}

impl Provenance {
    /// A short, sortable label for the matrix.
    pub fn label(&self) -> &'static str {
        match self {
            Provenance::LiveCaptured(_) => "live-captured",
            Provenance::DocFixture(_) => "doc-fixture",
            Provenance::DocOnly(_) => "doc-only",
        }
    }

    /// What the adapter was established against.
    pub fn source(&self) -> &'static str {
        match self {
            Provenance::LiveCaptured(s) | Provenance::DocFixture(s) | Provenance::DocOnly(s) => s,
        }
    }

    /// Whether a recorded payload backs this adapter — the thing the fixture
    /// corpus on disk can be checked against.
    pub fn has_fixture(&self) -> bool {
        matches!(
            self,
            Provenance::LiveCaptured(_) | Provenance::DocFixture(_)
        )
    }

    /// One line explaining what the reader should take from it.
    pub fn caveat(&self) -> &'static str {
        match self {
            Provenance::LiveCaptured(_) => "recorded from a live install",
            Provenance::DocFixture(_) => {
                "fixture built from primary docs, not recorded from a live install"
            }
            Provenance::DocOnly(_) => {
                "documented only — no recorded payload; verify before relying on it"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Gemini,
    Cursor,
    Windsurf,
    Cline,
    OpenCode,
    Pi,
    Aider,
    Copilot,
    /// Google Antigravity — skills / workflows / instructions targets, no hooks
    /// or permissions. Path research from primary docs (see the skill/command/
    /// instructions kinds); the `.agents/` layout is Antigravity 2.0 and may
    /// still shift, so it carries a documented-not-captured caveat.
    Antigravity,
}

pub const ALL: [Harness; 11] = [
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
    Harness::Antigravity,
];

impl Harness {
    pub fn from_id(s: &str) -> Option<Harness> {
        Some(match s {
            "claude-code" | "claude" => Harness::Claude,
            "codex" => Harness::Codex,
            "gemini" | "gemini-cli" => Harness::Gemini,
            "cursor" => Harness::Cursor,
            "windsurf" => Harness::Windsurf,
            "cline" => Harness::Cline,
            "opencode" => Harness::OpenCode,
            "pi" => Harness::Pi,
            "aider" => Harness::Aider,
            "copilot" => Harness::Copilot,
            "antigravity" => Harness::Antigravity,
            _ => return None,
        })
    }

    pub fn id(&self) -> &'static str {
        match self {
            Harness::Claude => "claude-code",
            Harness::Codex => "codex",
            Harness::Gemini => "gemini",
            Harness::Cursor => "cursor",
            Harness::Windsurf => "windsurf",
            Harness::Cline => "cline",
            Harness::OpenCode => "opencode",
            Harness::Pi => "pi",
            Harness::Aider => "aider",
            Harness::Copilot => "copilot",
            Harness::Antigravity => "antigravity",
        }
    }

    pub fn in_process(&self) -> bool {
        matches!(self, Harness::OpenCode | Harness::Pi)
    }

    /// A caveat about running on this harness at all, for a capability bound to
    /// `events`.
    ///
    /// Scoped to the events because not every caveat applies to every binding:
    /// Codex's is about which *tool* calls reach `PreToolUse`, and attaching it
    /// to a session-teardown capability tells the reader something true about
    /// the harness but false about their capability. Cline's platform limit and
    /// Antigravity's unstable layout are properties of the harness itself, so
    /// they always apply.
    pub fn platform_note(&self, events: &[NormEvent]) -> Option<&'static str> {
        let binds_tool = events.iter().any(|e| e.subject == SubjectKind::Tool);
        match self {
            Harness::Cline => Some("hooks are macOS/Linux only (no Windows support)"),
            // Verified: Codex's PreToolUse fires for the shell (Bash) tool only;
            // apply_patch/Edit/Write/Read/web-fetch/MCP calls bypass it. Hooks are
            // also off by default (`[features].codex_hooks = true` to enable).
            Harness::Codex if binds_tool => Some(
                "PreToolUse fires for the shell tool only (file/MCP/web tools bypass it); enable with [features].codex_hooks = true",
            ),
            Harness::Antigravity => Some(
                "the `.agents/` layout is Antigravity 2.0 and may still shift; GEMINI.md may be transitional — re-verify before an Antigravity-facing release",
            ),
            _ => None,
        }
    }

    /// How this harness's adapter was established, and against what.
    ///
    /// Declared here rather than inferred so the claim is reviewable in one
    /// place — and cross-checked against the fixture corpus on disk by
    /// `tests/provenance.rs`, so it cannot overstate what has actually been
    /// recorded. Upgrading a harness means running `oh capture` against a real
    /// install, committing the fixture, and changing the arm below; the test
    /// fails if you do either half without the other.
    pub fn provenance(&self) -> Provenance {
        match self {
            // Fixtures recorded from primary documentation (#5): the conformance
            // suite decodes them, but no live install produced them.
            Harness::Codex => Provenance::DocFixture(
                "Codex hooks references (agenticcontrolplane.com, developers.openai.com, deepwiki); \
                 the exact stdin field names are modeled on Claude's and are the one part not recorded",
            ),
            Harness::Cursor => Provenance::DocFixture(
                "Cursor Hooks documentation and deep-dives (blog.gitbutler.com, johnlindquist/cursor-hooks)",
            ),
            // Conventions are well documented and stable, but nothing here has
            // been round-tripped against the real thing.
            Harness::Claude => Provenance::DocOnly("Claude Code hooks + settings.json documentation"),
            Harness::Gemini => Provenance::DocOnly("Gemini CLI hooks documentation"),
            Harness::Windsurf => Provenance::DocOnly("Windsurf rules + hooks documentation"),
            Harness::Cline => Provenance::DocOnly("Cline hooks + .clinerules documentation"),
            Harness::OpenCode => Provenance::DocOnly("OpenCode plugin API documentation"),
            Harness::Pi => Provenance::DocOnly("Pi plugin API documentation"),
            Harness::Aider => Provenance::DocOnly("Aider conventions documentation"),
            Harness::Copilot => Provenance::DocOnly(
                "GitHub Copilot custom instructions + .github/instructions documentation",
            ),
            Harness::Antigravity => Provenance::DocOnly(
                "Antigravity 2.0 .agents/ layout, from primary docs; the layout may still shift",
            ),
        }
    }

    pub fn deny_style(&self) -> DenyStyle {
        match self {
            Harness::Claude | Harness::Gemini | Harness::Windsurf => DenyStyle::Exit2,
            // Codex signals via a stdout `permissionDecision` object, not exit 2.
            Harness::Codex => DenyStyle::CodexJson,
            Harness::Cursor => DenyStyle::CursorJson,
            Harness::Cline => DenyStyle::ClineJson,
            Harness::OpenCode | Harness::Pi => DenyStyle::InProcess,
            // Aider/Copilot/Antigravity have no hooks; deny is meaningless but
            // keep the mapping total.
            Harness::Aider | Harness::Copilot | Harness::Antigravity => DenyStyle::Exit2,
        }
    }

    /// THE core mapping table. Given a normalized event, what native surface
    /// does it land on for this harness?
    pub fn support(&self, ev: &NormEvent) -> Support {
        use Harness::*;
        use SubjectKind::*;
        let pre = ev.phase == Phase::Pre;
        match self {
            Claude | Codex => match ev.subject {
                Tool => single(if pre { "PreToolUse" } else { "PostToolUse" }, *ev),
                Prompt if pre => single("UserPromptSubmit", *ev),
                Session => match ev.boundary {
                    Some(Boundary::Start) => single("SessionStart", *ev),
                    Some(Boundary::End) => single("SessionEnd", *ev),
                    None => Support::Unsupported("session event needs a start/end boundary"),
                },
                Subagent => match ev.boundary {
                    Some(Boundary::Start) => single("SubagentStart", *ev),
                    Some(Boundary::End) => single("SubagentStop", *ev),
                    None => Support::Unsupported("subagent event needs a boundary"),
                },
                Model => Support::Unsupported("no model-phase hook (only Gemini exposes one)"),
                Prompt => Support::Unsupported("only the pre (submit) phase exists"),
                Task => Support::Unsupported("no task lifecycle events"),
            },
            Gemini => match ev.subject {
                Tool => single(if pre { "BeforeTool" } else { "AfterTool" }, *ev),
                Model => single(if pre { "BeforeModel" } else { "AfterModel" }, *ev),
                Prompt if pre => single("BeforeAgent", *ev), // approximate: agent-turn boundary
                Session => match ev.boundary {
                    Some(Boundary::Start) => single("SessionStart", *ev),
                    Some(Boundary::End) => single("SessionEnd", *ev),
                    None => Support::Unsupported("session event needs a boundary"),
                },
                Prompt => Support::Unsupported("only the pre phase maps"),
                Subagent => Support::Unsupported("no subagent hook"),
                Task => Support::Unsupported("no task lifecycle events"),
            },
            Cursor => match ev.subject {
                // Cursor splits "before a tool" into per-class events -> fan-out.
                Tool => cursor_tool(ev),
                Session if ev.boundary == Some(Boundary::Start) => single("sessionStart", *ev),
                Prompt => Support::Unsupported("no documented prompt-submit hook"),
                Session => Support::Unsupported("only sessionStart is exposed"),
                Model => Support::Unsupported("no model hook"),
                Subagent => Support::Unsupported("no subagent hook"),
                Task => Support::Unsupported("no task lifecycle events"),
            },
            Windsurf => match ev.subject {
                Tool => windsurf_tool(ev),
                Prompt if pre => single("pre_user_prompt", *ev),
                Prompt => single("post_user_prompt", *ev),
                Session => Support::Unsupported("hooks are tool/prompt-scoped, no session event"),
                Model => Support::Unsupported("no model hook"),
                Subagent => Support::Unsupported("no subagent hook"),
                Task => Support::Unsupported("no task lifecycle events"),
            },
            Cline => match ev.subject {
                Tool => single(if pre { "PreToolUse" } else { "PostToolUse" }, *ev),
                Prompt if pre => single("UserPromptSubmit", *ev),
                Task => match ev.task_kind {
                    Some(TaskKind::Start) => single("TaskStart", *ev),
                    Some(TaskKind::Resume) => single("TaskResume", *ev),
                    Some(TaskKind::Cancel) => single("TaskCancel", *ev),
                    None => Support::Unsupported("task event needs a kind"),
                },
                Session => Support::Unsupported("uses Task* lifecycle instead of Session*"),
                Model => Support::Unsupported("no model hook"),
                Subagent => Support::Unsupported("no subagent hook"),
                Prompt => Support::Unsupported("only the pre phase maps"),
            },
            OpenCode => match ev.subject {
                Tool => single(
                    if pre { "tool.execute.before" } else { "tool.execute.after" },
                    *ev,
                ),
                Session => single("session.*", *ev),
                Prompt => single("message.updated", *ev), // approximate
                Model => Support::Unsupported("no model hook in the plugin API"),
                Subagent => Support::Unsupported("no subagent hook"),
                Task => Support::Unsupported("no task lifecycle events"),
            },
            // Pi's core intentionally exposes almost nothing; everything else is DIY.
            Pi => match ev.subject {
                Tool => single("tool_call", *ev),
                _ => Support::Unsupported(
                    "Pi core exposes only tool_call; other lifecycle is build-your-own via extensions",
                ),
            },
            Aider => Support::Unsupported("Aider has no hook mechanism"),
            Copilot => Support::Unsupported("Copilot has no general-purpose hook mechanism"),
            Antigravity => Support::Unsupported("Antigravity has no known hook mechanism"),
        }
    }

    /// Narrow a registered event to the class of the call that actually arrived.
    ///
    /// Most harnesses expose **one** tool event (`PreToolUse` and friends) that
    /// fires for every tool, so the class in a registration is a claim about the
    /// binding, not about the call. Left unrefined that claim reaches the
    /// capability as fact: a `pre.tool.shell` registration on Claude hands a
    /// guard `"tool_class":"shell"` for a `Read`, and every class-scoped binding
    /// fires on every tool. Classifying the reported tool name fixes both — the
    /// payload describes the real call, and `pre.tool.shell` bindings run on
    /// shell calls.
    ///
    /// Only ever *replaces* the class, never invents one: a harness whose tool
    /// names we cannot classify (or an event carrying no tool) keeps the
    /// registered class, which is exactly the previous behavior.
    pub fn refine(&self, native: &Value, ev: &NormEvent) -> NormEvent {
        if ev.subject != SubjectKind::Tool {
            return *ev;
        }
        let name = self.tool_name(native);
        match tool_class_of(*self, &name) {
            Some(class) => NormEvent {
                tool_class: Some(class),
                ..*ev
            },
            None => *ev,
        }
    }

    /// The tool name this harness reports, in its own spelling.
    fn tool_name(&self, native: &Value) -> String {
        match self {
            // Windsurf nests the tool under `tool: { name, args }`.
            Harness::Windsurf => native
                .get("tool")
                .and_then(|t| t.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            // Cursor's per-class payloads mostly carry no name at all; the ones
            // that do (beforeMCPExecution) use `tool_name` like everyone else.
            _ => str_at(native, "tool_name"),
        }
    }

    /// Decode this harness's native stdin payload into the canonical shape. The
    /// field-name divergence lives here; the crux divergence (deny encoding)
    /// lives in `encode`.
    ///
    /// `ev` should already be [`refine`](Self::refine)d — [`crate::dispatch`]
    /// does that — so the `event` this puts in the payload describes the call.
    pub fn decode(&self, native: &Value, ev: &NormEvent) -> CanonicalPayload {
        let tool = match ev.subject {
            SubjectKind::Tool => Some(match self {
                // Cursor: per-event payloads, snake_case — verified against docs.
                // beforeShellExecution: {command, cwd, …}; beforeMCPExecution:
                // {tool_name, tool_input, …}; beforeReadFile: {file_path, content}.
                Harness::Cursor => decode_cursor_tool(native, ev),
                // Windsurf nests the tool under `tool: { name, args }`.
                Harness::Windsurf => ToolInfo {
                    name: native
                        .get("tool")
                        .and_then(|t| t.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    input: native
                        .get("tool")
                        .and_then(|t| t.get("args"))
                        .cloned()
                        .unwrap_or(Value::Null),
                },
                // Claude / Codex / Gemini / Cline share the `tool_name`/`tool_input`
                // convention. (Codex PreToolUse carries the shell tool only.)
                _ => ToolInfo {
                    name: str_at(native, "tool_name"),
                    input: native.get("tool_input").cloned().unwrap_or(Value::Null),
                },
            }),
            _ => None,
        };
        let prompt = native
            .get("prompt")
            .or_else(|| native.get("user_prompt"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        // Session identity, under the two spellings that appear in the payloads
        // we have: `session_id` (Claude Code, and Codex which mirrors it) and
        // `conversation_id` (Cursor — see tests/fixtures/cursor/). Read by name
        // rather than per-harness so an adapter we cannot verify still surfaces
        // an identity it happens to send; absent stays `None` rather than being
        // invented.
        let session = native
            .get("session_id")
            .or_else(|| native.get("conversation_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        // Falling back to the dispatcher's cwd: the harness invoked us from the
        // project, so this is the project root everywhere the hook is a spawned
        // process. It is still `Option` because an in-process shim host can be
        // elsewhere, and a confidently wrong project root is worse than none.
        let cwd = native
            .get("cwd")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
            });
        CanonicalPayload {
            protocol: PROTOCOL,
            harness: self.id().to_string(),
            event: *ev,
            blocking: ev.blocking(),
            tool,
            prompt,
            session,
            cwd,
            raw: native.clone(),
        }
    }

    /// Encode a merged decision into what this harness's hook runner expects.
    /// This is where the deny-signal fragmentation is made concrete.
    pub fn encode(&self, d: &Decision, ev: &NormEvent) -> NativeResponse {
        let denied = d.decision == Verdict::Deny && ev.blocking();
        match self.deny_style() {
            DenyStyle::Exit2 => {
                if denied {
                    NativeResponse {
                        stdout: String::new(),
                        stderr: reason_or(d, "blocked by open-harness capability"),
                        exit_code: 2,
                    }
                } else {
                    // allow / non-blocking / modify(degraded) — stdout is the
                    // context channel where the harness reads it.
                    NativeResponse {
                        stdout: d.context_append.clone().unwrap_or_default(),
                        stderr: String::new(),
                        exit_code: 0,
                    }
                }
            }
            DenyStyle::CursorJson => {
                // Verified response shape: {continue, permission, userMessage,
                // agentMessage}. permission ∈ allow|deny|ask.
                let v = if denied {
                    json!({"continue": false, "permission": "deny", "agentMessage": reason_or(d, "blocked")})
                } else {
                    json!({"continue": true, "permission": "allow"})
                };
                NativeResponse {
                    stdout: v.to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                }
            }
            DenyStyle::CodexJson => {
                // Verified: Codex reads a stdout `permissionDecision` object; a
                // deny becomes a tool error the agent sees.
                let v = if denied {
                    json!({
                        "permissionDecision": "deny",
                        "permissionDecisionReason": reason_or(d, "blocked"),
                    })
                } else {
                    json!({ "permissionDecision": "allow" })
                };
                NativeResponse {
                    stdout: v.to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                }
            }
            DenyStyle::ClineJson => {
                let v = if denied {
                    json!({"cancel": true, "reason": reason_or(d, "blocked")})
                } else if let Some(c) = &d.context_append {
                    json!({ "contextModification": c })
                } else {
                    json!({})
                };
                NativeResponse {
                    stdout: v.to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                }
            }
            DenyStyle::InProcess => {
                // The generated shim reads the canonical decision verbatim and
                // applies it through the plugin API.
                NativeResponse {
                    stdout: serde_json::to_string(d).unwrap_or_else(|_| "{}".into()),
                    stderr: String::new(),
                    exit_code: 0,
                }
            }
        }
    }

    /// Produce the native registration config that wires the dispatcher in as a
    /// SINGLE entrypoint per (native) event. Unsupported bindings are emitted as
    /// warning comments so "where it breaks" is visible in the output.
    pub fn emit_registration(&self, events: &[NormEvent], capability_id: &str) -> String {
        let mut targets: Vec<(String, NormEvent)> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        for ev in events {
            match self.support(ev) {
                Support::Native(name, cev) => targets.push((name.to_string(), cev)),
                Support::Fanout(list) => {
                    for (name, cev) in list {
                        targets.push((name.to_string(), cev));
                    }
                }
                Support::Unsupported(reason) => {
                    warnings.push(format!("{}  ->  UNSUPPORTED: {reason}", ev.id()))
                }
            }
        }
        let collapsed = collapse_targets(&mut targets);
        for (name, from, to) in &collapsed {
            warnings.push(format!(
                "{} share the one native `{name}` event on this harness, which fires for every \
                 tool — registered once as `{to}`; the dispatcher narrows it to the real tool \
                 class per call, so each binding still runs on its own calls",
                from.join(" + ")
            ));
        }
        let cmd = |cev: &NormEvent| format!("oh run --harness {} --event {}", self.id(), cev.id());
        let mut out = String::new();
        out.push_str(&format!(
            "# open-harness registration for capability `{capability_id}` on {}\n",
            self.id()
        ));
        if let Some(note) = self.platform_note(events) {
            out.push_str(&format!("# NOTE: {note}\n"));
        }
        for w in &warnings {
            out.push_str(&format!("# WARNING: {w}\n"));
        }

        match self {
            Harness::Claude | Harness::Gemini => {
                let mut hooks = serde_json::Map::new();
                for (name, cev) in &targets {
                    hooks.insert(
                        name.clone(),
                        json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": cmd(cev) }] }]),
                    );
                }
                let v = json!({ "hooks": Value::Object(hooks) });
                out.push_str(&format!("# file: settings.json\n{}\n", pretty(&v)));
            }
            Harness::Codex => {
                // Verified: Codex loads `~/.codex/hooks.json` (JSON) — an event →
                // array-of-commands map — or an equivalent inline `[hooks]` table.
                // Enable with `[features].codex_hooks = true`.
                let mut hooks = serde_json::Map::new();
                for (name, cev) in &targets {
                    hooks.insert(name.clone(), json!([{ "command": cmd(cev) }]));
                }
                let v = json!({ "hooks": Value::Object(hooks) });
                out.push_str(
                    "# enable first: [features].codex_hooks = true in ~/.codex/config.toml\n",
                );
                out.push_str(&format!("# file: ~/.codex/hooks.json\n{}\n", pretty(&v)));
            }
            Harness::Cursor | Harness::Windsurf => {
                let mut hooks = serde_json::Map::new();
                for (name, cev) in &targets {
                    hooks.insert(name.clone(), json!([{ "command": cmd(cev) }]));
                }
                let v = if *self == Harness::Cursor {
                    json!({ "version": 1, "hooks": Value::Object(hooks) })
                } else {
                    json!({ "hooks": Value::Object(hooks) })
                };
                out.push_str(&format!("# file: hooks.json\n{}\n", pretty(&v)));
            }
            Harness::Cline => {
                for (name, cev) in &targets {
                    out.push_str(&format!(
                        "# file: .clinerules/hooks/{name}  (chmod +x)\n#!/usr/bin/env sh\nexec {}\n\n",
                        cmd(cev)
                    ));
                }
            }
            Harness::OpenCode => {
                out.push_str("# file: .opencode/plugins/open-harness.mjs\n");
                out.push_str(&opencode_shim(&targets, self.id()));
            }
            Harness::Pi => {
                out.push_str("# file: .pi/extensions/open-harness.ts\n");
                out.push_str(&pi_shim(&targets, self.id()));
            }
            Harness::Aider | Harness::Copilot | Harness::Antigravity => {
                out.push_str("# no hook mechanism on this harness — nothing to register\n");
            }
        }
        out
    }
}

/// Collapse targets that land on the **same native event**, in place.
///
/// The registration maps native event → command, so two normalized events that
/// resolve to one native name (Claude sends both `pre.tool.any` and
/// `pre.tool.shell` to `PreToolUse`) used to overwrite each other and one
/// binding vanished with no warning. They are one native event, so they get one
/// command, at the widest class the event actually fires for —
/// [`Harness::refine`] narrows it per call, so both bindings still run on the
/// calls they asked for. Returns `(native name, collapsed ids, kept id)` per
/// collapse so the caller can report it.
fn collapse_targets(targets: &mut Vec<(String, NormEvent)>) -> Vec<(String, Vec<String>, String)> {
    let mut notes = Vec::new();
    let mut seen: Vec<(String, NormEvent)> = Vec::new();
    for (name, ev) in targets.iter() {
        match seen.iter_mut().find(|(n, _)| n == name) {
            None => seen.push((name.clone(), *ev)),
            Some((_, kept)) if *kept == *ev => {}
            Some((_, kept)) => {
                // Same native event, different normalized class: widen to the
                // class that native event really covers.
                let widened = if kept.subject == SubjectKind::Tool && kept.phase == ev.phase {
                    NormEvent::tool(kept.phase, ToolClass::Any)
                } else {
                    *kept
                };
                notes.push((name.clone(), vec![kept.id(), ev.id()], widened.id()));
                *kept = widened;
            }
        }
    }
    *targets = seen;
    notes
}

/// Classify a harness's tool by the **name it reports**, so a coarse native
/// event can be narrowed to the call that actually happened (see
/// [`Harness::refine`]).
///
/// `None` means "not classifiable here", and is the safe answer: the registered
/// class stands, so nothing that fired before stops firing because a name is
/// missing from a table. Confidence matches the rest of this module — Claude's
/// and OpenCode's names are exercised by the test suite; Gemini's and Cline's
/// are documentation-derived; Codex only ever fires this event for its shell
/// tool. Cursor and Windsurf need no table: both split "before a tool" into
/// per-class native events, so their registered class is already the truth.
pub fn tool_class_of(harness: Harness, tool_name: &str) -> Option<ToolClass> {
    if tool_name.is_empty() {
        return None;
    }
    // The `mcp__<server>__<tool>` convention is shared by the harnesses that
    // namespace MCP tools into the ordinary tool surface.
    if tool_name.starts_with("mcp__") {
        return Some(ToolClass::Mcp);
    }
    match harness {
        Harness::Claude => match tool_name {
            "Bash" | "BashOutput" | "KillShell" => Some(ToolClass::Shell),
            "Read" | "Glob" | "Grep" | "NotebookRead" => Some(ToolClass::FileRead),
            "Write" => Some(ToolClass::FileWrite),
            "Edit" | "MultiEdit" | "NotebookEdit" => Some(ToolClass::FileEdit),
            "WebFetch" | "WebSearch" => Some(ToolClass::Web),
            _ => None,
        },
        // Codex fires PreToolUse for the shell tool only, so the name is a
        // confirmation rather than a discriminator.
        Harness::Codex => match tool_name {
            "shell" | "local_shell" | "container.exec" => Some(ToolClass::Shell),
            _ => None,
        },
        Harness::Gemini => match tool_name {
            "run_shell_command" => Some(ToolClass::Shell),
            "read_file" | "read_many_files" | "glob" | "search_file_content" => {
                Some(ToolClass::FileRead)
            }
            "write_file" => Some(ToolClass::FileWrite),
            "replace" => Some(ToolClass::FileEdit),
            "web_fetch" | "google_web_search" => Some(ToolClass::Web),
            _ => None,
        },
        Harness::Cline => match tool_name {
            "execute_command" => Some(ToolClass::Shell),
            "read_file" | "search_files" | "list_files" | "list_code_definition_names" => {
                Some(ToolClass::FileRead)
            }
            "write_to_file" => Some(ToolClass::FileWrite),
            "replace_in_file" => Some(ToolClass::FileEdit),
            "use_mcp_tool" | "access_mcp_resource" => Some(ToolClass::Mcp),
            _ => None,
        },
        Harness::OpenCode => match tool_name {
            "bash" => Some(ToolClass::Shell),
            "read" | "grep" | "glob" | "list" => Some(ToolClass::FileRead),
            "write" => Some(ToolClass::FileWrite),
            "edit" | "patch" => Some(ToolClass::FileEdit),
            "webfetch" => Some(ToolClass::Web),
            _ => None,
        },
        // Per-class native events already carry the truth (Cursor, Windsurf), or
        // there is no documented tool vocabulary to key on (Pi), or no hooks at
        // all (Aider, Copilot, Antigravity).
        _ => None,
    }
}

fn single(name: &'static str, ev: NormEvent) -> Support {
    Support::Native(name, ev)
}

/// Decode Cursor's per-event tool payload into the canonical `ToolInfo`. Cursor
/// does not send a uniform `{tool_name, tool_input}` — each event has its own
/// shape (verified against docs), so normalize by the event's tool class.
fn decode_cursor_tool(native: &Value, ev: &NormEvent) -> ToolInfo {
    match ev.tool_class {
        // beforeShellExecution: the command is a top-level string.
        Some(ToolClass::Shell) => ToolInfo {
            name: "shell".to_string(),
            input: json!({ "command": str_at(native, "command") }),
        },
        // beforeReadFile: file_path (+ content, so a redactor can inspect it).
        Some(ToolClass::FileRead) => ToolInfo {
            name: "read".to_string(),
            input: json!({
                "file_path": str_at(native, "file_path"),
                "content": native.get("content").cloned().unwrap_or(Value::Null),
            }),
        },
        // afterFileEdit: file_path + edits[{old_string,new_string}].
        Some(ToolClass::FileEdit) | Some(ToolClass::FileWrite) => ToolInfo {
            name: "edit".to_string(),
            input: json!({
                "file_path": str_at(native, "file_path"),
                "edits": native.get("edits").cloned().unwrap_or(Value::Null),
            }),
        },
        // beforeMCPExecution: a real tool_name/tool_input pair (snake_case).
        Some(ToolClass::Mcp) => ToolInfo {
            name: str_at(native, "tool_name"),
            input: native.get("tool_input").cloned().unwrap_or(Value::Null),
        },
        // Generic/unknown: best-effort over the union of known fields.
        _ => ToolInfo {
            name: if native.get("tool_name").is_some() {
                str_at(native, "tool_name")
            } else {
                str_at(native, "hook_event_name")
            },
            input: native
                .get("tool_input")
                .or_else(|| native.get("command"))
                .cloned()
                .unwrap_or(Value::Null),
        },
    }
}

fn cursor_tool(ev: &NormEvent) -> Support {
    let pre = ev.phase == Phase::Pre;
    match (pre, ev.tool_class) {
        (true, Some(ToolClass::Shell)) => single("beforeShellExecution", *ev),
        (true, Some(ToolClass::Mcp)) => single("beforeMCPExecution", *ev),
        (true, Some(ToolClass::FileRead)) => single("beforeReadFile", *ev),
        (false, Some(ToolClass::FileEdit)) | (false, Some(ToolClass::FileWrite)) => {
            single("afterFileEdit", *ev)
        }
        // "any tool, pre" must register on every pre-tool event Cursor exposes.
        (true, Some(ToolClass::Any)) | (true, None) => Support::Fanout(vec![
            (
                "beforeShellExecution",
                NormEvent::tool(Phase::Pre, ToolClass::Shell),
            ),
            (
                "beforeMCPExecution",
                NormEvent::tool(Phase::Pre, ToolClass::Mcp),
            ),
            (
                "beforeReadFile",
                NormEvent::tool(Phase::Pre, ToolClass::FileRead),
            ),
        ]),
        _ => Support::Unsupported("no matching per-class tool event in Cursor"),
    }
}

fn windsurf_tool(ev: &NormEvent) -> Support {
    let pre = ev.phase == Phase::Pre;
    let p = |n: &'static str, c: ToolClass| single(n, NormEvent::tool(ev.phase, c));
    match (pre, ev.tool_class) {
        (true, Some(ToolClass::Shell)) => p("pre_run_command", ToolClass::Shell),
        (true, Some(ToolClass::FileWrite)) => p("pre_write_code", ToolClass::FileWrite),
        (true, Some(ToolClass::FileRead)) => p("pre_read_code", ToolClass::FileRead),
        (true, Some(ToolClass::Mcp)) => p("pre_mcp_tool_use", ToolClass::Mcp),
        (true, Some(ToolClass::Any)) | (true, None) => Support::Fanout(vec![
            (
                "pre_run_command",
                NormEvent::tool(Phase::Pre, ToolClass::Shell),
            ),
            (
                "pre_write_code",
                NormEvent::tool(Phase::Pre, ToolClass::FileWrite),
            ),
            (
                "pre_read_code",
                NormEvent::tool(Phase::Pre, ToolClass::FileRead),
            ),
            (
                "pre_mcp_tool_use",
                NormEvent::tool(Phase::Pre, ToolClass::Mcp),
            ),
        ]),
        (false, Some(ToolClass::Shell)) => p("post_run_command", ToolClass::Shell),
        _ => Support::Unsupported("no matching per-class tool event in Windsurf"),
    }
}

fn opencode_shim(targets: &[(String, NormEvent)], harness_id: &str) -> String {
    let map: Vec<String> = targets
        .iter()
        .map(|(n, e)| format!("  \"{n}\": \"{}\",", e.id()))
        .collect();
    format!(
        "// Auto-generated in-process shim. Spawns the language-agnostic capability\n\
         // over stdio and applies its canonical decision through OpenCode's plugin API.\n\
         import {{ spawnSync }} from \"node:child_process\";\n\
         const EVENTS = {{\n{}\n}};\n\
         export const OpenHarness = async () => ({{\n\
         \x20 \"tool.execute.before\": async (input, output) => {{\n\
         \x20   const ev = EVENTS[\"tool.execute.before\"];\n\
         \x20   const payload = JSON.stringify({{ tool_name: input.tool, tool_input: input.args }});\n\
         \x20   const r = spawnSync(\"oh\", [\"run\",\"--harness\",\"{harness_id}\",\"--event\",ev], {{ input: payload }});\n\
         \x20   const decision = JSON.parse(r.stdout.toString() || '{{}}');\n\
         \x20   if (decision.decision === \"deny\") throw new Error(decision.reason || \"blocked by open-harness\");\n\
         \x20 }},\n\
         }});\n",
        map.join("\n")
    )
}

fn pi_shim(targets: &[(String, NormEvent)], harness_id: &str) -> String {
    let ev = targets
        .iter()
        .find(|(n, _)| *n == "tool_call")
        .map(|(_, e)| e.id())
        .unwrap_or_else(|| "pre.tool.any".to_string());
    format!(
        "// Auto-generated Pi extension shim.\n\
         import {{ spawnSync }} from \"node:child_process\";\n\
         export default function (pi: any) {{\n\
         \x20 pi.on(\"tool_call\", async (event: any) => {{\n\
         \x20   const payload = JSON.stringify({{ tool_name: event.name, tool_input: event.args }});\n\
         \x20   const r = spawnSync(\"oh\", [\"run\",\"--harness\",\"{harness_id}\",\"--event\",\"{ev}\"], {{ input: payload }});\n\
         \x20   const decision = JSON.parse(r.stdout.toString() || \"{{}}\");\n\
         \x20   if (decision.decision === \"deny\") return {{ deny: true, reason: decision.reason }};\n\
         \x20 }});\n\
         }}\n"
    )
}

fn str_at(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn reason_or(d: &Decision, fallback: &str) -> String {
    if d.reason.is_empty() {
        fallback.to_string()
    } else {
        d.reason.clone()
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}
