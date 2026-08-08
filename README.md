# open-harness

**Author an AI-coding-agent capability once; run it on every harness.**

Claude Code, Codex, Gemini CLI, Cursor, Windsurf, Cline, OpenCode, Pi, Aider,
Copilot, and Google Antigravity all configure the same concepts — hooks, rules,
commands, tools, skills, permissions, subagents, instructions — in mutually
incompatible ways. `open-harness` is the integration layer that owns that
un-standardized middle: an OSS author defines a capability **once**, a developer
**composes** capabilities from different sources, and the customization is
**programming-language-agnostic**. It builds *on* the existing standards
(AGENTS.md for instructions, MCP for tools, SKILL.md for skills) rather than
replacing them.

This repository is a complete, working implementation of that design — all eight
capability kinds across **11 harnesses**, composition + a lockfile, a trust
model, Python + Node bindings, and a single `oh` CLI — built to a strict
**honest-by-design** rule: where a harness can't express a concept, that is
declared and surfaced (a loud "degraded" note or an "unsupported" reason), never
silently dropped. It began as a feasibility spike (see
[`FEASIBILITY.md`](./FEASIBILITY.md)); what's deferred now is precise and listed
at the end — the language bindings aren't yet published to npm/PyPI, and the
proprietary harnesses' adapters are encoded from documentation + recorded
fixtures rather than exercised against a live install.

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
oh init                                                   # write an open-harness.yaml profile
oh migrate                                                # rewrite legacy JSON configs as YAML
oh scaffold --kind hook --lang python --id my-guard       # a runnable capability starter (py|node|typescript|bash)
oh scaffold --kind agent --id db-engineer                 # or: skill | rule | command | tool | permission | instructions
oh scaffold --project  --id my-cap                        # a typed TypeScript capability as an npm package
oh capture  --harness cursor --event pre.tool.shell --out fixture.json   # record a harness's real native payload
oh version                                                # binary + protocol version (for consumer version negotiation)
oh doctor                                                 # check interpreters, runtimes + capability health

# Run & inspect
oh run    --harness claude-code --event pre.tool.any      # a harness's single native hook entrypoint
oh matrix                                                 # the (event × harness) support grid
oh check                                                  # per-capability installability across all harnesses
oh emit   --harness cursor                                # native registration config (note the fan-out)

# Compose & install
oh resolve --profile open-harness.yaml                    # sources → a pinned open-harness.lock
oh resolve --profile open-harness.yaml --locked           # verify the lock instead of rewriting it (CI)
oh sync    --profile open-harness.yaml --into ./project   # install + converge (idempotent)
oh check   --profile open-harness.yaml --into ./project --ci   # drift detection (fails CI on drift)
oh sync    --into ./project --uninstall                   # clean removal (managed files only)

# Trust
oh keygen --out author.key --label me                     # an ed25519 keypair
oh sign   --capability caps/web --key author.key
oh verify --capability caps/web --trust trust.yaml        # Trusted / Untrusted / Revoked / Invalid / Unsigned
oh trust  --capability caps/web --trust trust.yaml        # trust-on-first-use
oh trust  --root --key acme-root.key --trust trust.yaml   # trust a root (delegated trust via a keyring)
oh keyring --key acme-root.key --capability caps/web --out keyring.yaml   # a root vouches for an author
oh revoke --capability caps/web --trust trust.yaml --reason "compromised"

