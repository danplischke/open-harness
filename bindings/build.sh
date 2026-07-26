#!/usr/bin/env bash
# Generate the Python bindings from the Rust core and run the example.
# Run from anywhere; operates on the repo root.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> building the FFI cdylib (uniffi)"
cargo build --features ffi

echo "==> generating Python bindings"
cargo run -q --features ffi --bin uniffi-bindgen -- generate \
  --library target/debug/libopen_harness.so --language python --out-dir bindings/python

# The generated module loads libopen_harness.so from its own directory.
cp target/debug/libopen_harness.so bindings/python/

echo "==> running the Python example"
python3 bindings/python/example.py
