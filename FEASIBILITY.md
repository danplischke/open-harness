# Feasibility spike: cross-harness hook normalization + a language-agnostic stdio contract

**Question under test:** can a *single* normalized event model + a *single*
language-agnostic execution contract faithfully drive the hook surface of every
major coding harness — and where does that normalization break?

**Verdict:** **Feasible, with well-defined lossy edges.** The core mechanism
works and is proven by execution. The places it breaks are not fatal — they are
*declarable* (a capability states what it needs; install either satisfies it,
fans it out, degrades it, or refuses loudly). What a real build must accept is
that the normalization is a **spectrum with honest edges**, not a clean bijection.

This document records what was built, what it proves, and — the point of the
spike — exactly where it breaks.

---

## What was built (all runnable, in this repo)

- A **normalized event model** (`src/event.rs`): an event is a coordinate
  `(phase, subject, tool_class?)`, not a flat name. Reason under "Finding 1".
- **Ten harness adapters** (`src/adapters.rs`) — Claude Code, Codex, Gemini CLI,
  Cursor, Windsurf, Cline, OpenCode, Pi, Aider, Copilot — each encoding that
  harness's real (a) native event mapping, (b) deny signal, (c) registration format.
- A **single-entrypoint dispatcher** (`src/dispatch.rs`): one native hook per
  harness calls `oh-dispatch`, which fans out to capabilities, merges, re-encodes.
- Two **real capabilities in two languages** (`capabilities/`): a **Python**
  blocking secret-scanner and a **Node/TS** non-blocking context-injector.
- A **16-test conformance suite** (`tests/conformance.rs`), all passing, that
  encodes each harness's contract and runs the capabilities end-to-end.

**Method boundary (important):** the 8 proprietary harnesses are not installed
here, so their *native runtime* is not exercised. What is exercised for real is
the part a library actually owns: the event model, the dispatcher, the
stdio contract, decode/encode translation, and composition. Each harness's
native convention is encoded from its **documented** contract. Claude Code,
Gemini, Windsurf, Cline are high-confidence; **Codex and Cursor** (once
`MEDIUM CONFIDENCE`) have since been validated against primary docs and their
adapters corrected — Cursor's per-event snake_case payloads and `permission`
response, Codex's stdout `permissionDecision` deny (not exit-2), `hooks.json`
registration, and shell-only PreToolUse — with recorded payloads under
`tests/fixtures/`. Only Codex's exact stdin field names remain modeled rather
than captured. The *shape* of each divergence (which is what determines
feasibility) was robust regardless.

---

## The support matrix (generated: `oh-dispatch matrix`)

```
event \ harness   claude   codex   gemini  cursor    windsurf   cline   opencode  pi      aider  copilot
pre.tool.any      native   native  native  fanout×3  fanout×4   native  native    native  —      —
pre.tool.shell    native   native  native  native    native     native  native    native  —      —
post.tool.any     native   native  native  —         —          native  native    native  —      —
pre.model         —        —       native  —         —          —       —         —       —      —
pre.prompt        native   native  native  —         native     native  native    —       —      —
pre.session.start native   native  native  native    —          —       native    —       —      —
pre.subagent.startnative   native  —       —         —          —       —         —       —      —
pre.task.start    —        —       —       —         —          native  —         —       —      —
```

`native` = 1:1 · `fanout×N` = one normalized event must register on N native
events · `—` = no native target.

---

## Findings — where it breaks, and how bad

### Finding 1 — Tool-event granularity mismatch (fan-out). *Manageable; forced a model change.*
Claude/Codex/Gemini/Cline expose **one** generic pre-tool event with the tool
name in the payload. **Cursor and Windsurf split it by tool class**
(`beforeShellExecution`, `pre_write_code`, `pre_mcp_tool_use`, …). A flat
`PreToolUse` name cannot round-trip this. **Consequence:** the normalized event
had to become `(phase, subject, tool_class)`, and one normalized `pre.tool.any`
**registers on 3 (Cursor) / 4 (Windsurf) native events** — proven by
`oh-dispatch emit --harness cursor` and the fan-out tests. This works, but it
means "install a hook" is a 1→many operation the library must own.

### Finding 2 — Missing events / no target. *The hard ceiling; must be declarable.*
Some concepts exist on only one or two harnesses:
- **`model` hooks: Gemini only.** No analog on Claude/Codex/Cursor/etc.
- **`subagent` hooks: Claude/Codex only.**
- **`task` lifecycle: Cline only.**
- **Aider and Copilot have no hook mechanism at all.**
- **Pi core exposes essentially only `tool_call`** by design; everything else is
  build-your-own.

You cannot normalize a concept a harness does not have. The spike handles this
honestly: a capability **declares each binding `required` or not**, and `check`
resolves it to `installable — clean` / `fan-out` / `degraded (optional skipped)`
/ **`BLOCKED (missing required)`**. Real output:

```
secret-guard  aider   BLOCKED — missing required: pre.tool.any (Aider has no hook mechanism)
audit-note    aider   installable — degraded (skipped optional: pre.tool.any)
```

This is the single most important design consequence: **the capability matrix
is not documentation, it is an install-time gate.**

### Finding 3 — Deny-signal fragmentation. *Fully bridgeable — the central success.*
"Block this action" is spelled four incompatible ways. The dispatcher translates
one canonical `deny` into each, proven end-to-end:

