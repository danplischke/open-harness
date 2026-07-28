#!/usr/bin/env python3
"""A minimal stdio MCP server (JSON-RPC 2.0, newline-delimited).

Exists so the open-harness MCP->CLI bridge (#19) has a real server to talk to:
`initialize` -> `tools/list` -> `tools/call` for two toy tools, `echo` and `add`.
Not a general MCP implementation — just enough of the protocol to be real.
"""
import sys
import json

TOOLS = [
    {
        "name": "echo",
        "description": "Echo back the given text.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "add",
        "description": "Add two numbers.",
        "inputSchema": {
            "type": "object",
            "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
            "required": ["a", "b"],
        },
    },
]


def reply(msg_id, result):
    send({"jsonrpc": "2.0", "id": msg_id, "result": result})


def error(msg_id, code, message):
    send({"jsonrpc": "2.0", "id": msg_id, "error": {"code": code, "message": message}})


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def call_tool(name, args):
    if name == "echo":
        return {"content": [{"type": "text", "text": str(args.get("text", ""))}]}
    if name == "add":
        total = (args.get("a", 0) or 0) + (args.get("b", 0) or 0)
        return {"content": [{"type": "text", "text": str(total)}]}
    return None


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception:
            continue
        method = req.get("method")
        msg_id = req.get("id")

        if method == "initialize":
            reply(msg_id, {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "echo-bridge", "version": "0.0.0"},
            })
        elif method == "notifications/initialized":
            pass  # a notification — no reply
        elif method == "tools/list":
            reply(msg_id, {"tools": TOOLS})
        elif method == "tools/call":
            params = req.get("params", {})
            result = call_tool(params.get("name"), params.get("arguments", {}))
            if result is None:
                error(msg_id, -32602, f"unknown tool: {params.get('name')}")
            else:
                reply(msg_id, result)
        elif msg_id is not None:
            error(msg_id, -32601, f"method not found: {method}")


if __name__ == "__main__":
    main()
