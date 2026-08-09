# open-harness hook wire protocol — `hook@1`

**Status:** stable. **Protocol id:** `open-harness/hook@1`. **Version token:** `hook@1`.

This document is the normative contract between the open-harness dispatcher and a
**hook capability**. A hook capability is *any executable, in any language* that:

1. reads a **CanonicalPayload** as a single JSON document on **stdin**, and
2. writes a **Decision** as a single JSON document on **stdout**.

The machine-checkable schemas live next to this file:
[`schemas/canonical-payload.schema.json`](./schemas/canonical-payload.schema.json)
and [`schemas/decision.schema.json`](./schemas/decision.schema.json).

## Versioning

The version token is `hook@MAJOR[.MINOR]`. Compatibility is by **major** number:

- The dispatcher advertises its version in `CanonicalPayload.protocol`
  (`open-harness/hook@1`).
- A capability MAY declare the version it speaks via `protocol` in its manifest
  (e.g. `"protocol": "hook@1"`). If it does, the dispatcher refuses to run it
  when the **major** versions differ, and fails closed on blocking events. A
  capability that omits `protocol` is assumed compatible.
- **Minor** bumps are backward compatible: they only *add* optional fields.

## Forward compatibility (both directions)

- **Unknown fields are ignored, never rejected.** A capability MUST ignore
  payload fields it does not recognize; the dispatcher ignores decision fields it
  does not recognize. This is what lets minor versions add fields safely.
- `CanonicalPayload.raw` carries the untouched native payload as an escape hatch.
  Reading it is allowed but couples the capability to a specific harness and
  forfeits portability — prefer the normalized fields.

## CanonicalPayload (dispatcher → capability, stdin)

| field | type | required | meaning |
|---|---|---|---|
| `protocol` | string | yes | `open-harness/hook@1` |
| `harness` | string | yes | harness id, e.g. `claude-code`, `cursor` |
| `event` | object | yes | normalized event coordinate (below) |
| `blocking` | bool | yes | whether a `deny` will actually be honored here |
| `tool` | object | no | `{ name, input }` — present for tool events |
| `prompt` | string | no | present for prompt events |
| `cwd` | string | no | working directory, when the harness supplies it |
| `raw` | any | yes | the original native payload (escape hatch) |

`event` is a flat coordinate — **not** a single opaque name, because some
harnesses split "before a tool runs" into per-tool-class events:

| field | type | values |
|---|---|---|
| `phase` | string | `pre`, `post` |
| `subject` | string | `tool`, `model`, `prompt`, `session`, `subagent`, `task` |
| `tool_class` | string? | `any`, `shell`, `file_read`, `file_write`, `file_edit`, `mcp`, `web` |
| `boundary` | string? | `start`, `end` (session/subagent) |
| `task_kind` | string? | `start`, `resume`, `cancel` (task) |

`tool_class` describes **the call being made**, not the binding that caught it.
On a harness that fires one tool event for everything, the dispatcher classifies
the tool the harness reported before building this payload, so a capability bound
to `pre.tool.shell` is invoked for shell calls and reads `"tool_class":"shell"`
truthfully. Where a tool cannot be classified — an unrecognized name, or a
harness with no documented tool vocabulary — the class of the registration
stands; a capability that must be certain should check `tool.name` (or `raw`).

## Decision (capability → dispatcher, stdout)

| field | type | required | meaning |
|---|---|---|---|
| `decision` | string | no (default `allow`) | `allow`, `deny`, or `modify` |
| `reason` | string | no | human-readable justification; surfaced on `deny` |
| `context_append` | string | no | text to inject into the model's context |
| `modified_input` | any | no | replacement tool input when `decision` is `modify` |

Rules:

- An empty stdout is treated as `{"decision":"allow"}`.
- `deny` is only honored when `blocking` is true (a `deny` on a post-phase event
  cannot veto anything and is downgraded to `allow`).
- `modify` support is harness-dependent; where a harness cannot rewrite tool
  input, `modify` degrades to `allow`. Capabilities SHOULD treat `modify` as
  best-effort (declared and matrix-gated) rather than guaranteed.

## Composition

When several capabilities bind the same event, the dispatcher runs them in a
deterministic order and merges with **deny-wins** semantics: any `deny` wins;
otherwise the first `modify` applies; `context_append` values are concatenated.
Because the dispatcher owns this merge, ordering and conflict resolution are
identical on every harness.

## Failure policy

If a capability crashes, times out, or emits a Decision that fails validation,
the dispatcher **fails closed on blocking events** (synthesizes a `deny` with the
error as the reason) and degrades to `allow` on non-blocking events. This is why
a validation failure is never silently ignored.

## Validation

The dispatcher validates every Decision against the rules above before acting on
it (unknown fields excepted, per forward-compat). External capability authors can
validate their own I/O against the published JSON Schemas in any language.
