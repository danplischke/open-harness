# @open-harness/node

Node/TypeScript bindings for the open-harness core — a **napi-rs native addon**
exposing the stable API (`plan`, `dispatch`, `verify`, …) to Node and Bun.

## Native, not WASM

The core is filesystem- and subprocess-oriented: `dispatch` spawns capabilities
as subprocesses (the language-agnostic stdio contract), and `plan` / `verify`
read files. None of that works in `wasm32-unknown-unknown` (no processes, no
fs), so a native addon is the right vehicle — which is also where the harness
plugin shims live. In particular, an **OpenCode / Pi plugin can host the core
in-process** (see `opencode-plugin.example.mjs`) instead of shelling out to the
`oh` CLI for orchestration.

## Build

```sh
bash build.sh            # cargo build --release + place open-harness.<platform>-<arch>.node + run the example
```

This is a standalone Cargo crate (it references Node's C-API symbols, so it must
not be linked into the `oh` binary); the repo's `cargo build` never
touches it.

## Use

```js
import { harnesses, plan, dispatch, verify } from "@open-harness/node";

// Generative: plan a capability for a harness.
const manifest = JSON.stringify({ id: "demo", kind: "skill", skill: { body: "Be concise." } });
plan(manifest, ".", "claude-code");        // -> { installability, artifacts, notes, ... }

// Runtime: run the dispatcher (spawns capabilities), get the native response.
const payload = JSON.stringify({ tool_name: "Bash", tool_input: { command: "echo hi" } });
dispatch("opencode", "pre.tool.shell", payload, "./capabilities");   // -> { exit_code, stdout, ... }

// Trust: verify a capability directory against a trust store.
verify("./capabilities/secret-guard", JSON.stringify({ keys: [] }));  // -> { status, trusted, ... }
```

`harnesses()`, `kinds()`, `protocolVersion()`, `planAll()`, `dispatchWithLimits()`,
and `validateDecision()` are also exported. Complex results cross the boundary as
JSON and are parsed for you; the "arbitrary data" inputs (a manifest, a native
payload, a decision, a trust store) stay JSON strings — the same string boundary
the Python (uniffi) binding uses. Types are in `index.d.ts`.

## Status

Verified on Linux in this repo. Publishing to npm and prebuilt binaries for
macOS/Windows/arm (the release matrix) is the remaining part of #14 — it needs a
publish token and the per-platform build matrix, which `build.sh` + the CI job
are scaffolding for.
