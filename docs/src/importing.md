# Importing an existing project

Every other page here starts from an empty directory: write a capability,
compose a profile, sync it out. Real projects are not empty. They have a
`.claude/skills/`, a handful of `.cursor/rules/`, an `AGENTS.md` somebody wrote
six months ago, and a `.mcp.json`. Asking for all of that to be re-authored
before open-harness can be tried is the difference between a tool a team can
adopt on a Tuesday and one they file under "interesting".

`oh import` reads it instead.

```sh
oh import --dry-run                       # what your config would become
oh import                                 # write it to ./capabilities
oh import --harness cursor --out caps     # just .cursor/, somewhere else
```

It is the exact inverse of [the kinds](./concepts.md): where each `Kind` renders
one normalized capability into eleven native dialects, `oh import` reads those
dialects back.

## What it reads

| What you have | Becomes |
|---|---|
| `.claude/skills/*/SKILL.md`, `.cursor/skills/`, `.agents/skills/`, `.windsurf/skills/`, `.github/skills/`, `.opencode/skills/` | a `skill` capability |
| `.cursor/rules/*.mdc`, `.windsurf/rules/*.md`, `.clinerules/*.md`, `.claude/rules/*.md`, `.github/instructions/*.instructions.md` | a `rule` capability, with a normalized activation |
| `.claude/commands/`, `.cursor/commands/`, `.opencode/commands/`, `.github/prompts/*.prompt.md`, `.windsurf/workflows/`, `.clinerules/workflows/`, `.pi/prompts/` | a `command` capability, with portable argument syntax |
| `.claude/agents/*.md`, `.opencode/agents/*.md`, `.github/agents/*.agent.md` | an `agent` capability |
| `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.github/copilot-instructions.md` | an `instructions` capability |
| `.mcp.json`, `.cursor/mcp.json`, `.vscode/mcp.json` | a `tool` capability per server |
| `.claude/settings.json` → `permissions` | a `permission` capability |
| `.claude/settings.json` → `hooks`, `.cursor/hooks.json` | a `native` capability, **pinned to that harness** |

Portable kinds are written as [single-file capabilities](./authoring.md) —
`SKILL.md`, `RULE.md`, `COMMAND.md`, `AGENT.md`, `INSTRUCTIONS.md` — so the
result is something you can read and edit, not a generated blob.

## The translations it performs

Two of these are real work, and both are reported per item rather than done
behind your back.

**Rule activation.** Five harnesses spell the same four activations five
different ways. Cursor's `alwaysApply` + `globs` + `description` triple, a
Windsurf `trigger:`, a Copilot `applyTo:`, a Cline/Claude `paths:` array — all
read back to `always` / `glob` / `agent_requested` / `manual`. Re-emitting then
produces the right spelling everywhere, including the loud `Degraded` note where
a target harness cannot express the activation the source had.

**Command arguments.** The portable body uses `{{args}}` / `{{argN}}`; every
markdown harness writes `$ARGUMENTS` / `$N`. Importing without converting would
leave a body that renders a literal `$ARGUMENTS` onto Gemini and Pi, which read
braces. Pi already writes the portable spelling and is left alone.

**Tool vocabularies** fold back too: Claude's `Bash(psql:*)` becomes
`bash(psql:*)`, and OpenCode's `permission:` map — which gates an agent's tools
instead of listing them — becomes canonical tool verdicts, so an agent imported
from OpenCode re-emits correctly onto Claude and Copilot.

## What it refuses to do

An importer is the easiest place in a tool like this to lose something silently.
It looks like it worked; the rule that stopped applying is noticed months later.
So every file found under a known native path ends up in the report as exactly
one of three things.

**Imported.** Normalized, with any translation named.

**Not portable.** Hooks are the case that matters. A `.claude/settings.json`
hook is a shell command wired to Claude's own hook schema — its command would
receive a payload it cannot read on any other harness. Reshaping it into a
`hook@1` capability would be a lie, so it is carried verbatim as a
[`native`](./concepts.md) capability pinned to Claude and reported `Unsupported`
on the other ten. The report says so, and points at
`oh scaffold --kind hook` for making it genuinely portable.

**Skipped, with the reason.** Gemini's commands are TOML, which open-harness
does not read; half-parsing them with a regex would be worse than saying so.

Three more rules follow from the same principle:

- **Frontmatter that cannot travel is named.** A dropped
  `disable-model-invocation` changes how a subagent behaves, so the item says
  which keys were left behind and offers `overrides.<harness>` as the place to
  put them back.
- **Ambiguity is declared, not resolved.** Copilot writes no `applyTo` for both
  manual *and* always-on rules. The import takes `manual` — the narrower reading,
  since guessing "always" would widen where the rule applies — and tells you it
  guessed.
- **Nothing is overwritten.** An id that already exists under `--out` is reported
  as a collision and left exactly as it was. An importer that clobbers the
  capability you hand-edited is worse than one that refuses.

The same-id case is worth spelling out. A skill in both `.claude/skills/x/` and
`.cursor/skills/x/` is one capability: if the two files are identical they merge,
and the item records both readers. If they *differ*, they do not merge — the
second is held back, naming the file it conflicts with, because picking one
silently would drop whichever version you actually meant.

A skip means your project has something open-harness now does not, so
`oh import` exits non-zero when anything was left behind. A partial import must
not read as a complete one.

## After importing

```sh
oh check --capabilities capabilities      # how each one lands on all eleven harnesses
oh init                                   # a profile pointing at them
oh sync --into .                          # write them back out, everywhere
```

That last step is the payoff and the sanity check at once: the Cursor rule you
imported now also exists as a Windsurf `trigger: glob`, a Copilot `applyTo:` and
a Cline `paths:` array — and `oh check` tells you, per harness, wherever that
could not be done cleanly.
