# Language bindings

The one Rust core is exposed to other languages via [uniffi](https://mozilla.github.io/uniffi-rs/).
Bindings are **feature-gated** (`--features ffi`) so the default build and the CLI
never depend on uniffi.

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

Planned — tracked in the issue tracker. The same `api` facade is the target
surface; a `napi-rs` (or WASM) binding will mirror it.
