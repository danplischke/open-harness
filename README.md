# open-harness

**Author an AI-coding-agent capability once; run it on every harness.**

Claude Code, Codex, Gemini CLI, Cursor, Windsurf, Cline, OpenCode, Pi, Aider, and
Copilot all configure the same concepts — hooks, rules, commands, tools, skills,
permissions — in mutually incompatible ways. `open-harness` is the integration
layer that owns that un-standardized middle: an OSS author defines a capability
**once**, a developer **composes** capabilities from different sources, and the
customization is **programming-language-agnostic**. It stands *beside* the
existing standards (AGENTS.md for instructions, MCP for tools, SKILL.md for
skills), not on top of them.

This repository is a complete, working implementation of that design — all six
capability kinds across **10 harnesses**, composition + a lockfile, a trust
model, Python + Node bindings, and a single `oh` CLI — built to a strict
**honest-by-design** rule: where a harness can't express a concept, that is
declared and surfaced (a loud "degraded" note or an "unsupported" reason), never
silently dropped. It began as a feasibility spike (see
[`FEASIBILITY.md`](./FEASIBILITY.md)); what's deferred now is precise and listed
at the end — it is not yet packaged for distribution, and the eight proprietary
harnesses' adapters are encoded from documentation + recorded fixtures rather
than exercised against a live install.

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
[`spec/`](./spec/). Nothing about a capability is Rust-, Python-, or
TypeScript-specific — the events are normalized to a coordinate of `(phase,
subject, tool_class?)`, and one dispatcher translates the decision into whatever
each target harness actually reads: **exit code 2** (Claude/Codex/Gemini/
Windsurf), a **`permission`** JSON (Cursor), a **`cancel`** JSON (Cline), or an
**in-process return** via a generated shim (OpenCode/Pi). Where a harness has no
target at all (Aider/Copilot for tool gating), that's reported, not faked.

## The `oh` CLI

```sh
# Author
oh init                                                   # write an open-harness.json profile
oh scaffold --kind hook --lang python --id my-guard       # a runnable capability starter (py|node|typescript|bash)
oh scaffold --project  --id my-cap                        # a typed TypeScript capability as an npm package
oh capture  --harness cursor --event pre.tool.shell --out fixture.json   # record a harness's real native payload
oh doctor                                                 # check interpreters + capability health

# Run & inspect
oh run    --harness claude-code --event pre.tool.any      # a harness's single native hook entrypoint
oh matrix                                                 # the (event × harness) support grid
oh check                                                  # per-capability installability across all harnesses
oh emit   --harness cursor                                # native registration config (note the fan-out)

# Compose & install
oh resolve --profile open-harness.json                    # sources → a pinned open-harness.lock
oh sync    --profile open-harness.json --into ./project   # install + converge (idempotent)
oh check   --profile open-harness.json --into ./project --ci   # drift detection (fails CI on drift)
oh sync    --into ./project --uninstall                   # clean removal (managed files only)

# Trust
oh keygen --out author.key --label me                     # an ed25519 keypair
oh sign   --capability caps/web --key author.key
oh verify --capability caps/web --trust trust.json        # Trusted / Untrusted / Revoked / Invalid / Unsigned
oh trust  --capability caps/web --trust trust.json        # trust-on-first-use
oh trust  --root --key acme-root.key --trust trust.json   # trust a root (delegated trust via a keyring)
oh keyring --key acme-root.key --capability caps/web --out keyring.json   # a root vouches for an author
oh revoke --capability caps/web --trust trust.json --reason "compromised"

# MCP→CLI bridge (for orgs whose harness MCP client is disabled)
oh mcp list --id echo-bridge                              # speak MCP to a server through the shell
oh mcp call --id echo-bridge --tool echo --json '{"text":"hi"}'
```

### Try it in 30 seconds

```sh
cargo test                            # 112 tests, green on Linux/macOS/Windows
bash examples/walkthrough.sh          # the whole lifecycle: author → sign → compose → sync → dispatch → report
bash examples/demo.sh                 # one decision, four native deny conventions
cargo run -- matrix                   # the honest support grid across 10 harnesses
```

## Capability kinds

Capabilities declare a `kind`; each plugs into the same `Kind` trait
(`src/kind.rs`), so adding one needs no change to the dispatcher core. **All six
are implemented.**

- **hook** (runtime) — the stdio contract above, across 10 harnesses; authored in
  Python, Node, TypeScript (typed, run directly via Node type-stripping), or bash.
