# Authoring a capability

## Scaffold

The fastest start is `oh scaffold`, which writes a ready-to-edit capability
directory (a **hook** scaffold is runnable immediately):

```sh
oh scaffold --kind hook --lang python --id my-guard      # or --lang node | typescript | bash
oh scaffold --kind permission --id safe-shell
oh scaffold --kind skill --id commit-style
oh scaffold --kind agent --id database-engineer
oh scaffold --kind instructions --id project-conventions
```

Kinds: `hook`, `skill`, `rule`, `command`, `tool`, `permission`, `agent`,
`instructions`. `--lang` applies to hooks (the only kind with a runtime).

The **document kinds** (skill, agent, command, rule, instructions) scaffold as a
**single file** — `SKILL.md`, `AGENT.md`, `COMMAND.md`, `RULE.md`,
`INSTRUCTIONS.md` — whose YAML frontmatter *is* the manifest. There is no sidecar
`capability.yaml`: you author the artifact itself, and open-harness compiles that
one file into each harness's dialect. (Hooks and tools keep a `capability.yaml` —
they have no body and need structured `run` / `server` config.)

### Hook languages

Because a hook is just *an executable that reads stdin and writes stdout*, the
language is a per-capability choice — they are all peers:

| `--lang` | Writes | Runs via | Notes |
|---|---|---|---|
| `python` | `hook.py` | `python3 hook.py` | no deps |
| `node` (`js`) | `hook.mjs` | `node hook.mjs` | plain JavaScript, no deps |
| `typescript` (`ts`) | `hook.ts` | `node hook.ts` | **typed**, run directly via Node's type stripping (Node ≥ 22.18 / 23.6) — no build step, no runtime dependency |
| `bash` (`sh`) | `hook.sh` | `sh hook.sh` | no deps |

The `typescript` starter carries a small inline subset of the `hook@1` types so
the file is self-contained. For the full canonical `CanonicalPayload` / `Decision`
types from `@open-harness/node` plus an in-process test, scaffold a package with
[`--project`](#a-typescript-npm-package---project) instead.

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
built capability (`capability.yaml` + `dist/hook.js`) is plain Node with no
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

```yaml
id: my-guard
kind: hook
protocol: hook@1
run:
  command: python3
  args: [hook.py]
events:
  - phase: pre
    subject: tool
    tool_class: any
```

### Failure policy & limits

Per binding, `on_error` chooses `auto` (fail-closed on blocking events),
`fail_closed`, `fail_open`, or `pass_through`. `timeout_ms` overrides the runtime
timeout. The runtime caps output and classifies every failure (`spawn-error`,
`timeout`, `non-zero-exit`, `bad-json`, `protocol-mismatch`, `output-cap`).

## Document kinds — single file, frontmatter is the manifest

Skills, agents, commands, rules, and instructions are authored as one Markdown
file whose YAML frontmatter carries the metadata and whose body is the content.
`id` defaults to the directory name and `kind` to the filename (`SKILL.md` →
skill), so a skill is just:

```markdown
---
description: Teaches this repo's commit conventions.   # capabilities/commit-style/SKILL.md
allowed-tools: [Read]
---
# Commit style
Use Conventional Commits…
```

A **subagent** (`AGENT.md`) — tool names are canonical (`read`/`grep`/`bash`/…)
and lower to each harness's tokens; `model` is portable, `model-tier` is carried
but resolved by *you*, not the compiler:

```markdown
---
description: "Use for schema and query work: migrations and indexes."
model: claude-sonnet-5
tools: [read, grep, bash]
permissions: { bash: ask }
mode: subagent
---
You are a database engineer. Review migrations for safety…
```

An always-on **instructions** brief (`INSTRUCTIONS.md`) needs no frontmatter at
all — it installs as `CLAUDE.md` / `AGENTS.md` / `GEMINI.md` /
`.github/copilot-instructions.md`. Frontmatter values that look like numbers or
booleans (a `version`, a leading `-`) should be quoted to stay strings.

### Per-harness overrides

Any document capability may carry an `overrides` block to tune one target without
forking — replace the output `path`, the `tools` list (native tokens verbatim),
or merge extra `frontmatter`. In a single file it's just nested frontmatter
(block or flow YAML both parse):

```markdown
---
description: Use for schema and query work.
tools: [read, grep, bash]
overrides:
  copilot:
    tools: [readFile, search, runInTerminal]
    frontmatter: { user-invocable: true }
---
```

## Structured kinds — `capability.yaml`

Hooks and tools (and, if you prefer, any kind) use a `capability.yaml` manifest —
they have no body and need structured config. A permission policy, for example:

```yaml
id: safe-shell
kind: permission
permission:
  default: ask
  rules:
    - tool: bash
      pattern: "git *"
      verdict: allow
```

`capability.yaml` works for every kind and, when both are present in a directory,
wins over a single-file document. `oh check` shows how each capability lands on
every harness (clean / degraded / blocked); `oh emit --harness <h>` prints the
native artifacts.

### When a *document* kind wants a manifest

For the five document kinds the two forms are equally expressive — the only
manifest field the single-file reader will not lift is `permissions`, and a
document kind has no use for a sandbox manifest. So the single-file form is the
default, and a manifest should earn its place. Two reasons that do:

* **`body_file`** — the body is long enough to want its own reviewable file, and
  burying it in a YAML block scalar helps nobody.
* **`overrides`** — harness-specific frontmatter has to be declared per harness
  so it lands on the one that reads it and is stripped from the rest. Emitting a
  Claude-only key onto all six skill targets leaves five files carrying
  something nothing there understands.

`capabilities/postgres-review/` is the worked example of both:

```yaml
kind: skill
skill:
  allowed-tools: [read, grep, bash]
  body_file: PROCEDURE.md          # not SKILL.md — see below
overrides:
  claude-code:
    frontmatter:
      user-invocable: true         # Claude reads this; nobody else does
```

Name the body file something that is *not* a recognized document name. A
`RULE.md` sitting next to a `capability.yaml` is readable as either form, and
while `discover` resolves that in the manifest's favour, a directory that reads
two ways at a glance is worth avoiding. `tests/authoring.rs` gates this, along
with the rule that no kind may be demonstrated *only* as a manifest.

## Package, sign, share

Add `version` and `permissions` to the manifest, then sign it so consumers can
verify integrity + authorship:

```sh
oh keygen --out author.key
oh sign   --capability capabilities/my-guard --key author.key
oh verify --capability capabilities/my-guard --trust trust.yaml
```

Consumers compose your capability into a profile from a local path or a git repo,
resolve it to a pinned `open-harness.lock`, and `oh sync` it into their project.
