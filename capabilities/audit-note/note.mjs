#!/usr/bin/env node
// A language-agnostic open-harness capability, in JavaScript/TypeScript-land.
// Same contract as the Python one: CanonicalPayload in on stdin, Decision out on
// stdout. This one never blocks — it only appends context — which lets the demo
// show deny-wins merge semantics when it runs alongside secret-guard.

let raw = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d) => (raw += d));
process.stdin.on("end", () => {
  let p = {};
  try {
    p = raw.trim() ? JSON.parse(raw) : {};
  } catch {
    process.stdout.write(JSON.stringify({ decision: "allow" }));
    return;
  }
  const tool = (p.tool && p.tool.name) || "unknown";
  const harness = p.harness || "unknown";
  process.stdout.write(
    JSON.stringify({
      decision: "allow",
      context_append: `open-harness audit: about to run tool '${tool}' on harness '${harness}'`,
    })
  );
});
