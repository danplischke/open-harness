# Distributing capabilities

This is a **design note** for distributing *capabilities* — the skills, hooks,
rules, commands, tools, and agents an author writes — through a central,
CDN-hosted registry (for concreteness: a JSON index and content-addressed blobs
on S3 behind a CloudFront distribution).

It is deliberately framed as an *extension of an existing seam*, not a new
subsystem: open-harness already models "a central index over many artifacts,
integrity-checked, and pinned in a lock" as the [`Registry`
source](#what-exists-today). A CDN registry is that same concept with the index
(and, optionally, the bytes) served over HTTPS.

> **Status.** To stay honest about what ships versus what is proposed, every
> concrete change is tagged **Implemented** or **Proposed** in
> [Implementation](#implementation-implemented-vs-proposed). In prose: the
> `Registry` source, the lockfile, and the whole trust module exist today; HTTPS
> fetch, an HTTP/tarball leaf source, and a publish command do not.

## Two axes of distribution

These are orthogonal; this note is about the second.

1. **The `oh` tool.** The binary a developer runs. Distributed by
   `release.yml`, which builds cross-platform `oh` binaries on a tag.
2. **The capabilities `oh` installs.** The catalog an author publishes and a
   developer composes. This is the registry axis, and what follows.

## Where a registry fits

A profile (`open-harness.json`) is an ordered list of **sources**; resolving it
composes them, then writes `open-harness.lock` pinning every resolved capability
and every source's exact revision, so a re-resolve is reproducible.

```jsonc
{
  "name": "my-team",
  "harnesses": ["claude", "codex", "cursor"],
  "sources": [
    { "local":    { "path": "capabilities" } },       // in-repo overrides
    { "git":      { "url": "https://github.com/a/b", "rev": "main" } },
    { "registry": { "name": "commit-style", "index": "…" } }  // the catalog
  ]
}
```

Sources are **ordered and earlier-wins**: a later duplicate id is a loud
shadow warning, never a silent override. A CDN registry is simply *one more
source* in this list — the resolver already handles precedence, dedup, the
dependency graph, and locking uniformly across all source kinds. That
uniformity is the whole payoff: nothing about a CDN registry is special to the
resolver.

## What exists today

| Piece | Where | What it gives you |
|---|---|---|
| `Source::Registry { name, version, index }` | `profile.rs` | A **central JSON index** mapping capability names → their real source. |
| `RegistryIndex` / `RegistryEntry` | `profile.rs` | The index schema (below). |
| Lockfile (`Lock`, `LockedSource`, `LockedCap`) | `profile.rs` | Pins id + version + fingerprint per capability, and each git source's commit SHA. |
| Trust module | `trust.rs` | sha256 content digests, ed25519 signatures, a trust store, a root-of-trust keyring, and revocation. |
| HTTP/1.1 + rustls client | `mcp.rs` (`mcp-http-tls` feature) | The TLS building blocks, already in-tree (used today by the MCP bridge). |

The conceptual model is done. What is missing is hosting the index on a URL
instead of a local path, and (optionally) serving the capability bytes from the
CDN instead of a git clone.

## The registry index

The index is the catalog: names → where each capability actually lives. Today
each entry points at a `local` or `git` leaf source:

```json
{
  "capabilities": [
    {
      "name": "commit-style",
      "version": "1.2.0",
      "source": { "git": { "url": "https://github.com/a/b", "subdir": "capabilities/commit-style" } }
    }
  ]
}
```

For a CDN-native registry, an entry instead points at a content-addressed
artifact (**Proposed** — see the `http` leaf below):

```json
{
  "capabilities": [
    {
      "name": "commit-style",
      "version": "1.2.0",
      "source": {
        "http": {
          "url": "https://cdn.example.com/blobs/sha256/9f86d081…a08.tar.zst",
          "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        }
      },
      "signer": "ed25519:3b6a27bc…"   // optional: author key fingerprint
    }
  ]
}
```

Two rules keep the format durable:

- **Keep it vendor-neutral.** The schema knows "an entry points at a
  fetchable, digest-pinned artifact" — not "a CloudFront URL." Then S3 +
  CloudFront, GitHub Pages, an internal Artifactory, or a bare `file://` are all
  the same to the resolver. Model *a verifiable fetchable index*, not *a
  CloudFront registry*.
- **Keep the resolver's contract `name → metadata`,** never "download the whole
  world to resolve one name." One JSON file listing every capability@version is
  genuinely fine at hundreds of entries; that contract is what lets it later
  shard by name-prefix (or move to per-name `/<name>/index.json`) without a
  format break.

## CDN layout

The single operational trap is a **mutable `registry.json` at a fixed URL**: it
fights the CDN. Short TTLs defeat caching; long TTLs plus an invalidation on
every publish cost money, propagate slowly, and still let a consumer read a
stale index.

The standard fix is **immutable content + a tiny mutable pointer**:

```text
s3://oh-registry/                       (origin behind CloudFront)
  blobs/sha256/<digest>.tar.zst         # immutable, content-addressed
                                        #   Cache-Control: public, max-age=31536000, immutable
  index/1.json  index/2.json  …         # immutable index snapshots
  registry.json                         # { "latest": "index/2.json" } — tiny, short TTL
```

- **Blobs are content-addressed and cached forever.** The URL *is* the sha256,
  so a byte can never change under a URL; the cache never has to be invalidated.
- **Index snapshots are immutable too** — `index/2.json` is a frozen catalog;
  publishing writes `index/3.json`, never edits an existing one.
- **Only `registry.json` is mutable,** and it is a few bytes with a short TTL.

**Publish ordering makes it atomic:** upload the blobs, then the new immutable
index snapshot, then flip the pointer *last*. A consumer reading the pointer
only ever sees an index that references artifacts already present — there is no
window where the catalog names a blob that hasn't landed.

## The trust flow

A registry installs runnable hooks into people's agents, so it is a
supply-chain surface. The posture, already baked into `trust.rs`, is: **the CDN
is a dumb, cacheable byte pipe — not a root of trust.** TLS authenticates the
*hop*, not the *artifact*. Authenticity comes from signatures over content
digests, pinned in the lock.

End to end:

1. **Author / publish.** `oh sign` writes `capability.sig` — an ed25519
   signature over the capability's sha256 content digest (`capability_digest`,
   a hash of the whole file tree). Packaging carries the signature inside the
   tarball. The index snapshot itself can be signed too, so a consumer who has
   pinned a **root** key verifies the whole catalog's provenance in one step
   (root-of-trust `Keyring`).
2. **Resolve.** `oh resolve --trust trust.json [--require-signed]` fetches the
   pointer → index → blobs, and for each blob verifies **both** that its sha256
   equals the content-address URL **and** that its signature checks against the
   `TrustStore`. Verification is five-valued and non-negotiable on the reject
   states: `Trusted`, `Untrusted` (valid signature, unknown key — trust-on-
   first-use territory), `Revoked` (**always** rejected), `Invalid` (tampered
   or bad signature — **always** rejected), `Unsigned`. The resolved digest is
   pinned in `open-harness.lock`.
3. **Re-resolve.** With a lock present, artifacts are re-fetched by content
   address and re-verified — byte-reproducible, tamper-evident, and fully
   offline once the blobs are cached.
4. **Revoke.** A withdrawn key never verifies again — even for already-published
   artifacts, even in advisory mode — and revoking a *root* withdraws every
   author it vouched for.

The practical rule this yields: **"it came from our HTTPS CloudFront" must never
substitute for a signature.** A swapped or tampered blob fails on resolve
regardless of how trusted the transport looked.

## Resolution & composition

Because a registry is just a `Source`, everything the resolver already does
applies unchanged:

- **Ordering / precedence.** A local in-repo override shadows a registry entry
  with a loud warning; the registry never silently wins or loses.
- **Dependencies.** A capability's declared `dependencies` are resolved across
  *all* sources together — unmet deps and cycles are reported loudly and
  non-fatally, and the set is topologically ordered.
- **Reproducibility.** The lock pins the resolved digest of every registry
  artifact, so `resolve` in CI reproduces exactly what a teammate resolved.
- **Offline-testable.** A `file://` index over a fixture directory exercises the
  entire path with no network, mirroring how git sourcing is already tested
  against a throwaway local repo.

## Implementation (Implemented vs Proposed)

**Implemented (today):**

- `Source::Registry` with a **local** `index` path; `Source::Git`
  (clone/fetch/pin via system `git`); `Source::Local`.
- The lockfile and the full dependency resolver.
- `trust.rs`: `capability_digest`, `sign`/`verify`, `TrustStore`, `Keyring`
  (root of trust), and revocation — surfaced through `oh sign|verify|trust|
  revoke|keyring` and `oh resolve --require-signed`.
- A hand-rolled HTTP/1.1 + rustls client in `mcp.rs` behind the `mcp-http-tls`
  feature (the TLS stack is already a dependency, just gated).

**Proposed (the delta to CDN-native):**

1. **Fetch the index over HTTPS.** Today `RegistryIndex::load` only does
   `std::fs::read_to_string`. Teach it scheme detection and an HTTPS `GET`.
   In-grain options, best fit first: extract a small `http_get(url) -> bytes`
   from the existing rustls path in `mcp.rs`; **or** shell out to `curl`
   (matches the "shell out to system `git`" precedent); **or** add a tiny client
   crate (against the dependency-light grain). A `file://` scheme keeps offline
   tests trivial.
2. **An `http` / tarball leaf source.** `Source::Http { url, sha256 }`:
   download → verify the digest → unpack into the cache → scan. Lock kind
   `"http"`, pinning the sha256. This is the fourth `Source` variant beside
   `Local`/`Git`/`Registry`, and the reason a registry entry can resolve to CDN
   bytes rather than a git clone.
3. **A publish command.** `oh pack` / `oh registry publish`: package each
   capability into a content-addressed tarball, compute its sha256 (reuse
   `capability_digest`), sign it, and emit/update the index snapshot + pointer.
   This is the capability-content analog of what `release.yml` does for the
   binary.
4. **Pin the cryptographic digest in the lock.** `LockedCap.fingerprint` today
   is a non-cryptographic FNV-1a hash used for *drift* detection — distinct from
   the sha256 `capability_digest` used for *integrity*. A remote artifact's pin
   should record the sha256 (the content address), so the lock is the tamper
   boundary, not just a drift check.

## Non-goals & open decisions

**Non-goals / deferred (unchanged by this design):**

- **Runtime sandbox enforcement** of the permission manifest remains deferred
  (see `SECURITY.md`); the registry surfaces permissions for consent and policy,
  it does not sandbox execution.
- **Unbounded single-file scale.** One index file is fine now. Sharding is a
  future step the `name → metadata` contract already permits; this design does
  not build it.

**Decisions to make deliberately:**

- **HTTP client.** Reuse the in-tree rustls path (feature-gated, most in-grain)
  vs. shell out to `curl` (matches git) vs. a client crate (simplest, heaviest).
- **Mutable-pointer scheme and TTL** — e.g. `registry.json → index/<n>.json`
  with a short TTL, and whether the pointer is signed.
- **Default posture for remote sources.** Recommendation: treat an unsigned or
  untrusted **remote** artifact more strictly than a local one — either imply
  `--require-signed` for `http`/remote-registry sources or emit a loud warning —
  since a CDN artifact has no local provenance to fall back on.