# MCP→CLI bridge (for orgs whose harness MCP client is disabled)
oh mcp list --id echo-bridge                              # speak MCP to a server through the shell
oh mcp call --id echo-bridge --tool echo --json '{"text":"hi"}'
```

### Try it in 30 seconds

```sh
cargo test                            # 275 tests, green on Linux/macOS/Windows
bash examples/walkthrough.sh          # the whole lifecycle: author → sign → compose → sync → dispatch → report
bash examples/demo.sh                 # one decision, four native deny conventions
cargo run -- matrix                   # the honest support grid across 11 harnesses
```

## Capability kinds

Capabilities declare a `kind`; each plugs into the same `Kind` trait
(`src/kind.rs`), so adding one needs no change to the dispatcher core. **All eight
are implemented.**

The **document kinds** (skill, agent, command, rule, instructions) are authored
as a **single file** whose YAML frontmatter *is* the manifest — a skill is just a
`SKILL.md`, an agent an `AGENT.md` — so you write the artifact itself, no sidecar
`capability.yaml`. `id` defaults to the directory name, `kind` to the filename.
Hooks and tools keep a `capability.yaml` (no body, structured `run`/`server`
config); it also still works for any kind, and wins when both are present.

- **hook** (runtime) — the stdio contract above, across every harness with hooks;
  authored in Python, Node, TypeScript (typed, run directly via Node
  type-stripping), or bash.
- **skill** (generative) — unifies `SKILL.md` for Claude / Codex / Cursor /
  Windsurf / Copilot / OpenCode / Antigravity, with YAML-safe frontmatter and a
  portable passthrough that carries the Agent Skills spec + forward-compat fields.
- **agent** (generative) — one subagent → `.claude/agents/<id>.md`,
  `.opencode/agents/<id>.md` (tools lowered to OpenCode's `permission` map, with
  `write` folded into `edit`), or `.github/agents/<id>.agent.md`; a workflow shim
  on Antigravity; Unsupported elsewhere. `model` is portable; `model_tier` is
  carried but resolved by the consumer, not the compiler.
- **instructions** (generative) — one always-on brief → `CLAUDE.md` / `AGENTS.md`
  / `GEMINI.md` / `.github/copilot-instructions.md` (the AGENTS.md standard plus
  the vendor-specific names), converged to one shared file where several harnesses
  read the same path.
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

The **permission**, **skill**, and **agent** kinds share one canonical tool
vocabulary (`read`/`edit`/`write`/`grep`/`glob`/`bash`/`web-fetch`/`web-search`/
`task`) that lowers to each harness's native tokens (with the documented gotchas
baked in — Claude's `Task`→`Agent` rename, Copilot's `toolReferenceName`s),
passing unknown names through verbatim. Any generative capability can also carry a
per-harness **`overrides`** block (`path` / `tools` / `frontmatter`) to tune one
target without forking.

`oh matrix` prints the full (event × harness) grid, generated from the adapters
(never hand-maintained) with a CI drift gate.

## Configuration format

Everything open-harness itself defines is **YAML**: the capability manifest
(`capability.yaml`), the profile (`open-harness.yaml`), both lockfiles, the
registry index, the trust store, keyrings, key files, and detached signatures.
JSON is still *read* — a `capability.json` from before the switch loads
unchanged, and `oh migrate` rewrites a tree in place (`--dry-run` to preview).

The line is deliberate, and it's the same one this project draws everywhere else:
a **harness's** config is written in *that harness's* format, never ours.
`.claude/settings.json`, `opencode.json`, `.mcp.json`, `.vscode/settings.json`
and the Codex/Gemini TOML stay exactly as they are, along with the frozen
`hook@1` wire protocol (JSON on stdin/stdout), the JSON Schemas in `spec/`, and
`oh capture` fixtures — those are recordings of *native* payloads. `src/config.rs`
is that boundary; `sync`'s JSON deep-merge composition is untouched.

One caveat worth stating plainly: a capability is content-addressed over its file
tree, so renaming `capability.json` to `capability.yaml` **changes its digest and
invalidates any existing signature**. `oh migrate` says so; re-run `oh sign`.

## Sync & composition

`emit` prints one capability's config; `sync` **installs a whole set** into a
project and keeps it converged. It plans every capability across the target
harnesses, groups the artifacts by their real on-disk path, and writes them
idempotently — a second `sync` is a no-op, removing a capability **prunes** the
file it owned, and `--uninstall` cleanly removes everything (managed files only;
a hand-written file is never touched). Every write is tracked in a lockfile
(`.open-harness/sync.lock.yaml`), so `check --into DIR --ci` fails CI when the
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
capability (qualified name + version + a sha256 over its file tree) and every git
source's exact SHA. Sources are a **local path**, a **git repo** (personal-repo
sync — cloned, then pinned to a commit), or a **registry** — an index that maps
names to their real local/git source, one point of indirection over many repos.

```yaml
# open-harness.yaml
name: my-setup
harnesses: [claude-code, cursor]
sources:
  - local:
      path: capabilities
  - git:
      url: https://github.com/me/my-caps
      rev: main
      subdir: capabilities
  - registry:
      index: registry.yaml
      name: acme-guards
      version: 1.2.0
