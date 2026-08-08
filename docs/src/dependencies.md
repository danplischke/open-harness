# Dependencies

A capability can declare how it relates to other capabilities. This page is the
whole model: what you can write, how it resolves, and — just as important —
what it deliberately does not do.

The design principle throughout: **a capability is code that runs with your
agent's privileges.** That makes a dependency graph a supply-chain surface, not
just a convenience, and it is why the defaults here are stricter than a language
package manager's.

## Names

A bare `id` is unique only inside one tree. The moment your sources are other
people's repositories, two authors both shipping `mem-search` are different
capabilities — so every capability also has a **fully-qualified name**:

```
<namespace>/<id>
```

You rarely write the namespace. It comes from the source:

| source | namespace |
|---|---|
| `local` | none — your own tree, bare ids stay bare |
| `git` | `<owner>/<repo>` from the URL |
| `registry` | the registry entry's name |

An explicit `namespace:` in a manifest outranks whatever the source implies.

The bare `id` stays the local alias, and it is still what the emitted native
filenames use — so two namespaces both providing `mem-search` will *compose*
fine and then both try to write `.claude/skills/mem-search/SKILL.md`. `sync`
resolves that honestly (first producer wins, loudly flagged), and `resolve`
warns about it before anything is written. Fix it with a per-harness
`overrides.path`, or rename one.

## Declaring dependencies

Three spellings, in increasing order of detail:

```yaml
# 1. bare names — any version, relation `requires`
dependencies: [shared-patterns]

# 2. name → version requirement
dependencies:
  acme/shared-patterns: "^1.2"

# 3. the long form
dependencies:
  acme/legacy:
    version: ">=2, <4"
    relation: suggests
  acme/rival:
    relation: conflicts
```

### Version requirements

A **documented subset** of semver: `major[.minor[.patch]][-prerelease]`, with
missing parts treated as zero. Build metadata (`+…`) is accepted and ignored.

| form | meaning |
|---|---|
| `*` or empty | any version |
| `1.2.3` / `=1.2.3` | exactly that version |
| `^1.2.3` | `>=1.2.3, <2.0.0` — leading-zero aware, so `^0.2.3` → `<0.3.0` and `^0.0.3` → `<0.0.4` |
| `~1.2.3` | `>=1.2.3, <1.3.0`; `~1` → `<2.0.0` |
| `>=` `>` `<` `<=` | the obvious bound |

Comparators combine with `,` and **all** must hold: `>=1.2, <2.0`.

This is a subset rather than a near-miss of the full grammar on purpose. A
subset you can state in a table is honest; an almost-complete implementation
whose gaps surface as a mystery mismatch two repos deep is not. An unparseable
requirement is an error, never a silent "any" — a requirement that quietly means
"anything" is how you ship the wrong version.

Prereleases order the semver way (`1.0.0-rc.1` < `1.0.0`), and there is one rule
people expect and rarely get: **a prerelease only satisfies a requirement in
which some comparator names a prerelease at the same `major.minor.patch`.**
Without it, `^1.0.0` would accept `2.0.0-beta` — a prerelease of a version the
range explicitly excludes. The opt-in is judged per requirement, not per
comparator, so `>=1.5.0-rc.1, <2.0.0` accepts `1.5.0-rc.2`. `*` is exempt: it
states no constraint at all.

### Relations

| relation | meaning |
|---|---|
| `requires` (default) | must be present and satisfy the requirement |
| `suggests` | better together; absence is a note, not a problem |
| `conflicts` | must **not** be composed alongside |
| `replaces` | supersedes the named capability — a fork or vendored copy stands in for it |

`conflicts` earns its place here more than in a normal package manager. `sync`
merges same-path config **deny-wins**, so two contradictory permission policies
do not clash loudly — they silently produce the strictest union, which may be
neither author's intent. Declaring the conflict is how that becomes visible.

Only `requires` creates ordering edges. A suggestion that did could manufacture
a dependency cycle out of optional advice.

## How a name resolves

A dependency's written name is matched, in decreasing order of specificity:

1. an exact qualified-name match;
2. a **sibling** — the bare name inside the requirer's own namespace, so
   capabilities in one repo keep referring to each other by bare id;
3. a `replaces` alias;
4. a bare id that is unique across everything composed.

An **ambiguous** bare id (step 4, several matches) resolves to nothing and is
reported unmet. Guessing between two strangers' capabilities is precisely the
failure qualified names exist to prevent.

## How a version is selected

**Sources are a preference order.** The earliest source providing a name wins,
exactly as it did before requirements existed. A requirement can *veto* that
choice — if the preferred candidate does not satisfy what its dependents ask
for, the next satisfying candidate is taken — but it never reorders sources for
its own sake. Constraints constrain; they do not become a competing ranking.

When nothing on offer satisfies every dependent, you get the part of a solver
people actually need — who asked for what:

```
version conflict on 'acme/secret-guard': no available version satisfies every dependent
    my-setup requires ^1.2
    acme/pr-review requires >=2
    capabilities/ offers 1.4.2
    keeping 1.4.2 (from capabilities/) — the dependents' requirements are NOT met
```

### What this deliberately does not do

**There is no backtracking.** Requirements are collected from the preferred
candidate of each name, and selection runs once. A requirement that only appears
in a version another requirement rules out will not be discovered. At this scale
— tens of capabilities, not thousands of crates — that trade is worth it, and
the conflict report says what was asked and by whom rather than implying a
search that never happened. If it ever stops being worth it, a real solver drops
in behind the same interface.

## Selecting part of a source

A source is usually somebody else's repository, and you rarely want all of it.
Without a filter the only lever is `subdir`, which is an accident of how they
laid the repo out rather than a choice about what you need — a plugin shipping
nineteen skills is all nineteen or none.

Every source kind takes a `select` block:

```yaml
sources:
  - git:
      url: https://github.com/thedotmack/claude-mem
      subdir: plugin/skills
      select:
        include: [mem-search, timeline-report]
```

```
warning: select kept 2 of 19 from https://github.com/thedotmack/claude-mem
  (dropped babysit, cloud-sync, design-is, do, how-it-works, knowledge-agent,
   … and 11 more)
profile 'just-two-skills' → 2 capabilities across 2 harness(es)
```

| field | meaning |
|---|---|
| `include` | patterns to keep; **empty means everything**, so an existing profile is unaffected |
| `exclude` | patterns to drop, applied after `include` — exclude always wins |
| `kinds` | capability kinds to keep (`skill`, `hook`, …); empty means all |
| `with_dependencies` | re-admit capabilities a selected one `requires` (default `false`) |

Patterns are globs (via the `glob` crate, used on strings — nothing here touches
the filesystem) matched against the **qualified name** *and* the **bare id**:
`*`, `?`, and character classes like `[a-z]` or `[!abc]`. `*` spans `/`, so
`acme/*` selects a whole namespace. A malformed pattern — an unclosed `[`, say —
is an error naming the pattern, not a literal bracket that quietly matches
nothing.

`kinds` is the one worth knowing about: a plugin's skills are portable while its
hooks are usually harness-specific shell, so `kinds: [skill]` takes the useful
half without enumerating every skill by name.

### A pattern that matches nothing is an error

```
select.include pattern 'mem-serach' matches nothing in …/claude-mem.
Available: …/babysit, …/cloud-sync, …/design-is, …
```

A typo silently yielding zero capabilities is exactly the failure this feature
would otherwise introduce, so it fails the resolve and lists what is actually
there. A pattern that matches something `kinds` or `exclude` then removes is
*not* an error — that combination is deliberate.

### Selection and dependencies

If you select a capability and exclude something it `requires`, the default is
to **report it, not repair it**:

```
capability 'app' requires 'shared', which no source provides
```

Narrowing a source is your decision, and silently widening it back would undo
the choice you just made. When you do want the closure, ask for it:

```yaml
select:
  include: [app]
  with_dependencies: true
```

which pulls required siblings back in — transitively, to a fixpoint — and says
so for each one.

