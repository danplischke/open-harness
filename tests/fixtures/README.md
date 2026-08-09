# Recorded native hook fixtures (#5)

Real native hook stdin payloads for the two harnesses whose adapters were
`MEDIUM CONFIDENCE`. A live Codex/Cursor install isn't available in this repo, so
these are **doc-derived fixtures** captured from primary documentation rather
than a live capture — the adapter tests in `tests/conformance.rs` decode them and
assert the canonical shape, and encode a deny and assert each harness's real
native response format.

## Capturing from a live harness — `oh capture` (#24)

When you *do* have a harness installed, `oh capture` records its real native
payload straight into a fixture. Wire it as that harness's hook entrypoint; it
writes the raw stdin it receives to `--out`, then **allows** (exit 0) so nothing
is disrupted:

```sh
# as Cursor's beforeShellExecution hook, then trigger a shell command once:
oh capture --harness cursor --event pre.tool.shell \
           --out tests/fixtures/cursor/beforeShellExecution.stdin.json
```

The captured file is the same raw-native shape as the fixtures here, so it drops
straight into the conformance suite. This is how a doc-derived fixture above gets
upgraded to a live one — the recording is a manual step (it needs the real
harness), but the tooling and the round-trip test (`tests/capture.rs`) ship now.

### The provenance sidecar

A payload recorded off a live install and one transcribed out of a vendor's docs
are *identical bytes*. The fixture alone therefore cannot support the claim that
an adapter was verified against the real thing, so every `oh capture` also writes
a sidecar next to the fixture:

```jsonc
// cursor/beforeShellExecution.provenance.json
{ "kind": "live-captured", "harness": "cursor", "event": "pre.tool.shell",
  "harness_version": "Cursor 1.7.44",          // from --label
  "recorded_by": "oh 0.1.0", "recorded_at": "2026-08-09T09:12:44Z" }
```

That sidecar is the *only* thing that lets a harness declare
`Provenance::LiveCaptured` in `src/adapters.rs`: `tests/provenance.rs` holds every
adapter's declaration against what is actually committed here, and fails if a
declaration claims more evidence than exists — or less (a fixture landing while
the adapter still says `doc-only` is also a failure, so the matrix cannot
understate itself either).

To upgrade a harness end to end:

1. `oh capture --harness H --event E --label "<harness version>" --out tests/fixtures/H/<native-event>.stdin.json`
2. commit the fixture *and* its sidecar,
3. change that harness's arm in `Harness::provenance()` to `LiveCaptured`,
4. `bash docs/gen-matrix.sh` and commit the regenerated matrix.

Steps 2 and 3 are enforced against each other; step 4 is enforced by the CI
matrix drift gate. `oh matrix --provenance` prints the current state.

## Cursor

Per-event payloads are **snake_case and event-specific** (not a uniform
`tool_name`/`tool_input`), and the response is a `permission` object.

- `cursor/beforeShellExecution.stdin.json` — `{command, cwd, hook_event_name, …}`
- `cursor/beforeReadFile.stdin.json` — `{file_path, content, hook_event_name, …}`
- `cursor/beforeMCPExecution.stdin.json` — `{tool_name, tool_input, hook_event_name, …}`

Response (stdout): `{ "continue": bool, "permission": "allow|deny|ask", "userMessage": …, "agentMessage": … }`.

Sources: Cursor Hooks documentation & deep-dives —
<https://blog.gitbutler.com/cursor-hooks-deep-dive>,
<https://github.com/johnlindquist/cursor-hooks>.

## Codex

Event names mirror Claude (`PreToolUse`/`PostToolUse`), but:

- the deny signal is a **stdout `permissionDecision` object**, not exit code 2;
- hooks are registered in **`~/.codex/hooks.json`** (JSON), enabled by
  `[features].codex_hooks = true`;
- **`PreToolUse` fires for the shell tool only** — `apply_patch`, `Edit`/`Write`/
  `Read`, web fetch, and MCP calls bypass it.

- `codex/preToolUse.stdin.json` — modeled on the Claude-style `{tool_name,
  tool_input}` schema (the exact upstream field names are the one part not yet
  captured from a live install).

Sources: Codex hooks references —
<https://agenticcontrolplane.com/blog/codex-cli-hooks-reference>,
<https://developers.openai.com/codex/agent-approvals-security>,
<https://deepwiki.com/openai/codex/3.11-hooks-system>.
