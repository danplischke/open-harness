# Harness support matrix

> Generated from the adapters by `oh matrix --markdown` (`docs/gen-matrix.sh`) — do not edit by hand.

| event \ harness | claude-code | codex | gemini | cursor | windsurf | cline | opencode | pi | aider | copilot | antigravity |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `pre.tool.any` | native | native | native | fanout×3 | fanout×4 | native | native | native | — | — | — |
| `pre.tool.shell` | native | native | native | native | native | native | native | native | — | — | — |
| `post.tool.any` | native | native | native | — | — | native | native | native | — | — | — |
| `pre.model` | — | — | native | — | — | — | — | — | — | — | — |
| `pre.prompt` | native | native | native | — | native | native | native | — | — | — | — |
| `pre.session.start` | native | native | native | native | — | — | native | — | — | — | — |
| `pre.subagent.start` | native | native | — | — | — | — | — | — | — | — | — |
| `pre.task.start` | — | — | — | — | — | native | — | — | — | — | — |

Legend: `native` = 1:1 · `fanout×N` = one normalized event registers on N native events · `—` = no target.
