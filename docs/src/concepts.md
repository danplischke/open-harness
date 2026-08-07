# Concepts

## Layers

- **L1 — Capability spec.** A capability is a directory with a `capability.json`
  manifest declaring a `kind`, plus assets. The set of kinds is open.
- **L2 — Runtime contract.** Language-agnostic execution: a **hook** is any
  executable that reads a canonical payload as JSON on stdin and writes a
  decision as JSON on stdout. Generative kinds emit files.
- **L3 — Adapters.** Per-harness mapping of the normalized model to each
  harness's native surface: event mapping, decode/encode, registration, and the
  capability matrix.
- **L4 — Composition.** Profiles + sources (local / git), a resolved lockfile,
  `sync` (install + drift), and trust/signing.

## The normalized event model

A hook binds to normalized events, described by `(phase, subject, tool_class?)`:

- **phase** — `pre` or `post`.
- **subject** — `tool`, `model`, `prompt`, `session`, `subagent`, `task`.
- **tool_class** — for tool events: `shell`, `file_read`, `file_write`,
  `file_edit`, `mcp`, or `any`.

An event id like `pre.tool.shell` maps to each harness's native event — 1:1
(`native`), 1-to-many (`fanout×N`, e.g. Cursor splits "before a tool" into
per-class events), or 1-to-none (`—`). See the
[support matrix](./harness-matrix.md).

## The deny-signal families

Only some events are **blocking** (a `pre` tool/model/prompt event). A capability
that returns `{"decision":"deny"}` on a blocking event is translated into
whatever the harness reads: exit code 2 (Claude/Gemini/Windsurf), a stdout
`permission` object (Cursor), a `permissionDecision` object (Codex), a `cancel`
object (Cline), or an in-process return via a generated shim (OpenCode/Pi). The
dispatcher owns the **deny-wins** merge, so composition is deterministic
regardless of the host harness.

## Capability kinds

The set of kinds is open; eight are implemented today, across an 11-harness
fleet (Claude Code, Codex, Gemini CLI, Cursor, Windsurf, Cline, OpenCode, Pi,
Aider, Copilot, and Google Antigravity).

- **hook** (runtime) — the stdio decision contract, across every harness that
  exposes hooks.
- **skill** — unify `SKILL.md` across vendors (Claude, Codex, Cursor, Windsurf,
  Copilot, OpenCode, Antigravity), with YAML-safe frontmatter and portable
  passthrough for spec + forward-compat fields.
- **agent** — one subagent → `.claude/agents/<id>.md`, `.opencode/agents/<id>.md`
  (tools lowered to the `permission` map), or `.github/agents/<id>.agent.md`;
  Unsupported elsewhere. `model_tier` is carried but not resolved (that's the
  consumer's registry, not the compiler's).
- **instructions** — one always-on brief → `CLAUDE.md` / `AGENTS.md` / `GEMINI.md`
  / `.github/copilot-instructions.md` (the [AGENTS.md](https://agents.md)
  standard plus the vendor-specific names).
- **rule** — one glob-scoped rule → each harness's native spelling.
- **command** — one slash-command → each harness's native file.
- **tool** — one tool → native **MCP** config, an **MCP-free** shell delivery, or
  an **MCP→CLI bridge** (stdio or streamable-HTTP) for a server where the harness's
  MCP client is disabled.
- **permission** — one policy (`allow`/`ask`/`deny`) → faithful on Claude/OpenCode,
  approximated-with-a-loss-report elsewhere, unsupported (with a reason) where
  there's no committed permission file.

The **permission**, **skill**, and **agent** kinds share one canonical tool
vocabulary (`read`, `edit`, `write`, `grep`, `glob`, `bash`, `web-fetch`,
`web-search`, `task`) that maps to each harness's native tokens — so a tool name
means the same thing wherever it appears, and unknown names pass through
verbatim. A per-harness `overrides` block (`{ "<harness-id>": { path, tools,
frontmatter } }`) lets one canonical capability tune its own output for a single
target without forking it.

## Trust

Installing a capability runs someone else's code. A capability is
content-addressed by a `sha256` digest and signed with an **ed25519** key; the
`oh verify` verdict is `Trusted` / `Untrusted` / `Revoked` / `Invalid` (tampered
— always rejected) / `Unsigned`, and `oh sync --require-signed` gates the
install. Each
capability also declares a **permission manifest** (`read` / `exec` / `network`)
that is surfaced for consent. See `SECURITY.md` for the threat model.