### Selection and the lockfile

Filtering happens **before** the capabilities are locked, so the lock records
only what you selected. Widening the selection later is therefore a visible lock
change that `--locked` catches, rather than a silent one.

## Transitive acquisition

By default (`resolution: flat`) only the sources your profile names are used. A
dependency nothing provides is reported unmet, and **nothing is fetched**.

You can opt into following the sources capabilities declare for their own
dependencies:

```yaml
name: my-setup
harnesses: [claude-code]
resolution: transitive
transitive_trust: trusted   # trusted (default) | tofu | any
trust: trust.yaml
sources:
  - local:
      path: capabilities
```

with the dependency naming where to get it:

```yaml
dependencies:
  acme/shared-patterns:
    version: "^1.2"
    source:
      git:
        url: https://github.com/acme/capabilities
        rev: v1.4.0
```

This is off by default because turning it on means *installing one capability
can clone repositories you never named, whose code then runs with your agent's
privileges*. Everything acquired this way must pass a gate:

| `transitive_trust` | admits |
|---|---|
| `trusted` (default) | a valid signature by a key already in your trust store |
| `tofu` | also a valid signature by an *unknown* key — content still verified, only the signer is new |
| `any` | anything, including unsigned code |

Every refusal is reported with the fix, and every acquisition is reported too —
nothing enters the composed set silently in either direction. Tampered content
is refused even when the signer is trusted, because the digest is recomputed
from disk before the signature is checked.

Enabling `transitive` without a `trust:` store tells you up front that an empty
store trusts no key and nothing can be acquired. That is the correct default,
not a bug.

## Per-harness satisfaction

"Present" is not the same as "usable here". A rule that depends on a permission
policy resolves fine, and then meets Cline — which has no committed allowlist
for the permission kind to write.

So the resolver asks the same `plan()` question `sync` does, once per target
harness, and reports each dependency that is Unsupported where its dependent is
not:

```
capability 'app' depends on 'policy', which is Unsupported on: cline
    — 'app' installs there without it
```

The dependent still installs. The honest-degradation rule applies to the
dependency graph as much as anywhere else: you find out.

## The lockfile

`open-harness.lock` pins the resolved graph, not the names anyone happened to
write:

```yaml
version: 1
profile: my-setup
sources:
  - kind: git
    origin: https://github.com/acme/capabilities
    rev: 9f2c1e4…
    capabilities:
      - name: acme/secret-guard
        id: secret-guard
        version: 1.4.2
        kind: hook
        digest: "sha256:…"
        dependencies:
          acme/shared-patterns: 0.9.1
```

`digest` is a sha256 over the capability's whole file tree — the same digest
signatures are made over, so the lock and the trust layer agree on what "this
capability" means, and a changed skill body moves the lock.

### `--locked`

```sh
oh resolve --profile open-harness.yaml --locked
oh sync    --profile open-harness.yaml --into . --locked
```

The lockfile becomes a contract rather than an output: it must exist, the
resolution must match it, and **nothing is written**. A mismatch prints what
drifted and exits non-zero.

```
lockfile is stale — open-harness.lock does not match the profile as resolved:
  ~ acme/secret-guard 1.4.2 — content changed
  + acme/new-thing 0.1.0 (not in the lock)
  - acme/retired (locked, but no source provides it)
```

This is the flag that makes a lock worth having in CI. A `--locked` run that
quietly rewrote the lock it was meant to verify would verify nothing.

Note what `--locked` does *not* fail on: a git source whose branch has moved
upstream. That is the whole point of a lock — `rev: main` stays at the recorded
commit until you re-resolve, so somebody else's push cannot change your build.
Editing `rev:` in the profile is a different thing entirely, and it *does* win
over the pin; the lock records the revision that was requested alongside the
commit it resolved to, precisely so the two cases can be told apart.

## Cycles

A `requires` cycle is reported and the capabilities are kept in source order,
rather than dropped:

```
dependency cycle among [acme/a, acme/b] — kept in source order
```