```

A source rarely gives you only what you want, so each one takes a `select`
block — `include` / `exclude` patterns over the qualified name or bare id,
`kinds` to take a plugin's skills without its harness-specific hooks, and
`with_dependencies` when you want the closure rather than a report of what the
narrowing cost. An `include` pattern that matches nothing is an error listing
what is actually there, and selection happens before locking, so widening it
later is a visible lock change.

Because sources are other people's repositories, a capability's identity is a
**qualified name** — `<namespace>/<id>`, the namespace coming from the source
(`<owner>/<repo>` for git, the entry name for a registry, nothing for a local
path). Two authors' `mem-search` are different capabilities and both compose; the
bare `id` remains the local alias and the emitted filename, so the path clash
that implies is reported before anything is written.

Capabilities declare **`dependencies`** with version requirements and a relation
— `requires` / `suggests` / `conflicts` / `replaces` — over a documented semver
subset (`^ ~ >= > < <= =`, `*`, combining with `,`):

```yaml
dependencies:
  acme/shared-patterns: "^1.2"
  acme/legacy: { version: ">=2, <4", relation: suggests }
```

Sources stay a **preference order**: the earliest providing a name wins, and a
requirement can *veto* that choice but never reorders sources for its own sake.
When nothing satisfies every dependent, the report names who asked for what.
There is no backtracking, and the docs say so rather than implying a search that
did not happen. Cycles, unmet edges, live `conflicts`, and dependencies that are
**Unsupported on some target harness** (present ≠ usable there) are all reported
loudly and non-fatally; the set is ordered so a dependency precedes its
dependents.

Following a dependency's *own* source — cloning a repo you never named, whose
code then runs with your agent's privileges — is **opt-in** (`resolution:
transitive`) and gated on a signature by default (`transitive_trust: trusted |
tofu | any`). Every refusal and every acquisition is reported.

`oh resolve --locked` / `oh sync --locked` turn the lockfile into a contract:
it must exist, the resolution must match, nothing is written, and a mismatch
prints exactly what drifted. See [`docs/src/dependencies.md`](./docs/src/dependencies.md)
for the full model.

## Runtimes

A capability is any executable, so it can need things the host lacks. A hook
whose import fails exits non-zero, and on a blocking event that is a **deny** —
every tool call refused, with a stack trace as the reason. So a capability can
declare what its entrypoint needs on `PATH`:

```yaml
runtime:
  requires: [uv]
run:
  command: uv
  args: [run, --script, guard.py]