| Family | Harnesses | Native signal | Verified |
|---|---|---|---|
| Exit-2 | Claude, Codex, Gemini, Windsurf | non-zero exit + stderr reason | `exit=2`, reason on stderr |
| Cursor-JSON | Cursor | stdout `{"permission":"deny",…}` | ✓ |
| Cline-JSON | Cline | stdout `{"cancel":true,…}` | ✓ |
| In-process | OpenCode, Pi | shim reads canonical decision, throws/returns | ✓ (via generated shim) |

The *same* Python secret-scanner produced a correct block in all four families.

### Finding 4 — Payload field divergence. *Mechanical; lives in `decode`.*
Field names differ (`tool_name`/`tool_input` vs Cursor `toolName`/`toolInput` vs
Windsurf nested `tool:{name,args}`). Each adapter's `decode` normalizes into one
`CanonicalPayload`; a `raw` passthrough is kept as an escape hatch. Proven by
`decode_translates_divergent_native_field_names`. Cost is per-harness mapping
code, not a design problem.

### Finding 5 — In-process harnesses need a generated shim. *Feasible; one extra artifact.*
OpenCode (JS plugin) and Pi (TS extension) don't exec a command — they call a
function. Bridging them means **generating a tiny plugin/extension that spawns
the capability over stdio** and applies the canonical decision. `oh-dispatch
emit --harness opencode` generates exactly this. So "8 harnesses" is really
"6 exec-native + 2 shim-hosted," and both paths reach the same capability binary.

### Finding 6 — Error policy must be owned centrally. *Solved; security-relevant.*
A capability that crashes must **fail closed** on a blocking event (deny), never
silently allow. The dispatcher enforces this (`e2e_capability_crash_fails_closed`).
Native harnesses do not agree on crash behavior, which is another reason to route
through one dispatcher rather than wiring capabilities in directly.

### Finding 7 — Composition/ordering is the dispatcher's job. *Solved; a genuine value-add.*
Two capabilities on the same event → deterministic **deny-wins merge** with
context concatenation (`merge_is_deny_wins…`, and the live demo where the Python
guard's deny overrides the Node auditor's allow). Because the dispatcher is the
single entrypoint, ordering/mediation is consistent across every harness — a
property none of them provide on their own.

### Finding 8 — Modify/mutation semantics are weak and non-portable. *Deferred; document the limit.*
`allow`/`deny` map cleanly. **Rewriting a tool's input** (`modify`) is
inconsistently supported (varies by harness, largely absent on the exit-2
family). The spike models `modify` but degrades it to `allow` where unsupported.
A real build should treat `modify` as a **capability-declared, matrix-gated**
feature, not assume it everywhere.

### Finding 9 — Windows portability. *Real constraint; plan for it.*
Cline hooks are macOS/Linux only, and the stdio-exec model relies on the Rust
launcher resolving interpreters (never on `#!` shebangs) to work on Windows. The
adapter surfaces this via `platform_note`. Not a blocker; a work item.

---

## What this means for the full build

**Proceed — but build it as a compiler with a capability matrix, not a leaky
"universal config."** Concretely:

1. **Keep the event coordinate `(phase, subject, tool_class)`.** A flat event
   name cannot represent Cursor/Windsurf and will trap you later.
2. **Keep the single-entrypoint dispatcher.** It is what makes ordering,
   fail-closed policy, and in-process/exec parity tractable, and it shrinks each
   adapter to "map events + translate I/O."
3. **Make the capability matrix an install-time gate** (`required` vs optional →
   BLOCKED / degraded / fan-out). This is the honest answer to Finding 2.
4. **Standardize on stdio-JSON for hooks and MCP for tools.** The spike confirms
   stdio-JSON is a real, already-converged substrate for hooks; do not invent a
   new execution model.

### Applying this to the wider scope you added
- **Skills unification across vendors** — *easier than hooks.* `SKILL.md` is
  already a near-standard (Claude `.claude/skills/`, Codex `.agents/skills/`,
  Cursor `.cursor/skills/`, Windsurf `.windsurf/skills/`); unification is a
  path + frontmatter transform with **no execution-model problem**. It belongs in
  the same adapter/matrix framework as a second capability *kind*, and it is a
  low-risk early win.
- **Rust core + Python/TS bindings** — the core here is already binding-shaped
  (small, data-in/data-out). Recommend **`uniffi`** (Rust → Python/Kotlin/Swift
  from one interface) plus **`napi-rs`** or a WASM build for TS. Note the split:
  *capabilities* stay language-agnostic via stdio; *bindings* are for embedding
  the open-harness engine into a host program.
- **Sync from personal repos → custom harnesses** — this is the L4 "sources"
  layer: capability sets pulled from git, pinned in a lockfile, composed into a
  profile. The dispatcher/matrix built here is the runtime it composes onto.
- **Open-ended future utilities** — the capability *kind* is deliberately not a
  closed enum; new kinds (rules, commands, permissions, …) plug into the same
  adapter + matrix + dispatcher spine.

### Deferred (not in this spike)
Since the spike, the runtime was hardened (timeouts, output caps, error
taxonomy, fail policy — #3), all six capability kinds landed (#6–#11), execution
went cross-platform with a Windows CI matrix (#4), and Codex/Cursor were
validated against docs (#5). Still deferred: capability **sandboxing** and
process-group kill; **registry signing** (a real supply-chain surface once
capabilities are distributable); the **TypeScript/napi** binding; the MCP→CLI
bridge runtime (#19); and a **live** (not doc-derived) capture against a real
Codex/Cursor install once available.

---

## Reproduce

```sh
cargo test                 # 16 conformance tests
cargo run -- matrix        # the support grid above
cargo run -- check         # per-capability installability across harnesses
bash examples/demo.sh      # end-to-end deny across all four signal families
cargo run -- emit --harness cursor   # see the fan-out registration
```