- **skill** (generative) — unifies `SKILL.md` for Claude / Codex / Cursor / Windsurf.
- **rule** (generative) — one glob-scoped rule → each harness's native spelling
  (Cursor `alwaysApply`+`globs`, Copilot `applyTo`, Windsurf `trigger`, Cline /
  Claude `paths`), degrading the activation modes a harness lacks with a loud note.
- **tool** (generative) — one tool → each harness's native **MCP** config
  (`mcpServers` vs `servers`, JSON vs Codex TOML, OpenCode `type:local`, `url` vs
  `httpUrl`), **or** an **MCP-free** shell delivery (`provider: exec`) for
  environments that block MCP, **or** — for an MCP *server* where the harness's
  MCP client is disabled — an **MCP→CLI bridge** (see below).
- **command** (generative) — one slash-command → each harness's native file:
  Markdown+YAML with `$ARGUMENTS`/`$N` (Claude/Codex/OpenCode/Cursor/Copilot/
  Windsurf/Cline) vs Gemini's `.toml` with `{{args}}`; positional args degrade
  on Gemini with a note.
- **permission** (generative) — one policy (per-tool/pattern `allow`/`ask`/`deny`)
  → **faithful** on Claude/OpenCode, **approximated with a loss report** on
  Codex/Gemini/Cursor/Windsurf/Copilot, and **Unsupported (with a reason)** on
  Cline/Aider/Pi. The honest-degradation showcase — five incompatible models.

`oh matrix` prints the full (event × harness) grid, generated from the adapters
(never hand-maintained) with a CI drift gate.

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
- Nothing is silently dropped — hook **registrations** and **global (`~/…`)**
  paths are reported as manual steps, **Unsupported** targets as blocked.

## Profiles, sources & dependencies

A **profile** names a set of **sources** and target harnesses; resolving it
composes capabilities and writes a reproducible `open-harness.lock` pinning every
capability (id + version + content fingerprint) and every git source's exact SHA.
Sources are a **local path**, a **git repo** (personal-repo sync — cloned, then
pinned to a commit), or a **registry** — a JSON index that maps names to their
real local/git source, one point of indirection over many repos.

```jsonc
// open-harness.json
{ "name": "my-setup", "harnesses": ["claude-code", "cursor"],
  "sources": [
    { "local": { "path": "capabilities" } },
    { "git": { "url": "https://github.com/me/my-caps", "rev": "main", "subdir": "capabilities" } },
    { "registry": { "index": "registry.json", "name": "acme-guards", "version": "1.2.0" } }
  ] }
```

Capabilities may declare **`dependencies`** on other ids; the resolver reports
unmet dependencies and cycles (loudly, non-fatally), orders the composed set so a
dependency precedes its dependents (a stable topological sort), and pins the
resolved graph in the lock. `sync --profile` resolves, then converges — sourcing
feeding the sync layer.

## Trust & signing

Installing a capability runs someone else's code with your agent's privileges, so
trust is built in. A capability is content-addressed by a **sha256** digest over
its file tree; the author signs that digest with an **ed25519** key into a
detached `capability.sig`. On verify the digest is recomputed from disk (tamper
caught before crypto) and the signature checked. The verdict is **five-valued**:

| verdict | meaning | passes gate? |
|---|---|---|
| `Trusted` | valid signature by a known (or root-delegated) key | always |
| `Untrusted` | valid signature, unknown key (trust-on-first-use territory) | only without `--require-signed` |
| `Revoked` | valid signature by a **withdrawn** key | never |
| `Invalid` | tampered content or bad signature | never |
| `Unsigned` | no signature | only without `--require-signed` |

Trust has three layers: **trust-on-first-use** (pin an author key), a **root of
trust** (a trusted root signs a *keyring* vouching for many authors, so you pin a
root once), and **revocation** (a withdrawn key never verifies again, even in
advisory mode — and revoking a root withdraws every author it vouched for). Each
capability also declares a **permission manifest** (`read` / `exec` / `network`)
surfaced for consent and policy-checked; `--require-signed` refuses anything not
`Trusted`, and a tampered capability is rejected before anything is written. See
[`SECURITY.md`](./SECURITY.md) for the full threat model and what is enforced vs
deferred.

## The MCP→CLI bridge

Some orgs block MCP by disabling the harness's MCP *client*. The bridge lets an
existing MCP *server's* tools still be called through the allowlisted shell:
`oh mcp call` speaks MCP to the server and returns the result on stdout, so the
agent never needs the harness's MCP client. It is a minimal, hand-rolled JSON-RPC
2.0 client over **stdio** and **streamable-HTTP** (chunked / Content-Length JSON
and SSE replies, `Mcp-Session-Id` threading); `http://` needs no dependency, and
`https://` is available via the optional **`mcp-http-tls`** feature (rustls),
with `OPEN_HARNESS_CA_FILE` to trust a private/corporate CA. Every emitted bridge
artifact carries a loud "the server still runs — this bridges the call path, not
the server or its egress" caveat.

