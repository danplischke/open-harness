#!/usr/bin/env bash
# Build the napi native addon and place it next to index.cjs with the
# platform-tagged name the loader expects (open-harness.<platform>-<arch>.node).
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release

case "$(uname -s)" in
  Linux)   os=linux;  lib=lib; ext=so ;;
  Darwin) os=darwin; lib=lib; ext=dylib ;;
  MINGW*|MSYS*|CYGWIN*) os=win32; lib=""; ext=dll ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
arch=$(node -e 'process.stdout.write(process.arch)')

src="target/release/${lib}open_harness_node.${ext}"
dst="open-harness.${os}-${arch}.node"
cp "$src" "$dst"
echo "built $dst"

# Smoke-test.
node example.mjs
