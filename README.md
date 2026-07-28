# open-harness (feasibility spike)

> **Status: feasibility spike, not a product.** This repository currently holds a
> deliberately narrow prototype that answers one question — *can the fragmented
> hook surface of the major coding harnesses be driven from a single normalized
> model over a language-agnostic contract, and where does that break?* — before
> committing to the full architecture. See [`FEASIBILITY.md`](./FEASIBILITY.md)
> for the findings and verdict.

## The idea being tested

Different coding harnesses (Claude Code, Codex, Gemini CLI, Cursor, Windsurf,
Cline, OpenCode, Pi, …) configure the same concepts — hooks, rules, commands,
tools — in mutually incompatible ways. `open-harness` is meant to be the missing
**integration layer**: OSS authors build a capability **once**, developers
**compose** capabilities from different sources, and the customization is
**programming-language-agnostic**. It stands *beside* the existing standards
(AGENTS.md for instructions, MCP for tools, SKILL.md for skills) and covers the
un-standardized middle.

This spike implements the riskiest slice of that — **hooks** — for 10 harnesses.

## What's here

| Path | What |
|---|---|
| `src/event.rs` | Normalized event model: `(phase, subject, tool_class?)` |
| `src/adapters.rs` | 10 harness adapters: event mapping, deny signal, registration format |
| `src/dispatch.rs` | Single-entrypoint dispatcher: concurrent fan-out, merge, policy-driven fail-closed |
| `src/runtime.rs` | Hardened execution: per-capability timeout, output cap, error taxonomy |
| `src/model.rs` | The canonical stdio contract (payload in, decision out) |
| `src/api.rs` | Stable embeddable API (JSON-string boundary) the CLI + bindings call |
| `src/sync.rs` | Compose a capability set, converge it into a project, detect drift |
| `capabilities/secret-guard/` | A real **Python** blocking capability |
| `capabilities/audit-note/` | A real **Node/TS** non-blocking capability |
| `tests/conformance.rs` | 58 tests: harness contracts, protocol, kind plans, composition, runtime, API |

## The contract (this is the whole point)

A **capability** is *any executable in any language* that reads a canonical
payload as JSON on stdin and writes a decision as JSON on stdout:

```jsonc
// stdin  (CanonicalPayload)
{ "protocol": "open-harness/hook@1", "harness": "claude-code",
  "event": {"phase":"pre","subject":"tool","tool_class":"shell"},
  "blocking": true, "tool": {"name":"Bash","input":{"command":"…"}} }

// stdout (Decision)
{ "decision": "deny", "reason": "potential secret in tool input" }
```

The wire protocol is frozen as `hook@1` with a normative spec and JSON Schemas in
[`spec/`](./spec/).

The dispatcher translates that one decision into whatever the target harness
actually reads — exit code 2 (Claude/Codex/Gemini/Windsurf), a `permission` JSON
(Cursor), a `cancel` JSON (Cline), or an in-process return via a generated shim
(OpenCode/Pi).

## Try it

```sh
cargo test                            # 58 conformance tests
cargo run -- matrix                   # support grid across 10 harnesses
cargo run -- check                    # per-capability installability
bash examples/demo.sh                 # deny across all four signal families
cargo run -- emit --harness cursor    # native registration (note the fan-out)
cargo run -- sync --into /tmp/proj --dry-run   # compose the set → files (preview)
cargo run -- sync --into /tmp/proj             # install; re-run = no-op (idempotent)
cargo run -- check --into /tmp/proj --ci       # drift detection, non-zero on drift
cargo run -- sync --into /tmp/proj --uninstall # clean removal (managed files only)
```

## Capability kinds

Capabilities declare a `kind`; each kind plugs into the same `Kind` trait
(`src/kind.rs`) and adding one needs no change to the dispatcher core.

- **hook** (runtime) — the stdio contract above, across 10 harnesses.
- **skill** (generative) — unifies `SKILL.md` for Claude / Codex / Cursor / Windsurf.
- **rule** (generative) — one glob-scoped rule → each harness's native spelling
  (Cursor `alwaysApply`+`globs`, Copilot `applyTo`, Windsurf `trigger`, Cline /
  Claude `paths`), degrading the activation modes a harness lacks with a loud note.
