#!/usr/bin/env bash
# End-to-end demo: the SAME two capabilities (Python secret-guard + Node
# audit-note) driving each harness's distinct deny convention through one
# dispatcher. Run from the repo root after `cargo build`.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=./target/debug/oh-dispatch
[ -x "$BIN" ] || cargo build

SAFE='{"tool_name":"Bash","tool_input":{"command":"ls -la"}}'
SECRET='{"tool_name":"Bash","tool_input":{"command":"echo AKIAIOSFODNN7EXAMPLE"}}'
CURSOR_SECRET='{"toolName":"Bash","toolInput":{"command":"echo AKIAIOSFODNN7EXAMPLE"}}'

rule() { printf '\n=== %s ===\n' "$1"; }

rule "Claude (exit-2): SAFE -> exit 0, audit context on stdout"
printf '%s' "$SAFE" | $BIN run --harness claude-code --event pre.tool.any --explain || true

rule "Claude (exit-2): SECRET -> exit 2, reason on stderr (deny-wins)"
printf '%s' "$SECRET" | $BIN run --harness claude-code --event pre.tool.any --explain || true

rule "Cline (stdout cancel): SECRET"
printf '%s' "$SECRET" | $BIN run --harness cline --event pre.tool.any || true

rule "Cursor (stdout permission), per-class pre.tool.shell: SECRET"
printf '%s' "$CURSOR_SECRET" | $BIN run --harness cursor --event pre.tool.shell || true

rule "OpenCode (in-process shim reads this canonical decision): SECRET"
printf '%s' "$SECRET" | $BIN run --harness opencode --event pre.tool.any || true

rule "Matrix"
$BIN matrix

rule "Installability check"
$BIN check