## Embedding

The core is a Rust library (`open_harness::api`, a stable JSON-string boundary)
exposed to **Python** (uniffi) and **Node/TypeScript** (napi-rs). Both keep the
default build dependency-free.

```sh
bash bindings/build.sh         # Python: build the FFI cdylib, generate + run example.py
bash bindings/node/build.sh    # Node:   build the napi addon + run example.mjs
```

The Node addon is **native, not WASM** — the core spawns capabilities and reads
files, which WASM can't do — and it's the vehicle for **hosting the core
in-process inside an OpenCode/Pi plugin** rather than shelling out (see
`bindings/node/opencode-plugin.example.mjs`). The TypeScript authoring path
(`oh scaffold --project`) uses `@open-harness/node` for typed authoring + an
in-process dispatch test. See [`bindings/README.md`](./bindings/README.md).

## What's here

| Path | What |
|---|---|
| `src/event.rs` | Normalized event model: `(phase, subject, tool_class?)` |
| `src/adapters.rs` | 10 harness adapters: event mapping, deny signal, registration format |
| `src/kind.rs` + `src/kinds/` | The `Kind` trait + the six capability kinds |
| `src/dispatch.rs` | Single-entrypoint dispatcher: concurrent fan-out, merge, policy-driven fail-closed |
| `src/runtime.rs` | Hardened execution: per-capability timeout, output cap, error taxonomy, cross-platform interpreters |
| `src/model.rs` | The canonical stdio contract (payload in, decision out) |
| `src/sync.rs` | Compose a capability set, converge it into a project, detect drift |
| `src/profile.rs` | Profiles + sources (local / git / registry) + transitive deps → `open-harness.lock` |
| `src/trust.rs` | ed25519 signing/verification, trust store, root-of-trust keyrings, revocation, permissions |
| `src/mcp.rs` | Minimal MCP client (stdio + streamable-HTTP, optional TLS) — the bridge runtime |
| `src/scaffold.rs` + `src/capture.rs` + `src/matrix.rs` | `oh scaffold` / `oh capture` / the generated support matrix |
| `src/api.rs` + `src/main.rs` | Stable embeddable API; the `oh` CLI |
| `bindings/` | Python (uniffi) + Node/TS (napi) bindings |
| `capabilities/` | Real example capabilities (Python guard, Node audit note, MCP bridge, all six kinds) |
| `docs/` | mdBook site (concepts, authoring guide, generated matrix) |
| `spec/` | The frozen `hook@1` protocol + JSON Schemas |
| `tests/` | 112 tests: conformance (70) + sourcing (13) + trust (15) + MCP bridge (7) + capture (2) + authoring (5) |
| `.github/workflows/ci.yml` | CI: test on Linux/macOS/Windows; fmt+clippy; docs + matrix drift gate; e2e walkthrough; TLS feature |

## Dependencies

The default build is deliberately lean: `serde` / `serde_json`, and `ed25519-dalek`
/ `sha2` / `getrandom` for trust (integrity verification is always available, so
it is not feature-gated). Everything else that would pull weight is **opt-in**:
`ffi` (uniffi, for the Python binding), `mcp-http-tls` (rustls, for https on the
bridge). The stdio JSON-RPC client, the http client, and hex/base64 are
hand-rolled to keep it that way.

## Scope & honesty

- The eight proprietary harnesses aren't installed here, so their native
  *runtime* isn't exercised end-to-end; their conventions are encoded from docs.
  Codex and Cursor were validated against primary docs with recorded payloads
  under [`tests/fixtures/`](./tests/fixtures/); `oh capture` records real native
  payloads from a live harness to upgrade those fixtures (the recording is a
  manual step — it needs the harness installed).
- Execution is **cross-platform**: interpreters are resolved per-OS (no `#!`
  reliance — `python3`→`python` on Windows, `PATHEXT` honored), and the Python +
  Node capabilities run on Linux/macOS/Windows in CI.
- **Deferred, tracked as follow-ups on the
  [roadmap epic](https://github.com/danplischke/open-harness/issues/1):** runtime
  **sandbox enforcement** of the permission manifest (real fs/net/exec confinement
  needs OS mechanisms), process-group kill for forking capabilities, full
  TUF-style registry roles, live-recorded adapter fixtures, and publishing the
  bindings to npm / PyPI with a prebuilt-binary matrix. See `FEASIBILITY.md`.

License: MIT.
