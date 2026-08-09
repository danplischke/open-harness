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
> `Registry` source, the lockfile, the whole trust module, **fetching the index
> over HTTP(S)**, and **the archive leaf** (so the capability *bytes* come from
> the CDN too) exist today; a publish command and signature verification of a
> fetched archive do not.

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
| Index from a **URL** | `profile.rs` | `index` accepts an `http(s)://` URL, a `file://` URL, or a workdir-relative path. |
| `Source::Http { url, sha256 }` | `profile.rs` | A digest-pinned **archive leaf**: the capability bytes themselves, fetched from the CDN. |
| tar reader + hardened extractor | `archive.rs` | Unpacks an archive without letting it write outside its destination. |
| Lockfile (`Lock`, `LockedSource`, `LockedCap`) | `profile.rs` | Pins id + version + fingerprint per capability, and each git source's commit SHA. |
| Trust module | `trust.rs` | sha256 content digests, ed25519 signatures, a trust store, a root-of-trust keyring, and revocation. |
| HTTP/1.1 + rustls client | `http.rs` (`http-tls` feature) | The shared transport, used by both the MCP bridge and registry fetches. |

The conceptual model is done, and both the index and the capability bytes can
already be CDN-hosted. What is missing is the **publish** side that produces them
and signature verification on the way in.

### Hosting the index

```jsonc
{ "registry": { "index": "https://cdn.example.com/registry.json", "name": "commit-style" } }
{ "registry": { "index": "file:///srv/catalog/registry.json",     "name": "commit-style" } }
{ "registry": { "index": "registry.json",                         "name": "commit-style" } }
```

A fetch failure is a **loud warning**, never a silent skip, and `https://`
without the `http-tls` feature is refused with a pointer to `file://`, a local
path, or a git source — the transport reports that TLS is missing, the caller
supplies the remedy.

One rule falls out of hosting the index remotely: **a remote index may not
select paths on this machine.** An entry pointing at a `local` source would
otherwise resolve against the *consumer's* workdir — a file chosen by whoever
wrote the catalog. Such an entry is refused loudly. Remote indexes point at
fetchable sources; local indexes keep their local reach.

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
artifact:

```json
{
  "capabilities": [
    {
      "name": "commit-style",
      "version": "1.2.0",
      "source": {
        "http": {
          "url": "https://cdn.example.com/blobs/sha256/9f86d081…a08.tar",
          "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        }
      },
      "signer": "ed25519:3b6a27bc…"   // optional: author key fingerprint (not yet verified)
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
  blobs/sha256/<digest>.tar             # immutable, content-addressed
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
- **Offline-testable.** A `file://` index and a `file://` archive exercise the
  whole path with no network, and the HTTP path is covered by a loopback server —
  mirroring how git sourcing is already tested against a throwaway local repo.
- **Cached by content address.** An unpacked archive is keyed by its digest, so
  it is never re-fetched and can never be a stale copy of different bytes.

## Implementation (Implemented vs Proposed)

**Implemented (today):**

- `Source::Registry`, `Source::Git` (clone/fetch/pin via system `git`),
  `Source::Local`.
- **A registry index loaded from an `http(s)://` URL, a `file://` URL, or a
  workdir-relative path** — with a loud warning on fetch failure, an honest
  refusal when `https` meets a build without `http-tls`, and a refusal when a
  *remote* index names a `local` path.
- The lockfile and the full dependency resolver.
- `trust.rs`: `capability_digest`, `sign`/`verify`, `TrustStore`, `Keyring`
  (root of trust), and revocation — surfaced through `oh sign|verify|trust|
  revoke|keyring` and `oh resolve --require-signed`.
- A hand-rolled HTTP/1.1 + rustls client in **`http.rs`**, shared by the MCP
  bridge and registry fetches, with TLS behind the `http-tls` feature
  (`mcp-http-tls` remains as a compatibility alias).

- **`Source::Http { url, sha256, subdir }`** — the archive leaf: fetch → verify
  the digest → unpack → scan, with lock kind `"http"` pinning `sha256:<digest>`
  as the source's revision. `sha256` is **required**, so an unpinned remote
  artifact cannot be expressed. The archive is an uncompressed tar, read by the
  hand-rolled `archive.rs` (see [Archive format](#archive-format)).

**Proposed (the delta to CDN-native):**

1. **A publish command.** `oh pack` / `oh registry publish`: package each
   capability into a content-addressed tarball, compute its sha256 (reuse
   `capability_digest`), sign it, and emit/update the index snapshot + pointer.
   This is the capability-content analog of what `release.yml` does for the
   binary. Until it exists, a publisher uses `tar -cf` + `sha256sum` — which is
   exactly what the archive leaf consumes.
2. **Verify signatures on a fetched archive.** The digest pin makes an archive
   *tamper-evident against the profile*; it does not yet check `capability.sig`
   against the `TrustStore` on the way in, so `--require-signed` does not yet
   gate an `http` source.
3. **Pin the per-capability cryptographic digest in the lock.**
   `LockedCap.fingerprint` is still a non-cryptographic FNV-1a hash used for
   *drift* detection. The archive's sha256 is now pinned at the **source** level
   (`LockedSource.rev`); doing the same per capability would make the lock a
   tamper boundary for git and local sources too.

### Archive format

The archive is an **uncompressed tar**. `archive.rs` hand-rolls the reader,
because tar is one of the *simple* wire formats this crate hand-rolls (fixed
512-byte headers, octal fields) — while compression is not, and would mean a new
dependency. A `.tar.gz` / `.tar.zst` / `.tar.xz` / `.tar.bz2` is recognised by
its magic bytes and **refused by name**, never mis-read.

This costs less than it looks: capability trees are small text files, and a CDN
can still compress them **on the wire** (`Content-Encoding`) without changing the
bytes the digest covers. Adding compressed archives later is a contained change
(a decompressor in front of the same reader) and does not alter the index format.

Extraction is the security-critical half, so the reader refuses anything that
could write outside the destination — absolute paths, drive letters, any `..`
component, symlinks and hardlinks, device nodes — and bounds entry count and
unpacked size against a decompression-style bomb. The digest is verified
**before** unpacking, so tampered bytes never reach the extractor at all.

## Non-goals & open decisions

**Non-goals / deferred (unchanged by this design):**

- **Runtime sandbox enforcement** of the permission manifest remains deferred
  (see `SECURITY.md`); the registry surfaces permissions for consent and policy,
  it does not sandbox execution.
- **Unbounded single-file scale.** One index file is fine now. Sharding is a
  future step the `name → metadata` contract already permits; this design does
  not build it.
- **Compressed archives.** Uncompressed tar only, deliberately — see
  [Archive format](#archive-format). Refused by name, not mis-read.

**Decisions to make deliberately:**

- **Mutable-pointer scheme and TTL** — e.g. `registry.json → index/<n>.json`
  with a short TTL, and whether the pointer is signed.
- **Default posture for remote sources.** Recommendation: treat an unsigned or
  untrusted **remote** artifact more strictly than a local one — either imply
  `--require-signed` for `http`/remote-registry sources or emit a loud warning —
  since a CDN artifact has no local provenance to fall back on.
