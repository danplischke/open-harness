#!/usr/bin/env bash
# End-to-end walkthrough of the whole open-harness lifecycle, with real commands
# and real output, exercising every major subsystem in order:
#
#   1. author    — capabilities in any language, one stdio contract
#   2. sign+trust— integrity & authorship for a distributable capability
#   3. compose   — a profile of sources → a pinned lockfile
#   4. sync      — install the composed set into a project (native fan-out)
#   5. dispatch  — one decision → each harness's native deny convention
#   6. import    — the project's native config read back into capabilities
#   7. bridge+report — MCP→CLI bridge + the generated support matrix
#
# Everything is written under examples/.walkthrough (a throwaway workspace, git
# ignored); nothing here mutates the repo's own capabilities or keys.
#
#   bash examples/walkthrough.sh
#
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=./target/debug/oh
[ -x "$BIN" ] || cargo build --bin oh

WORK=examples/.walkthrough
rm -rf "$WORK"; mkdir -p "$WORK"
cp -R capabilities "$WORK/capabilities"   # work on a copy — signing writes .sig files
CAPS="$WORK/capabilities"

# The same one decision, expressed in each harness's native input shape.
SAFE='{"tool_name":"Bash","tool_input":{"command":"git status"}}'
SECRET='{"tool_name":"Bash","tool_input":{"command":"echo AKIAIOSFODNN7EXAMPLE"}}'
CURSOR_SECRET='{"command":"echo AKIAIOSFODNN7EXAMPLE","cwd":"/repo"}'   # Cursor's beforeShellExecution shape

# Echo each command as `$ oh …`, then run it against the freshly built binary.
oh()   { printf '\n$ oh %s\n' "$*"; "$BIN" "$@"; }
act()  { printf '\n\n══════════ %s ══════════\n' "$1"; }
note() { printf '  » %s\n' "$1"; }

act "1 · AUTHOR — capabilities in any language, one contract"
note "A hook is any executable that reads a CanonicalPayload and writes a Decision."
note "The repo ships a Python guard (secret-guard) + a Node audit note (audit-note)."
note "Scaffold a typed TypeScript peer and run it directly — Node >= 22.18 strips the"
note "types, so there is no build step and no dependency:"
oh scaffold --kind hook --lang typescript --id latency-note --into "$WORK/authored"
printf '%s' "$SAFE" | oh run --harness claude-code --event pre.tool.any --capabilities "$WORK/authored" --explain || true
note "Health-check the capabilities we'll compose (does each install on >=1 harness?):"
oh doctor --capabilities "$CAPS" || true

act "2 · SIGN & TRUST — integrity + authorship for a distributable capability"
oh keygen --out "$WORK/author.key" --label demo-author
oh sign --capability "$CAPS/secret-guard" --key "$WORK/author.key"
note "Verify against an empty trust store → valid signature, but UNKNOWN signer:"
oh verify --capability "$CAPS/secret-guard" --trust "$WORK/trust.yaml" || true
note "Trust the signer (trust-on-first-use), then verify again → trusted:"
oh trust --capability "$CAPS/secret-guard" --trust "$WORK/trust.yaml" --label demo-author
oh verify --capability "$CAPS/secret-guard" --trust "$WORK/trust.yaml"

act "3 · COMPOSE — a profile of sources → a pinned lockfile"
cat > "$WORK/open-harness.yaml" <<'YAML'
name: walkthrough
harnesses: [claude-code, cursor, opencode]
sources:
  - local:
      path: capabilities
YAML
note "profile: 3 harnesses, 1 local source ($WORK/open-harness.yaml)"
oh resolve --profile "$WORK/open-harness.yaml"

act "4 · SYNC — install the composed set into a project (native fan-out)"
oh sync --profile "$WORK/open-harness.yaml" --into "$WORK/project"
note "the native files open-harness wrote — one composed set → three harnesses:"
( cd "$WORK/project" && find . -type f | sort | sed 's/^/      /' )
note "re-check drift against the SAME profile → in sync (sync is idempotent):"
oh check --profile "$WORK/open-harness.yaml" --into "$WORK/project" --ci

act "5 · DISPATCH — one decision, each harness's native deny convention"
note "Claude — SAFE input → exit 0 (allowed), audit note added to model context:"
printf '%s' "$SAFE"   | oh run --harness claude-code --event pre.tool.any --capabilities "$CAPS" --explain || true
note "Claude — SECRET input → exit 2 (a deny wins over the allow):"
printf '%s' "$SECRET" | oh run --harness claude-code --event pre.tool.any --capabilities "$CAPS" --explain || true
note "Cline — the same deny, delivered as a stdout {cancel} document:"
printf '%s' "$SECRET" | oh run --harness cline --event pre.tool.any --capabilities "$CAPS" || true
note "Cursor — the same deny, as a stdout {permission} document (per-class pre.tool.shell):"
printf '%s' "$CURSOR_SECRET" | oh run --harness cursor --event pre.tool.shell --capabilities "$CAPS" || true
note "OpenCode — the same deny, as the canonical decision its in-process shim reads:"
printf '%s' "$SECRET" | oh run --harness opencode --event pre.tool.any --capabilities "$CAPS" || true

act "6 · IMPORT — read the project's native config back into capabilities"
note "the inverse of step 4: what sync just wrote, read back as portable capabilities."
note "this is the adoption path for a project that already has harness config."
oh import --into "$WORK/project" --out "$WORK/reimported" --dry-run || true
note "written for real, then loaded back:"
oh import --into "$WORK/project" --out "$WORK/reimported" >/dev/null 2>&1 || true
( cd "$WORK/reimported" && find . -type f | sort | sed 's/^/      /' )
# `head` closes the pipe; with `pipefail` that surfaces as SIGPIPE (141).
oh check --capabilities "$WORK/reimported" 2>/dev/null | head -n 14 || true

act "7 · BRIDGE & REPORT — MCP→CLI bridge + the generated support matrix"
note "Call an MCP server's tool through the shell (for MCP-disabled environments):"
oh mcp call --id echo-bridge --capabilities "$CAPS" --tool echo --json '{"text":"hello from the bridge"}' || true
note "The (event × harness) support grid + how each adapter was established:"
oh matrix

printf '\n\ndone — explore the generated project, lockfile, and signatures under %s/\n' "$WORK"
