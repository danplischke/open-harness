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
| `src/dispatch.rs` | Single-entrypoint dispatcher: fan-out, merge, fail-closed |
| `src/model.rs` | The canonical stdio contract (payload in, decision out) |
| `src/api.rs` | Stable embeddable API (JSON-string boundary) the CLI + bindings call |
| `capabilities/secret-guard/` | A real **Python** blocking capability |
| `capabilities/audit-note/` | A real **Node/TS** non-blocking capability |
| `tests/conformance.rs` | 37 tests: harness contracts, protocol validation, kind plans, API |

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
cargo test                            # 37 conformance tests
cargo run -- matrix                   # support grid across 10 harnesses
cargo run -- check                    # per-capability installability
bash examples/demo.sh                 # deny across all four signal families
cargo run -- emit --harness cursor    # native registration (note the fan-out)
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
- **permission** — planned; see the issue tracker.

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
- Deferred: timeouts/sandboxing, registry/signing, the remaining capability
  kinds, the TypeScript/`napi` binding, Windows exec. See `FEASIBILITY.md`.

License: MIT.
