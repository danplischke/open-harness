# Project and user scope

Most capabilities you want are not per-project. A commit-message skill, a
secret guard, your house style — you want those everywhere, and only a few
things are genuinely specific to one repo.

```sh
oh sync --global            # ~/.claude/skills/… — every project on this machine
oh sync --into ./project    # this repo only
```

## Why this is two flat layers and not a merge

Every harness open-harness targets already reads **both** levels itself, and
already decides which wins — project over user, near-universally. Claude looks in
`~/.claude/skills/` and `./.claude/skills/`; Cursor does the same for rules.

So open-harness does not compose the two. It writes to the right level and gets
out of the way. Concretely:

* `~/.open-harness/open-harness.yaml` is your **user profile** — what you want
  everywhere.
* `./open-harness.yaml` is the **project profile** — only what is *extra* here.
* Neither knows about the other.

The alternative — resolving the user profile inside each project and syncing the
union — would vendor a copy of every global capability into every repo. That is
churn in `git status`, a second copy to keep in step, and a diff nobody wants to
review. The harness's own lookup already does this correctly for free.

Each scope keeps its own lockfile (`.open-harness/sync.lock.yaml`, relative to
that scope's root), so the ownership model works per scope and the two can never
prune each other's files. `oh sync --global --uninstall` removes exactly what the
user scope installed and structurally cannot touch a project.

## Why `--global` is a flag and not a profile field

A profile is *data that travels*: cloned, vendored, resolved from a git source.
A `scope: user` key inside one would let a repository you cloned write into your
home directory. A flag can only be set by the person at the keyboard.

## Where things go

Checked against vendor documentation in August 2026. Anything not in this table
is **deferred with a reason** rather than guessed at — see below.

| harness | artifact | project | user |
|---|---|---|---|
| Claude Code | skills | `.claude/skills/` | `~/.claude/skills/` |
| Claude Code | subagents | `.claude/agents/` | `~/.claude/agents/` |
| Claude Code | commands | `.claude/commands/` | `~/.claude/commands/` |
| Claude Code | instructions | `CLAUDE.md` | `~/.claude/CLAUDE.md` |
| Claude Code | permissions | `.claude/settings.json` | `~/.claude/settings.json` |
| Cursor | rules | `.cursor/rules/` | `~/.cursor/rules/` |
| Codex | instructions | `AGENTS.md` | `~/.codex/AGENTS.md` |
| Codex | commands | — | `~/.codex/prompts/` (always user-level) |
| Gemini CLI | instructions | `GEMINI.md` | `~/.gemini/GEMINI.md` |
| Gemini CLI | commands | `.gemini/commands/` | `~/.gemini/commands/` |
| OpenCode | instructions | `AGENTS.md` | `~/.config/opencode/AGENTS.md` |
| OpenCode | subagents | `.opencode/agents/` | `~/.config/opencode/agents/` |
| OpenCode | commands | `.opencode/commands/` | `~/.config/opencode/commands/` |
| OpenCode | skills | `.opencode/skills/` | `~/.config/opencode/skills/` |

open-harness's own directory (`.open-harness/`) is not a harness's config and
needs no table entry — at user scope it is simply `~/.open-harness/`, beside the
user profile and lockfile.

### The mapping is not "same path under `$HOME`"

That naive rule is wrong often enough to be dangerous, and being wrong here means
writing files into a home directory that nothing will ever read:

* **A different directory.** OpenCode's user config is XDG —
  `~/.config/opencode/`, not `~/.opencode/`.
* **A different filename.** Claude's project brief is `CLAUDE.md` at the repo
  root; the user-level one is `~/.claude/CLAUDE.md`, *not* `~/CLAUDE.md`.
* **A shared file that splits.** `AGENTS.md` is one file that six harnesses read
  in a project. At user scope they diverge — `~/.codex/AGENTS.md` and
  `~/.config/opencode/AGENTS.md` are different files — so the remap runs per
  harness, before paths are grouped.

## What is deliberately missing

`oh sync --global` reports these instead of writing them. Each is a case where a
plausible guess exists and shipping it would be worse than the deferral.

| harness | why not |
|---|---|
| Windsurf rules | user-level rules are a **single** `~/.codeium/windsurf/memories/global_rules.md` capped at 6,000 characters — not a per-rule directory, so N rules cannot be N files |
| Cline | documented as `~/Documents/Cline/Rules`, but the project's own issue tracker records the Linux path as wrong and `~/Cline/Rules` in use; platform-ambiguous |
| Copilot | user-level custom instructions live in editor settings, not a file with a stable path |
| Claude rules, Cursor skills/commands, Codex skills, most permission and MCP targets | no user-level location documented for that artifact |

This mirrors the [adapter provenance](./harness-matrix.md) rule: the tool says
what it knows and how well, rather than acting confidently on a guess. If you
know a documented path that is missing here, it is a one-line table entry in
`src/sync.rs`.

## Sources

- [Claude Code — skills](https://code.claude.com/docs/en/skills)
- [Claude Code — settings](https://code.claude.com/docs/en/settings) (the
  user/project/local table, including `~/.claude/CLAUDE.md`)
- [Cursor — rules](https://cursor.com/docs/rules) and
  [Cursor global user rules](https://codehabits.dev/blog/cursor-global-user-rules)
- [Codex — AGENTS.md](https://developers.openai.com/codex/guides/agents-md)
- [Gemini CLI — settings](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/settings.md)
- [OpenCode — rules](https://opencode.ai/docs/rules/)
- [Windsurf — Cascade memories](https://docs.windsurf.com/windsurf/cascade/memories)
- [Cline — rules](https://docs.cline.bot/customization/cline-rules) and
  [cline#5153](https://github.com/cline/cline/issues/5153) (the Linux path
  discrepancy)
