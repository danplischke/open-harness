#!/usr/bin/env python3
"""A language-agnostic open-harness capability, in Python.

Contract: read a CanonicalPayload as JSON on stdin, write a Decision as JSON on
stdout. Nothing here knows or cares which harness invoked it.
"""
import sys
import json
import re

PATTERNS = [
    (r"AKIA[0-9A-Z]{16}", "aws-access-key-id"),
    (r"-----BEGIN [A-Z ]*PRIVATE KEY-----", "private-key-block"),
    (r"ghp_[A-Za-z0-9]{36}", "github-pat"),
    (r"(?i)(api[_-]?key|secret|token)\s*[:=]\s*['\"]?[A-Za-z0-9_\-]{16,}", "inline-secret-assignment"),
]


def main() -> None:
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw) if raw.strip() else {}
    except Exception:
        # On unparseable input, do not veto (fail-open at the capability; the
        # dispatcher decides overall error policy).
        print(json.dumps({"decision": "allow"}))
        return

    tool = payload.get("tool") or {}
    blob = json.dumps(tool.get("input", ""))

    for pattern, label in PATTERNS:
        if re.search(pattern, blob):
            print(json.dumps({
                "decision": "deny",
                "reason": f"potential secret in tool input matched rule '{label}'",
            }))
            return

    print(json.dumps({"decision": "allow"}))


if __name__ == "__main__":
    main()
