#!/usr/bin/env sh
# Regenerate docs/src/harness-matrix.md from the adapters. Run from anywhere;
# CI runs this and fails if the committed file drifts (proving it's generated).
set -eu
cd "$(dirname "$0")/.."
{
  echo "# Harness support matrix"
  echo
  echo "> Generated from the adapters by \`oh matrix --markdown\` (\`docs/gen-matrix.sh\`) — do not edit by hand."
  echo
  cargo run --quiet --bin oh -- matrix --markdown
} > docs/src/harness-matrix.md
echo "wrote docs/src/harness-matrix.md" >&2