```

`oh check` and `oh doctor` report the gap with a per-OS install hint before a
session ever starts, and if it still reaches dispatch the failure has its own
class (`runtime-missing`) naming the tool instead of a traceback. The verdict is
deliberately unchanged — a guard that cannot run leaves you unguarded.

**open-harness installs nothing and bundles nobody else's toolchain.** `pip` and
`npm` execute arbitrary code at install time, which is a bigger hole than the
one gating transitive acquisition closes; and vendoring `uv` (50 MB) or `node`
(119 MB) into a 2.8 MB static binary would ship third-party code with none of
the verification capabilities themselves must pass. It checks, it reports, you
decide. [`docs/src/runtimes.md`](./docs/src/runtimes.md) has the full ladder —
compiled binary, vendored deps (which fall inside the content digest, so a
signature covers them), ecosystem-provisioned, system packages — and which
languages are paved versus merely resolvable.

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
| `src/adapters.rs` | 11 harness adapters: event mapping, deny signal, registration format |
| `src/kind.rs` + `src/kinds/` | The `Kind` trait + the eight capability kinds |
| `src/tools.rs` + `src/yaml.rs` | Canonical tool vocabulary (shared by permission/skill/agent) + YAML (hand-rolled block emit; read via `yaml-rust2`) |
| `src/config.rs` | open-harness's own config files: YAML out, YAML-or-JSON in — the boundary against a harness's native formats |
| `src/dispatch.rs` | Single-entrypoint dispatcher: concurrent fan-out, merge, policy-driven fail-closed |
| `src/runtime.rs` | Hardened execution: per-capability timeout, output cap, error taxonomy, cross-platform interpreters, `runtime.requires` pre-flight |
| `src/model.rs` | The canonical stdio contract (payload in, decision out) |
| `src/sync.rs` | Compose a capability set, converge it into a project, detect drift |
| `src/profile.rs` | Profiles + sources (local / git / registry), qualified names, selection, opt-in transitive acquisition → `open-harness.lock` |
| `src/deps.rs` | The dependency vocabulary: a documented semver subset, requirements, relations |
| `src/trust.rs` | ed25519 signing/verification, trust store, root-of-trust keyrings, revocation, permissions |
| `src/mcp.rs` | Minimal MCP client (stdio + streamable-HTTP, optional TLS) — the bridge runtime |
| `src/scaffold.rs` + `src/capture.rs` + `src/matrix.rs` | `oh scaffold` / `oh capture` / the generated support matrix |
| `src/api.rs` + `src/main.rs` | Stable embeddable API; the `oh` CLI |
| `bindings/` | Python (uniffi) + Node/TS (napi) bindings |
| `capabilities/` | Real example capabilities (Python guard, Node audit note, MCP bridge, subagent, instructions — all eight kinds) |
| `docs/` | mdBook site (concepts, authoring guide, dependencies, runtimes, generated matrix) |
| `spec/` | The frozen `hook@1` protocol + JSON Schemas |
| `tests/` | 275 tests: conformance (70) + sourcing & dependencies (40) + new kinds (24) + deps vocabulary (21) + config/YAML (20) + single-file (19) + selection (17) + trust (15) + runtimes (12) + JSON→YAML migration (12) + `--locked` (9) + MCP bridge (7) + authoring (5) + capture (2) + unit (2) |
| `.github/workflows/` | `ci.yml` (test on Linux/macOS/Windows; fmt+clippy; docs + matrix drift gate; e2e walkthrough; TLS feature) + `release.yml` (cross-platform `oh` binaries + checksums on a version tag) |

## Dependencies

The default build is deliberately lean: `serde` / `serde_json` (with
`preserve_order`, so a config file's key order survives a round-trip),
`ed25519-dalek` / `sha2` / `getrandom` for trust (integrity verification is
always available, so it is not feature-gated), and `yaml-rust2` to read YAML.
Everything else that would pull weight is **opt-in**: `ffi` (uniffi,
for the Python binding), `mcp-http-tls` (rustls, for https on the bridge). The
line we draw: the *simple, fully-specified* wire formats — the stdio JSON-RPC
client, the http client, hex/base64, and YAML *emission* (a closed value set: no
anchors, no tags, no multi-document streams) — are hand-rolled to stay lean, but
YAML is *read* with a real parser (`yaml-rust2`), because YAML is a
genuinely complex spec and getting quotes / nesting / anchors right is what a
library is for, not something to hand-roll. We still emit our own frontmatter.

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
- **Packaging:** the crate carries a real version (`0.1.0`), `oh version` reports
  it for consumer version negotiation, and `release.yml` builds cross-platform
  `oh` binaries (linux x86_64/aarch64, macOS x86_64/aarch64, windows x86_64) with
  a `SHA256SUMS`, attaching them to a GitHub Release on a `v*` tag — enough to
  unblock the shell-out integration path. Still pending: **publishing the Python /
  Node bindings to PyPI / npm** with a prebuilt-wheel matrix (the publish steps
  need registry tokens the repo owner arms).
- **Deferred, tracked as follow-ups on the
  [roadmap epic](https://github.com/danplischke/open-harness/issues/1):** runtime
  **sandbox enforcement** of the permission manifest (real fs/net/exec confinement
  needs OS mechanisms), process-group kill for forking capabilities, full
  TUF-style registry roles, and live-recorded adapter fixtures. See
  `FEASIBILITY.md`.

License: MIT.
