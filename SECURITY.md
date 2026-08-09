# Security & threat model

open-harness distributes **runnable capabilities** — hooks that execute as
subprocesses inside a developer's coding agent, and generated config that alters
how that agent behaves. That is a supply-chain surface: installing a capability
is running someone else's code with your agent's privileges. Trust is therefore
a first-class part of the design (#17), not an afterthought.

This document states what is protected, how, and — honestly — what is **not yet**
enforced.

## Assets & trust boundaries

- **The developer's machine / agent privileges.** A hook capability runs with
  whatever the agent can do (read the repo, run shell commands, reach the
  network). This is the asset to protect.
- **The capability author.** Identified by an ed25519 public key. Authenticity
  means "this capability is the bytes that key signed."
- **The integration surface.** `oh sync` writes files into a project and wires
  registrations; it must never write outside the target tree or clobber
  unmanaged files.

Trust boundary: everything crossing from an **external source** (a git repo, a
registry, a downloaded capability) into the **local install** is untrusted until
verified.

## The trust model

### Content integrity — `sha256` digest
`capability_digest` hashes every file in a capability directory individually,
sorts the `(path, hash)` pairs, and hashes the list. Any changed byte, added, or
renamed file changes the digest. It is deterministic and platform-independent
(forward-slash paths, sorted), so it is stable across machines.

A **symlink is hashed as a link** — its target path, as git records one — and is
never followed. Dereferencing would break the "stable across machines" property
it claims: a link out of the tree folds a file the capability does not ship into
its identity (editing that unrelated file then reports the capability as
*tampered*), an absolute link digests differently on every machine so its
signature could never verify downstream, and a link cycle is walked until the OS
refuses. Recording the link still covers it, so adding or repointing one is
detected. A capability with no symlinks digests exactly as it always has, so
existing signatures are unaffected.

Because the *filename* is part of the digest, migrating a manifest from
`capability.json` to `capability.yaml` (`oh migrate`) changes the capability's
digest and therefore **invalidates any existing `capability.sig`** — verification
returns `Invalid`, exactly as it should for content that no longer matches what
was signed. This is a one-way door: re-sign with `oh sign` after migrating.

### Authenticity — ed25519 signatures
The author signs the digest with an **ed25519** private key. The detached
`capability.sig` carries `{algorithm, public_key, digest, signature}`. On verify
the digest is **recomputed from disk** (so tampering is caught before any crypto
runs) and the signature is checked against the embedded public key with
`ed25519-dalek`. We never roll our own crypto.

### Trust — an explicit key store (TOFU)
A valid signature only proves *who* signed, not that you *trust* them. A
`TrustStore` lists trusted public keys. Verification is five-valued:

| Verdict | Meaning | Passes gate? |
|---|---|---|
| `Trusted` | valid signature by a known (or root-delegated) key | always |
| `Untrusted` | valid signature, key not in the store | only without `--require-signed` |
| `Revoked` | valid signature by a **withdrawn** key | **never** |
| `Invalid` | tampered content, a bad signature, or a signature file that is present but malformed | **never** |
| `Unsigned` | **no** signature file | only without `--require-signed` |

`Unsigned` means the file is absent, and nothing else. A `capability.sig` that is
present but unreadable or malformed is `Invalid`: collapsing the two would make
*corrupting* a signature strictly better for an attacker than tampering with
content, since `Unsigned` passes the gate whenever `--require-signed` is off.

`oh trust --capability DIR` adds a signer's key to the store — an explicit
trust-on-first-use consent step. `oh sync --require-signed` refuses anything not
`Trusted`.

### Least privilege — the permission manifest
A capability declares what it expects to touch:

```jsonc
"permissions": { "read": ["src/**"], "exec": ["git"], "network": ["*"] }
```

`oh verify` / `oh sync` **surface** this for consent, and check it against a host
policy (`--deny-network`, `--deny-exec`). Violations are advisory warnings by
default and **hard failures under `--require-signed`**.

## Threats & mitigations

| Threat | Mitigation | Status |
|---|---|---|
| Tampering with a capability in transit / at rest | recomputed `sha256` digest, verified before crypto | **enforced** |
| Impersonating an author | ed25519 signature over the digest | **enforced** |
| Running unknown / unsigned code silently | five-valued verdict; `--require-signed` gate; TOFU consent | **enforced (opt-in)** |
| Over-broad capability privileges | declared permission manifest, surfaced + policy-checked | **advisory / enforced under `--require-signed`** |
| `sync` writing or deleting outside the project | every filesystem path — planned artifacts **and** lockfile entries alike — is normalized and rejected if it carries `..` / an absolute prefix / `~`; containment is structural, not a lexical prefix test | **enforced** |
| `sync` clobbering a developer's own file | writes are gated on ownership (the lockfile fingerprint): a foreign JSON file is merged into, a foreign non-JSON file is refused and reported, and a file edited since we wrote it is never deleted | **enforced** |
| A capability that forks grandchildren evading the timeout kill | only the direct child is killed today; a grandchild holding an inherited pipe can no longer wedge the dispatcher (`output-stuck`), but it is not killed | **partial** (bounded, not reaped — needs process-group kill) |
| Header/request forgery through the hand-rolled HTTP client | header names, values, and the URL's host and path are refused if they carry control characters, before a socket is opened | **enforced** |
| Untrusted code doing anything at all once installed (sandboxing) | not sandboxed; runs with agent privileges | **deferred** |
| Key compromise / revocation | trust-store revocation list; a revoked key never verifies again, even in advisory mode (#22) | **enforced** |
| Rollback / downgrade to an old vulnerable version | the profile lockfile pins commit + digest; no signed-version-monotonicity yet | **partial** (pinning only) |
| Malicious registry / key distribution | registry index resolution (#21) + a root-of-trust keyring (a trusted root vouches for authors; revoking the root withdraws them, #21); not yet full TUF (no threshold / timestamp / snapshot roles) | **partial** |

## What is enforced today vs deferred

**Enforced:** content-digest integrity, ed25519 signature verification, the
trust store + `--require-signed` gate, **key revocation** (a revoked key never
verifies again, even in advisory mode — #22), permission-manifest surfacing +
policy checks, the `sync` path-containment and content-ownership guarantees
(nothing open-harness did not write is overwritten or deleted, and no path from a
lockfile can leave the project), and lockfile pinning (commit + digest) from #15.

**Deferred (documented, not silently missing):** runtime **sandbox enforcement**
of the permission manifest — real fs/net/exec confinement needs OS mechanisms
(Linux `landlock`/`seccomp`, macOS `sandbox-exec`, Windows job objects) or a
sandboxing dependency, so the manifest is surfaced + policy-checked but not yet
enforced at runtime (#22); process-group kill for forking capabilities; a
**full TUF-style** registry root of trust (the #21 keyring model gives a root
that vouches for authors, but not threshold / timestamp / snapshot roles); and
signed version monotonicity. These are tracked as follow-ups on the roadmap epic.

## Reporting

This is a feasibility spike, not a production release. If you find a security
issue in the model or implementation, open an issue describing the scenario (or,
for anything sensitive, contact the maintainer privately) rather than posting a
working exploit.
