# Language bindings

The one Rust core is exposed to other languages: **Python** via
[uniffi](https://mozilla.github.io/uniffi-rs/) and **Node/TypeScript** via
[napi-rs](https://napi.rs/). Both keep the default `cargo build` and the CLI
dependency-free — Python is feature-gated (`--features ffi`), and the Node addon
is a separate crate (`bindings/node/`) that the root build never touches.

## Python (uniffi)

```sh
bash bindings/build.sh
```

That builds the FFI cdylib, generates `bindings/python/open_harness.py`, copies
`libopen_harness.so` next to it, and runs `bindings/python/example.py`.

The generated `.py` and the `.so` are build artifacts (git-ignored); regenerate
them with the script above. `example.py` is the committed source and doubles as a
smoke test — it embeds the core to list harnesses, plan a capability, run the
dispatcher (blocking a secret), and exercise the error path.

The FFI surface mirrors `open_harness::api`: `protocol_version`, `harnesses`,
`kinds`, `plan`, `plan_all`, `dispatch`, `validate_decision`, `negotiate`, with
`Plan` / `PlanArtifact` / `DispatchResult` records and an `ApiError` exception.
Arbitrary data (manifests, native payloads, decisions) crosses as JSON strings.

## TypeScript / Node (napi)

```sh
bash bindings/node/build.sh
```

Builds the `napi-rs` native addon (`open-harness.<platform>-<arch>.node`) and runs
`bindings/node/example.mjs`. See [`bindings/node/README.md`](./node/README.md).

Native, **not** WASM: the core is filesystem- and subprocess-oriented (`dispatch`
spawns capabilities; `plan`/`verify` read files), none of which works in
`wasm32-unknown-unknown`. This is also where the harness plugin shims live — an
**OpenCode / Pi plugin can host the core in-process** (see
`bindings/node/opencode-plugin.example.mjs`) instead of shelling out to the
`oh-dispatch` CLI. The surface mirrors `open_harness::api` (`harnesses`, `kinds`,
`protocolVersion`, `plan`, `planAll`, `dispatch`, `dispatchWithLimits`,
`validateDecision`, `verify`); complex results cross as JSON. Types in
`index.d.ts`.

Remaining for #14: publishing to npm with prebuilt binaries for the full
platform matrix (needs a token + the release build matrix; `build.sh` and the CI
`node-addon` job are the scaffolding). Verified on Linux today.
