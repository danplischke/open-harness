# Contributing to open-harness

Thanks for helping build the cross-harness capability layer. This is a
feasibility spike evolving into a platform — contributions that keep it **honest**
(no lowest-common-denominator; degradation is declared, never silent) are
especially welcome.

## Development

```sh
cargo build                 # default build — lean, no FFI/crypto-optional deps beyond core
cargo test                  # the conformance + profile + trust + mcp suites
cargo fmt --check           # formatting (CI-enforced)
cargo clippy --all-targets -- -D warnings   # lints (CI-enforced)
```

CI runs the test suite on Linux, macOS, and Windows, plus fmt/clippy and a docs
build. Match the surrounding code's comment density and idioms.

## Adding a capability kind

Adding a kind touches only its own module — the dispatcher core is untouched
(proven for all six current kinds):

1. Implement the `Kind` trait in `src/kinds/<kind>.rs` (`plan(cap, harness) ->
   KindPlan`), returning `Clean` / `Degraded(reason)` / `Unsupported(reason)` and
   the artifacts that realize it.
2. Register it in `kind_impl` (`src/kind.rs`) and `KindId`.
3. Add an example capability under `capabilities/` and tests in
   `tests/conformance.rs`.
4. Document it in the README and `docs/`.

## Adding / changing a harness adapter

Harness conventions live in `src/adapters.rs`. If you change event mapping,
**regenerate the support matrix** so it can't drift:

```sh
bash docs/gen-matrix.sh     # rewrites docs/src/harness-matrix.md from the adapters
```

CI fails if the committed matrix differs from the code. For harnesses we can't
install, cite the primary docs and, where possible, add a recorded payload under
`tests/fixtures/` (see Codex/Cursor).

## Authoring a capability

Use `oh scaffold` — see [the authoring guide](./docs/src/authoring.md). A hook
scaffold is runnable immediately.

## Commit hygiene

Keep the default build dependency-light. Optional surfaces (uniffi Python
bindings, the napi Node addon) are feature-gated or separate crates so
`cargo build` never pulls them.
