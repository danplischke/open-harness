# Trying a source before you adopt it

Every other command in this book answers for capabilities you have already
committed to. `oh add` edits your profile, `oh resolve` pins it, `oh sync`
writes it into your harnesses. To judge a stranger's capability you had to adopt
it first and read the diff afterwards — which is backwards for a tool whose
claim is that you can see what lands on your machine *before* it lands.

`oh try` is that step:

```sh
oh try git+https://github.com/me/review-skill@v1
```

```text
source   [git] https://github.com/me/review-skill @ 9f2c1ab3d4e5
targets  claude-code cursor (from ./open-harness.yaml)
scope    project

capability  me/code-review  (Code Review)  1.2.0  [skill]
  signature unsigned
  asks      network any · exec rg,git · writes ./.review-cache/**
  runtime   node — ready
  writes
    .claude/skills/code-review/SKILL.md  [claude-code]
    .cursor/skills/code-review/SKILL.md  [cursor]

nothing was written. to keep it:
  oh add git+https://github.com/me/review-skill@v1
  oh sync --into .
```

## What it writes

Nothing. Not your profile, not `open-harness.lock`, not one byte of harness
configuration, not a file beside the source it read. The single exception is the
fetch cache under `~/.open-harness/cache/`, which is where a git clone or a
verified archive is kept so a second `try` — or the `oh add` that follows — is a
fetch rather than a clone. A local source does not even touch that.

This is checked rather than asserted: `tests/try.rs` snapshots the directory
`try` was run in before and after and fails on any new entry, and separately
fails if anything appears in the home tree outside `.open-harness/cache/`.

## What it tells you

| Line | Why it is there |
|---|---|
| `source … @ <rev>` | The resolved revision. `oh add` afterwards pins exactly what you previewed — not "whatever `main` is by then". |
| `targets` | Which harnesses this preview covers, and where that set came from. |
| `scope` | Project or user (`--global`) — the paths differ, and so does what is placeable at all. |
| `signature` | `Trusted` / `Untrusted` / `Revoked` / `Invalid` / `Unsigned`. Unsigned is a fine answer; not being told is not. |
| `asks` | Network reach, executables, filesystem writes — printed **always**, including when all three are `none`, so that a quiet capability is distinguishable from an unreported one. |
| `runtime` | An interpreter it needs and whether you have it, so a missing `node` surfaces now instead of as a denied tool call next week. |
| `writes` | The exact path per harness, with any degradation the kind had to apply to fit. |
| the deferred block | Everything that could **not** be placed — unsupported harnesses, hook registrations, artifacts with no documented user-level home. Listing only the successes would read as "works everywhere". |

## Which harnesses it previews

The profile in the current directory, when there is one — previewing against
eleven harnesses when you use two buries the answer. With `--global` it reads
the user profile instead. Without any profile it falls back to all of them.

`--harness <id>` overrides. In every case the `targets` line says which set was
used and why, because a preview that narrowed itself silently is a preview you
cannot act on.

## What it deliberately does not do

**It does not fetch dependencies.** Resolution stays `flat` whatever the
capability declares, so an unmet `requires` edge is reported as unmet rather
than resolved by cloning a second repository you never named. Acquiring a graph
is precisely the commitment `try` exists to defer; see
[Dependencies](./dependencies.md) for the opt-in that does acquire it.

**It does not run anything.** A capability's `run` entrypoint is never executed
and its `provision` command is never executed — that is `oh install --runtimes`,
which asks first.

**It is not a substitute for reading the code.** A preview tells you what a
capability *declares* and what open-harness *would write*. A `permissions` block
is the author's claim about their own capability, surfaced for consent and
checked against your policy — not a sandbox. See `SECURITY.md` for what is
enforced versus what is surfaced.

## Options

| Flag | Effect |
|---|---|
| `--harness <id>` | Preview one harness (or `all`) instead of the profile's set |
| `--global` | Preview the **user** scope: `~/.claude/skills/…` and friends |
| `--wire` | Include hook registrations as file writes rather than manual steps, matching `oh sync --wire` |
| `--profile <file>` | Take the harness set from a specific profile |
| `--trust <file>` | Verify signatures against this trust store rather than reporting them unsigned |

## Exit codes

| Code | When |
|---|---|
| `0` | The source resolved and the report was produced |
| `1` | The source resolved but provides no capabilities, or resolution failed |
| `2` | No spec was given, or the spec is unusable — an unpinned remote archive, a registry index with no `#name=` |
