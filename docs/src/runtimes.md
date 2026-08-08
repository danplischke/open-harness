# Runtimes

A capability is *any executable in any language*. That's the whole contract —
and it means a capability can need things the host doesn't have: an interpreter,
a helper CLI, third-party packages.

This page is about what open-harness does with that, and, just as importantly,
what it refuses to do.

## The failure this exists to prevent

A hook whose script imports a package you don't have doesn't degrade. It
**denies**:

```
errored=["needs-deps: [non-zero-exit] exit 1: ModuleNotFoundError:
  No module named 'yaml' -> deny (auto)"] -> exit=2
```

Exit 2 is the block signal. On a blocking event the default failure policy is
fail-closed — correctly, because a guard that cannot run leaves you unguarded —
so a missing `pip install` means every `Bash` call the agent makes is refused,
and the reason the user is shown is a stack trace.

That verdict is right. The *diagnosis* was not, and nothing warned first.

## `runtime.requires`

Declare what the entrypoint needs on `PATH`:

```yaml
id: secret-guard
kind: hook
runtime:
  requires: [uv]
run:
  command: uv
  args: [run, --script, guard.py]
```

Now the gap is caught in three places instead of one:

```
$ oh check --capabilities caps
capability: secret-guard  [kind: hook]
  runtime      MISSING — uv (install: brew install uv)
  claude-code  installable — clean

$ oh doctor
  ✗ secret-guard   [hook]
      runtime missing: uv (install: brew install uv)
```

and if it still reaches dispatch, the failure has its own class:

```
[runtime-missing] runtime missing: uv (install: brew install uv) -> deny (auto)
```

The verdict is unchanged — still a deny on a blocking event. What changed is
that it names the tool, offers the fix, and `check` told you before the session
started.

`requires` is a list of **executable names**, resolved on `PATH` exactly the way
the dispatcher resolves them, so `check` and the runtime can never disagree.
Unknown tools simply get no install hint, which is more useful than a wrong one.

## What open-harness will not do

**It does not install anything.** `sync` writes config; it never runs a package
manager. There is no `oh` command that shells out to `pip`.

That's a deliberate line, and the reasoning is the same one this project applies
everywhere else:

- `pip install` and `npm install` **execute arbitrary code** at install time —
  build backends, `setup.py`, `postinstall`. That is strictly more dangerous than
  the capability itself, which at least only runs when an event fires. Gating
  transitive capability acquisition on signatures while opening an ungated
  code-execution path underneath it would be theatre.
- Owning resolution, lockfiles, caches and wheel matrices for three-plus
  ecosystems, cross-platform, is a bigger project than open-harness.
- "`run` is just a command line" stops being true the moment the manifest knows
  what a Python package is.

**And it does not bundle anyone else's toolchain.** `oh` is a 2.8 MB static
binary; `uv` is 50 MB and `node` is 119 MB. Shipping either would be the smaller
objection, though — the real one is that vendoring a third-party binary into our
release means it arrives with *none* of the verification we require of
capabilities, unpinnable and uninspectable, signed by us as though we'd vetted
it. Your package manager does this better, with provenance and updates you
control. So open-harness checks for the tool, tells you how to get it, and lets
you decide.

## The ladder

Best first. Every rung below the top trades away something real.

### 1. No runtime — a compiled binary, or stdlib only

```yaml
run:
  command: ./guard          # or: python3 guard.py, using only the stdlib
```

Nothing to install, nothing to provision, nothing to go stale, no package
manager executing code on your machine. If a capability can be written this way,
it should be. Worth insisting on for anything security-sensitive.

### 2. Vendored dependencies

Ship the dependencies inside the capability directory rather than a recipe for
fetching them.

This needs no machinery at all — they're just files — and it buys something
nothing else here can: **vendored dependencies fall inside the capability's
content digest**, so the author's existing signature covers them. Everywhere
else, a signature covers the capability and says nothing about what it will pull
down later. This is the only rung that gives you a signed dependency closure.

It's also the answer for locked-down environments — the same constituency the
MCP→CLI bridge exists for. A vendored capability declares `runtime: {requires:
[]}` and works with no network at all.

The costs are narrow but real: native extensions are platform-specific (a
vendored `numpy` is a lie on three of your five targets), and the repository
grows. Right for pure-Python and pure-JS; wrong for anything compiled.

### 3. Provisioned by the ecosystem's own tool

Declare dependencies the language-native way and let that language's tool handle
them. For Python that means PEP 723 inline metadata, which puts the declaration
in the capability's own script rather than in our manifest:

```python
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyyaml>=6"]
# ///
import yaml
```

```yaml
runtime:
  requires: [uv]
run:
  command: uv
  args: [run, --script, guard.py]
```

`uv` resolves, caches and isolates; ten Python capabilities share one wheel
cache instead of ten virtualenvs, and `uv lock --script` gives reproducibility.
For Node, the capability directory is already an npm package (`oh scaffold
--project` generates one) and `npm ci` against a committed `package-lock.json`
does the same job — failing loudly if the lockfile is absent rather than quietly
resolving something new.

The trade: needs the tool, and needs network at least once.

### 4. System packages

The host has to already be right. `requires` can tell you it isn't; it cannot
fix it. Fine for `git` or a company-standard CLI, poor for anything a capability
should be able to guarantee about itself.

## Which runtimes are supported

Two tiers, and the difference is worth being explicit about.

**Paved** — scaffolded, `doctor`-checked, covered in CI on Linux, macOS and
Windows, with a working example capability in the repository:

| | interpreter | scaffold | provisioning |
|---|---|---|---|
| Python | `python3` | `oh scaffold --lang python` | `uv` (rung 3) |
| Node / TypeScript | `node` | `--lang node` / `--lang typescript` | `npm` (rung 3) |
| shell | `sh` / `bash` | `--lang bash` | none — its dependencies *are* `requires` |

**Resolvable but not paved** — `deno`, `bun` and `ruby`. The interpreter
resolver knows how to find them and run a `.rb` or a Deno script, which is
genuinely useful, but there is no scaffold, no CI coverage and no example. They
work; they are not a promise. Anything else — Go, Rust, an in-house CLI — is in
the same position via `requires` plus a `run` command, and needs no support from
us at all.

Adding a language to the paved tier means a scaffold, a `doctor` line, a CI
matrix entry and an example capability. Not a match arm.

## Cross-platform notes

- `requires` resolution honors `PATHEXT` on Windows and never relies on a `#!`
  shebang, matching how the dispatcher resolves interpreters.
- Install hints are per-OS: `brew` on macOS, the distribution's package manager
  on Linux, and the vendor's own documentation where a package identifier would
  be a guess.
- **shell is the weak rung.** `sh` doesn't exist on Windows, and unlike Python
  and Node it has no CI coverage. Prefer Python or a compiled binary for
  anything that must run everywhere.