- **tool** (generative) — one tool → each harness's native **MCP** config
  (`mcpServers` vs `servers`, JSON vs Codex TOML, OpenCode `type:local`, `url` vs
  `httpUrl`), **or** an **MCP-free** shell delivery (`provider: exec`) for
  environments that block MCP. (An MCP-server→shell bridge is tracked in #19.)
- **command** (generative) — one slash-command → each harness's native file:
  Markdown+YAML with `$ARGUMENTS`/`$N` (Claude/Codex/OpenCode/Cursor/Copilot/
  Windsurf/Cline) vs Gemini's `.toml` with `{{args}}`; positional args degrade
  on Gemini with a note.
- **permission** (generative) — one policy (per-tool/pattern `allow`/`ask`/`deny`)
  → **faithful** on Claude/OpenCode, **approximated with a loss report** on
  Codex/Gemini/Cursor/Windsurf/Copilot, and **Unsupported (with a reason)** on
  Cline/Aider/Pi. The honest-degradation showcase — five incompatible models.

All six capability kinds are now implemented.

## Sync & composition

`emit` prints one capability's config; `sync` **installs a whole set** into a
project and keeps it converged. It plans every capability across the target
harnesses, groups the artifacts by their real on-disk path, and writes them
idempotently — a second `sync` is a no-op, removing a capability **prunes** the
file it owned, and `--uninstall` cleanly removes everything (managed files only;
a hand-written file is never touched). Every write is tracked in a lockfile
(`.open-harness/sync.lock.json`), so `check --into DIR --ci` fails CI when the
installed config drifts (missing / modified / stale) from the source.

When several capabilities target the **same file** — two permission policies on
`opencode.json`, or Windsurf + Copilot both on `.vscode/settings.json` — they are
**composed**, not clobbered, by documented, deterministic rules:

- **JSON** deep-merges: objects recurse, arrays union, and a verdict clash takes
  the **most restrictive** (`deny` > `ask` > `allow` — the hook deny-wins rule,
  extended to permissions). Every resolution is reported as a conflict.
- **Non-JSON** (Markdown / TOML) can't be merged structurally: identical content
  is idempotent, differing content keeps the first producer (by id order) and is
  loudly flagged.
- Nothing is silently dropped — hook **registrations** and **global (`~/…`)
  paths** are reported as manual steps, **Unsupported** targets as blocked.

```sh
cargo run -- sync --into /tmp/proj --harness opencode   # compose safe-shell + strict-ci → one policy
```

(Composition is the L4 "apply a profile" layer, #16. Profiles/lockfile sourcing
from git & personal repos is #15.)

## Embedding

The core is a Rust library (`open_harness::api`) exposed to other languages via
uniffi — feature-gated so the default build never depends on it. Python works
today:

```sh
bash bindings/build.sh    # build the FFI cdylib, generate + run the Python example
```

See [`bindings/README.md`](./bindings/README.md). TypeScript/`napi` is planned.

## Scope & honesty

- The 8 proprietary harnesses aren't installed here, so their native *runtime*
  isn't exercised; their conventions are encoded from docs. Codex and Cursor
  exact field names are `MEDIUM CONFIDENCE` (flagged in code).
- The hook runtime is hardened (`src/runtime.rs`): per-capability timeout with
  kill-on-exceed, stdout/stderr caps, a classified error taxonomy surfaced via
  `--explain`, and per-binding failure policy (`auto`/`fail_closed`/`fail_open`/
  `pass_through`). Only the direct child is killed today — a process-group kill
  for forking capabilities needs a `libc`/`nix` dep and is deferred.
- Deferred: sandboxing + process-group kill, registry/signing, the MCP→CLI
  bridge (#19), the TypeScript/`napi` binding, Windows exec. See `FEASIBILITY.md`.

License: MIT.
