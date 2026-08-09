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
the dispatcher resolves them — including the per-OS interpreter remap, so a
`requires: [python3]` is satisfied on Windows where the binary is `python.exe`.
`check`, `doctor`, `run` and `provision` therefore cannot disagree about whether
a capability will start. Unknown tools simply get no install hint, which is more
useful than a wrong one.

## What open-harness will not do

**It never resolves dependencies itself.** There is no open-harness dependency
solver for Python or Node, no lockfile of ours for them, no cache of ours. When
you ask for provisioning, it runs *the ecosystem's own tool* — `uv`, `npm` — and
that tool owns resolution, caching and reproducibility.

**And it never provisions implicitly.** `sync` writes config and nothing else;
running a package manager is a separate verb (`oh install --runtimes`) that
shows the commands and stops until you confirm.

That's a deliberate line, and the reasoning is the same one this project applies
everywhere else:

- `pip install` and `npm install` **execute arbitrary code** at install time —
  build backends, `setup.py`, `postinstall`. That is strictly more dangerous than
  the capability itself, which at least only runs when an event fires. Gating
  transitive capability acquisition on signatures while opening an *ungated*
  code-execution path underneath it would be theatre — which is why the one path
  that does execute is confirmed, refusable, and signature-gateable.
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

#### Provisioning it

Declaring `provision` lets `oh install --runtimes` run the ecosystem's own tool
for you:

```yaml
runtime:
  requires: [uv]
  provision:
    command: uv
    args: [sync, --script, guard.py]
```

```
$ oh install --runtimes
1 capability(ies) declare a provisioning command:

  secret-guard         uv sync --script guard.py
                         in capabilities/secret-guard · missing uv (install: brew install uv)

These commands run with your privileges and may execute code from the package
registries they contact.
Re-run with --yes to proceed.
```

Three gates stand in front of that, all deliberate:

1. **It is a separate verb.** `sync` writes config and never provisions —
   installing config and executing a package manager are different acts.
2. **It shows before it runs.** Without `--yes` it prints the exact commands and
   stops. `pip` and `npm` execute arbitrary code at install time, so the blast
   radius should be visible beforehand, not afterwards.
3. **`--deny-network` refuses outright**, because provisioning is
   network-touching by nature and honouring the flag by running anyway would
   make it a lie.

`--require-signed` additionally restricts it to capabilities whose signature
verifies — the same gate transitive acquisition uses, for the same reason.

The command runs in the capability's own directory, and its requirements are
re-probed afterwards. A provisioner that exits 0 *without* satisfying
`runtime.requires` is reported as a failure, not a success: the command ran, the
dependency is not there, and calling that ✓ would be the same silent lie this
project refuses everywhere else.

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

- `requires`, `run` and `provision` all resolve a command the **same** way:
  `PATHEXT` on Windows, no `#!` reliance, and the per-OS interpreter remap
  (`python3` → `python`). Three code paths spawning or probing the same name
  must not disagree — otherwise a capability that runs fine is reported as
  having a missing runtime, or fails to provision, on Windows only.
- Install hints are per-OS: `brew` on macOS, the distribution's package manager
  on Linux, and the vendor's own documentation where a package identifier would
  be a guess.
- **shell is the weak rung.** `sh` doesn't exist on Windows, and unlike Python
  and Node it has no CI coverage. Prefer Python or a compiled binary for
  anything that must run everywhere.
