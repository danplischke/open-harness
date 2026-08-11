# Harness support matrix

> Generated from the adapters by `oh matrix --markdown` (`docs/gen-matrix.sh`) — do not edit by hand.

| event \ harness | claude-code | codex | gemini | cursor | windsurf | cline | opencode | pi | aider | copilot | antigravity |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `pre.tool.any` | native | native | native | fanout×3 | fanout×4 | native | native | native | — | — | — |
| `pre.tool.shell` | native | native | native | native | native | native | native | native | — | — | — |
| `pre.tool.file_read` | native | native | native | native | native | native | native | native | — | — | — |
| `pre.tool.file_write` | native | native | native | — | native | native | native | native | — | — | — |
| `pre.tool.file_edit` | native | native | native | — | — | native | native | native | — | — | — |
| `pre.tool.mcp` | native | native | native | native | native | native | native | native | — | — | — |
| `pre.tool.web` | native | native | native | — | — | native | native | native | — | — | — |
| `pre.model` | — | — | native | — | — | — | — | — | — | — | — |
| `pre.prompt` | native | native | native | — | native | native | native | — | — | — | — |
| `pre.session.start` | native | native | native | native | — | — | native | — | — | — | — |
| `pre.session.end` | native | native | native | — | — | — | native | — | — | — | — |
| `pre.subagent.start` | native | native | — | — | — | — | — | — | — | — | — |
| `pre.subagent.end` | native | native | — | — | — | — | — | — | — | — | — |
| `pre.task.start` | — | — | — | — | — | native | — | — | — | — | — |
| `pre.task.resume` | — | — | — | — | — | native | — | — | — | — | — |
| `pre.task.cancel` | — | — | — | — | — | native | — | — | — | — | — |
| `post.tool.any` | native | native | native | — | — | native | native | native | — | — | — |
| `post.tool.shell` | native | native | native | — | native | native | native | native | — | — | — |
| `post.tool.file_read` | native | native | native | — | — | native | native | native | — | — | — |
| `post.tool.file_write` | native | native | native | native | — | native | native | native | — | — | — |
| `post.tool.file_edit` | native | native | native | native | — | native | native | native | — | — | — |
| `post.tool.mcp` | native | native | native | — | — | native | native | native | — | — | — |
| `post.tool.web` | native | native | native | — | — | native | native | native | — | — | — |
| `post.model` | — | — | native | — | — | — | — | — | — | — | — |
| `post.prompt` | — | — | — | — | native | — | native | — | — | — | — |
| `post.session.start` | native | native | native | native | — | — | native | — | — | — | — |
| `post.session.end` | native | native | native | — | — | — | native | — | — | — | — |
| `post.subagent.start` | native | native | — | — | — | — | — | — | — | — | — |
| `post.subagent.end` | native | native | — | — | — | — | — | — | — | — | — |
| `post.task.start` | — | — | — | — | — | native | — | — | — | — | — |
| `post.task.resume` | — | — | — | — | — | native | — | — | — | — | — |
| `post.task.cancel` | — | — | — | — | — | native | — | — | — | — | — |

Legend: `native` = 1:1 · `fanout×N` = one normalized event registers on N native events · `—` = no target.

The rows are every event at least one harness can host, derived from the adapters rather than hand-picked — so an event the adapters support cannot be missing from this grid. Session and subagent events are keyed by their **boundary**, so both phases resolve to the same native event: `pre.session.end` and `post.session.end` are one target, not two.

## Adapter provenance

How each adapter was established. `native` above describes what the adapter targets; this describes how much that is known — they are different claims.

| harness | provenance | established against |
|---|---|---|
| claude-code | doc-only | Claude Code hooks + settings.json documentation |
| codex | doc-fixture | Codex hooks references (agenticcontrolplane.com, developers.openai.com, deepwiki); the exact stdin field names are modeled on Claude's and are the one part not recorded |
| gemini | doc-only | Gemini CLI hooks documentation |
| cursor | doc-fixture | Cursor Hooks documentation and deep-dives (blog.gitbutler.com, johnlindquist/cursor-hooks) |
| windsurf | doc-only | Windsurf rules + hooks documentation |
| cline | doc-only | Cline hooks + .clinerules documentation |
| opencode | doc-only | OpenCode plugin API documentation |
| pi | doc-only | Pi plugin API documentation |
| aider | doc-only | Aider conventions documentation |
| copilot | doc-only | GitHub Copilot custom instructions + .github/instructions documentation |
| antigravity | doc-only | Antigravity 2.0 .agents/ layout, from primary docs; the layout may still shift |

Legend: `live-captured` = a payload recorded from a real install (`oh capture`) and committed as a fixture · `doc-fixture` = a fixture built from the vendor's primary docs, decoded by the conformance suite, but never recorded from a live run · `doc-only` = encoded from documentation, no recorded payload.

2 of 11 adapters are backed by a recorded payload. Upgrading one means running `oh capture` against a real install and committing the fixture — see [`tests/fixtures/`](https://github.com/danplischke/open-harness/tree/main/tests/fixtures).
