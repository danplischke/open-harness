# Importing plugins

Claude Code plugins are a real, populated ecosystem, and the layout is becoming
a de-facto standard. A `plugin` source imports one as capabilities:

```yaml
sources:
  - plugin:
      url: https://github.com/thedotmack/claude-mem
      rev: v13.14.0
```

```
warning: plugin 'claude-mem': MCP server 'mcp-search' imported as a portable tool,
  but its command (`node`) may resolve against Claude Code's own plugin install
warning: plugin 'claude-mem': 6 hook(s) (Setup, SessionStart, UserPromptSubmit,
  PostToolUse, PreToolUse, Stop) imported as a claude-code-native capability —
  they are not `hook@1` capabilities and will be reported Unsupported on every
  other harness
profile 'with-claude-mem' → 21 capabilities across 4 harness(es)
      · mem-search 13.14.0 [skill]      … 19 skills
      · mcp-search 13.14.0 [tool]
      · claude-mem-hooks 13.14.0 [native]
```

Importing is reverse-compilation — the job the adapters do forwards, run
backwards from one harness's dialect into the canonical model. So the honesty
rule matters more here than anywhere else, because **a plugin is not uniformly
portable.**

## What travels, and what doesn't

| in the bundle | imports as | travels? |
|---|---|---|
| `skills/<id>/SKILL.md` | `skill` | **yes** — every harness with skills |
| `commands/<id>.md` | `command` | **yes** |
| `agents/<id>.md` | `agent` | **yes** |
| `.mcp.json` | `tool` (one per server) | **mostly** — MCP is the shared standard |
| `hooks/hooks.json` | `native` | **no** |

**Skills, commands and agents** are already the shapes open-harness defines, so
importing them is a load rather than a translation — and the payoff is immediate:
a plugin's skills stop being Claude-only.

**`.mcp.json`** rides the shared standard, so the *config* genuinely travels.
The caveat, reported every time: a plugin's server command routinely resolves
against that harness's own plugin cache, so the config being portable does not
guarantee the server starts elsewhere. That is stated rather than pretended away.

**`hooks/hooks.json` does not travel at all.** It is opaque shell wired to
Claude's native hook schema — frequently hunting Claude's own plugin cache at
runtime — not the `hook@1` stdin/stdout contract. Nothing can make it portable.

## The `native` kind

Which leaves a choice with three options: drop the hooks silently, claim they
compile, or carry them as what they are and say so. open-harness takes the third,
via a capability kind that exists for exactly this:

```yaml
kind: native
native:
  harness: claude-code
  reason: Claude Code hook bundle — opaque commands against Claude's own schema
  artifacts:
    - path: .claude/hooks/claude-mem.json
      body: "{ … }"
```

On the named harness the artifacts are written **byte-for-byte** — open-harness
does not interpret them. Everywhere else the plan is `Unsupported`, carrying the
reason, so `oh check` shows a blocked cell with an explanation rather than a
silent absence:

```
→ claude-mem-hooks on cursor: Claude Code hook bundle from the 'claude-mem'
  plugin — opaque commands against Claude's native hook schema, not the
  open-harness `hook@1` contract, so it has no equivalent here
```

Even on its own harness a `native` capability plans as **Degraded**, not Clean.
It installs, but calling it clean would imply a portability it does not have.

`native` cannot be scaffolded. It is what importing produces, not a template for
writing unportable config by hand — `oh scaffold --kind native` refuses and says
so.

## Locating the plugin

A repository is either a plugin itself (`.claude-plugin/plugin.json` at the root)
or a **marketplace** publishing several, each at its own subdirectory. Both work:

```yaml
# a marketplace publishing exactly one plugin resolves without being named
- plugin:
    url: https://github.com/thedotmack/claude-mem
    rev: v13.14.0

# several? name the one you want
- plugin:
    url: https://github.com/vendor/plugins
    rev: main
    name: alpha
```

A marketplace with several plugins and no `name` is an error listing what it
publishes. Guessing which of somebody's plugins you meant is not a decision to
make silently.

Omitting `rev` treats `url` as a **local path**, so a plugin checked out beside
your profile imports with no git and no network.

## Selection and namespacing

A plugin source takes the same [`select`](./dependencies.md#selecting-part-of-a-source)
block as every other source, which makes the obvious want a one-liner — a
plugin's skills without its native hook bundle:

```yaml
- plugin:
    url: https://github.com/thedotmack/claude-mem
    rev: v13.14.0
    select:
      kinds: [skill]
      include: [mem-search, timeline-report]
```

The namespace comes from the **plugin's own name**, not `<owner>/<repo>` — its
published identity rather than where it happens to be hosted. So the capabilities
above are `claude-mem/mem-search` and `claude-mem/timeline-report`, and the
version on each is the plugin's, unless a document declares its own.

## Trust

A plugin source is somebody else's repository, so everything in
[Dependencies → Transitive acquisition](./dependencies.md#transitive-acquisition)
applies. The bundle is content-addressed and locked like any other source: the
lock pins the resolved commit and a sha256 over each imported capability's tree,
so `--locked` catches an upstream change.

Note what importing does **not** do: it never runs anything from the bundle. A
plugin's hooks are copied, not executed, and whether you then wire them into
Claude Code is your decision — made with the reason for their unportability in
front of you.
