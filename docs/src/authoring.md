# Authoring a capability

## Scaffold

The fastest start is `oh scaffold`, which writes a ready-to-edit capability
directory (a **hook** scaffold is runnable immediately):

```sh
oh scaffold --kind hook --lang python --id my-guard      # or --lang typescript | bash
oh scaffold --kind permission --id safe-shell
oh scaffold --kind skill --id commit-style
```

Kinds: `hook`, `skill`, `rule`, `command`, `tool`, `permission`. `--lang` applies
to hooks (the only kind with a runtime).

### A TypeScript npm package (`--project`)

For a full TypeScript authoring loop — typed against the contract and testable
before you install anywhere — scaffold a hook as an **npm package**:

```sh
oh scaffold --project --id my-cap
cd capabilities/my-cap
npm install                 # typescript + @types/node
npm link @open-harness/node # types + the in-process test (see below)
npm run build               # tsc → dist/hook.js
npm test                    # runs THIS capability through the core, in-process
```

The package writes `src/hook.ts` typed against `CanonicalPayload` / `Decision`
from `@open-harness/node`, and a `test.mjs` that dispatches the built capability
through the core **in-process** via the native addon — no separate harness, no
install step, just `npm test`.

`@open-harness/node` is used for the **types and the dev-time test only** — the
built capability (`capability.json` + `dist/hook.js`) is plain Node with no
runtime dependency on it. Because the addon isn't published to npm yet, the
package doesn't list it as a dependency (that would 404 on `npm install`); you
supply it with `npm link` (`build.sh` in `bindings/node`, then `npm link` there).

## The hook contract

A hook is *any executable in any language* that reads a `CanonicalPayload` as
JSON on stdin and writes a `Decision` as JSON on stdout:

```jsonc
// stdin  (CanonicalPayload)
{ "protocol": "open-harness/hook@1", "harness": "claude-code",
  "event": {"phase":"pre","subject":"tool","tool_class":"shell"},
  "blocking": true, "tool": {"name":"Bash","input":{"command":"…"}} }

// stdout (Decision)
{ "decision": "deny", "reason": "potential secret in tool input" }
```

`decision` is `allow` | `deny` | `modify`. On a blocking event a `deny` is
translated into each harness's native block signal; `context_append` adds model
context. The wire protocol is frozen as `hook@1` (see `spec/`).

The manifest declares how to run it and which events it binds:

```jsonc
{ "id": "my-guard", "kind": "hook", "protocol": "hook@1",
  "run": { "command": "python3", "args": ["hook.py"] },
  "events": [ { "phase": "pre", "subject": "tool", "tool_class": "any" } ] }
```

### Failure policy & limits

Per binding, `on_error` chooses `auto` (fail-closed on blocking events),
`fail_closed`, `fail_open`, or `pass_through`. `timeout_ms` overrides the runtime
timeout. The runtime caps output and classifies every failure (`spawn-error`,
`timeout`, `non-zero-exit`, `bad-json`, `protocol-mismatch`, `output-cap`).

## Generative kinds

Skills, rules, commands, tools, and permissions declare a config block instead of
a `run`. For example a permission policy:

```jsonc
{ "id": "safe-shell", "kind": "permission",
  "permission": { "default": "ask",
    "rules": [ { "tool": "bash", "pattern": "git *", "verdict": "allow" } ] } }
```

`oh check` shows how each capability lands on every harness (clean / degraded /
blocked). `oh emit --harness <h>` prints the native artifacts.

## Package, sign, share

Add `version` and `permissions` to the manifest, then sign it so consumers can
verify integrity + authorship:

```sh
oh keygen --out author.key
oh sign   --capability capabilities/my-guard --key author.key
oh verify --capability capabilities/my-guard --trust trust.json
```

Consumers compose your capability into a profile from a local path or a git repo,
resolve it to a pinned `open-harness.lock`, and `oh sync` it into their project.
