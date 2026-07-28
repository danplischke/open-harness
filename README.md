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
| `src/mcp.rs` | Minimal MCP client (stdio JSON-RPC) — the MCP→CLI bridge runtime |
| `src/model.rs` | The canonical stdio contract (payload in, decision out) |
| `src/api.rs` | Stable embeddable API (JSON-string boundary) the CLI + bindings call |
| `src/sync.rs` | Compose a capability set, converge it into a project, detect drift |
| `src/profile.rs` | Profiles + sources (local / git / registry) → resolved `open-harness.lock` |
| `src/trust.rs` | Signing + verification (ed25519 over a sha256 digest), trust store, permissions |
| `capabilities/secret-guard/` | A real **Python** blocking capability |
| `capabilities/audit-note/` | A real **Node/TS** non-blocking capability |
| `tests/conformance.rs` | 69 tests: harness contracts, protocol, kind plans, composition, runtime, API |
| `tests/profile.rs` + `tests/trust.rs` | Sourcing/lockfile (8) and signing/trust (9) tests |
| `tests/fixtures/` | Recorded Codex + Cursor native payloads (adapter validation, with sources) |
| `.github/workflows/ci.yml` | CI: build + test on Linux/macOS/Windows; fmt + clippy on Linux |

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
cargo test                            # 90 tests (conformance + profile + trust + mcp)
cargo run -- mcp call --id echo-bridge --tool echo --json '{"text":"hi"}'   # MCP→CLI bridge
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
  environments that block MCP, **or** — for an MCP *server* where the harness's
  MCP client is disabled — an **MCP→CLI bridge**: a shell wrapper backed by
  `oh mcp call` (a minimal MCP client) plus instructions, with a loud "the server
  still runs" caveat on every artifact.
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

## Profiles & sources

A **profile** names a set of **sources** and target harnesses; resolving it
composes capabilities from a **local path**, a **git repo** (personal-repo sync
— cloned, then pinned to a commit), or a **registry** (stubbed, reported), and
writes a reproducible `open-harness.lock` pinning every capability (id + version
+ fingerprint) and every git source's exact SHA. `sync --profile` resolves, then
converges the composed set — sourcing (L4, #15) feeding the sync layer (#16).

```jsonc
// open-harness.json
{ "name": "my-setup", "harnesses": ["claude-code", "cursor"],
  "sources": [
    { "local": { "path": "capabilities" } },
    { "git": { "url": "https://github.com/me/my-caps", "rev": "main", "subdir": "capabilities" } }
  ] }
```

```sh
cargo run -- resolve --profile open-harness.json      # resolve sources → write open-harness.lock
cargo run -- sync    --profile open-harness.json --into /tmp/proj   # resolve, then converge
```

(Registry sources and transitive dependency resolution are the remaining #15
follow-ups; a `version`/`dependencies` package format and the lockfile are here.)

## Trust & signing

Installing a capability runs someone else's code with your agent's privileges, so
trust is built in (#17). A capability is content-addressed by a **sha256** digest
over its file tree; the author signs that digest with an **ed25519** key into a
detached `capability.sig`. On verify the digest is recomputed from disk (tamper
caught before crypto) and the signature checked; the verdict is four-valued —
`Trusted` (known key), `Untrusted` (valid but unknown key), `Invalid` (tampered /
bad signature — **always rejected**), `Unsigned`. A `TrustStore` holds trusted
keys (trust-on-first-use). Each capability also declares a **permission manifest**
(`read` / `exec` / `network`) that is surfaced for consent and policy-checked.

```sh
cargo run -- keygen --out author.key --label "me"        # generate an ed25519 keypair
cargo run -- sign   --capability caps/web --key author.key
cargo run -- verify --capability caps/web --trust trust.json     # Untrusted until you trust the key
cargo run -- trust  --capability caps/web --trust trust.json     # TOFU: add the signer
cargo run -- sync   --profile open-harness.json --into /tmp/proj \
                    --trust trust.json --require-signed --deny-network   # gated install
```

`--require-signed` refuses anything not `Trusted`; a tampered capability is
rejected before anything is written. See [`SECURITY.md`](./SECURITY.md) for the
full threat model and what is enforced vs deferred (sandboxing, revocation, a
registry root of trust). This adds `ed25519-dalek` + `sha2` as core deps —
integrity verification must always be available, so it is not feature-gated.

## Embedding

The core is a Rust library (`open_harness::api`) exposed to **Python** (uniffi)
and **Node/TypeScript** (napi-rs). Both keep the default build dependency-free.

```sh
bash bindings/build.sh         # Python: build the FFI cdylib, generate + run example.py
bash bindings/node/build.sh    # Node:   build the napi addon + run example.mjs
```

The Node addon is **native, not WASM** — the core spawns capabilities and reads
files, which WASM can't do — and it's the vehicle for **hosting the core
in-process inside an OpenCode/Pi plugin** rather than shelling out (see
`bindings/node/opencode-plugin.example.mjs`). See
[`bindings/README.md`](./bindings/README.md). Publishing the npm package with a
prebuilt-binary matrix is the remaining #14 follow-up.

## Scope & honesty

- The 8 proprietary harnesses aren't installed here, so their native *runtime*
  isn't exercised; their conventions are encoded from docs. Codex and Cursor —
  previously `MEDIUM CONFIDENCE` — were validated against primary docs and their
  adapters corrected (Cursor's per-event snake_case payloads; Codex's stdout
  `permissionDecision` deny, `hooks.json` registration, shell-only PreToolUse),
  with recorded payloads under [`tests/fixtures/`](./tests/fixtures/). The one
  part still modeled rather than captured is Codex's exact stdin field names.
- The hook runtime is hardened (`src/runtime.rs`): per-capability timeout with
  kill-on-exceed, stdout/stderr caps, a classified error taxonomy surfaced via
  `--explain`, and per-binding failure policy (`auto`/`fail_closed`/`fail_open`/
  `pass_through`). Only the direct child is killed today — a process-group kill
  for forking capabilities needs a `libc`/`nix` dep and is deferred.
- Execution is **cross-platform**: interpreters are resolved per-OS (no `#!`
  reliance — `python3`→`python` on Windows, `PATHEXT` honored) and the Python +
  Node capabilities are exercised on Linux/macOS/Windows in CI. `platform_note`
  surfaces per-harness caveats (e.g. Cline hooks are Unix-only).
- Deferred: sandboxing + process-group kill, registry/signing, the MCP→CLI
  bridge (#19), the TypeScript/`napi` binding. See `FEASIBILITY.md`.

License: MIT.
