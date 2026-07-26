"""Embed open-harness from Python via the uniffi bindings.

Run from the repo root:  python3 bindings/python/example.py
(bindings/python/ must hold the generated open_harness.py + libopen_harness.so;
see bindings/README.md for how to regenerate them.)
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import open_harness as oh  # noqa: E402

print("protocol:", oh.protocol_version())
print("harnesses:", ", ".join(oh.harnesses()))
print("kinds:    ", ", ".join(oh.kinds()))

assert oh.protocol_version() == "hook@1"
assert "claude-code" in oh.harnesses() and len(oh.harnesses()) == 10
assert "tool" in oh.kinds()

HOOK = (
    '{"id":"g","kind":"hook","run":{"command":"true"},'
    '"events":[{"phase":"pre","subject":"tool","tool_class":"any","required":true}]}'
)
p = oh.plan(HOOK, ".", "claude-code")
print("\nplan(claude):", p.installability, "->", p.artifacts[0].kind)
assert p.installability == "clean" and p.artifacts[0].kind == "registration"
assert oh.plan(HOOK, ".", "aider").installability == "unsupported"

SECRET = '{"tool_name":"Bash","tool_input":{"command":"echo AKIAIOSFODNN7EXAMPLE"}}'
r = oh.dispatch("claude-code", "pre.tool.any", SECRET, "capabilities")
print("dispatch(secret): exit", r.exit_code, "ran", r.ran)
assert r.exit_code == 2 and "secret-guard" in r.ran

try:
    oh.plan(HOOK, ".", "not-a-harness")
    raise SystemExit("expected an ApiError")
except oh.ApiError.Harness as e:
    print("error path OK:", e)

print("\nPYTHON BINDINGS OK")
