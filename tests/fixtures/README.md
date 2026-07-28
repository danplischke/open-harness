# Recorded native hook fixtures (#5)

Real native hook stdin payloads for the two harnesses whose adapters were
`MEDIUM CONFIDENCE`. A live Codex/Cursor install isn't available in this repo, so
these are **doc-derived fixtures** captured from primary documentation rather
than a live capture — the adapter tests in `tests/conformance.rs` decode them and
assert the canonical shape, and encode a deny and assert each harness's real
native response format.

## Cursor

Per-event payloads are **snake_case and event-specific** (not a uniform
`tool_name`/`tool_input`), and the response is a `permission` object.

- `cursor/beforeShellExecution.stdin.json` — `{command, cwd, hook_event_name, …}`
- `cursor/beforeReadFile.stdin.json` — `{file_path, content, hook_event_name, …}`
- `cursor/beforeMCPExecution.stdin.json` — `{tool_name, tool_input, hook_event_name, …}`

Response (stdout): `{ "continue": bool, "permission": "allow|deny|ask", "userMessage": …, "agentMessage": … }`.

Sources: Cursor Hooks documentation & deep-dives —
<https://blog.gitbutler.com/cursor-hooks-deep-dive>,
<https://github.com/johnlindquist/cursor-hooks>.

## Codex

Event names mirror Claude (`PreToolUse`/`PostToolUse`), but:

- the deny signal is a **stdout `permissionDecision` object**, not exit code 2;
- hooks are registered in **`~/.codex/hooks.json`** (JSON), enabled by
  `[features].codex_hooks = true`;
- **`PreToolUse` fires for the shell tool only** — `apply_patch`, `Edit`/`Write`/
  `Read`, web fetch, and MCP calls bypass it.

- `codex/preToolUse.stdin.json` — modeled on the Claude-style `{tool_name,
  tool_input}` schema (the exact upstream field names are the one part not yet
  captured from a live install).

Sources: Codex hooks references —
<https://agenticcontrolplane.com/blog/codex-cli-hooks-reference>,
<https://developers.openai.com/codex/agent-approvals-security>,
<https://deepwiki.com/openai/codex/3.11-hooks-system>.
