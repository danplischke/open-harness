// Node/TS usage of the open-harness core. Run: `node example.mjs`
// (after `bash build.sh`).
import {
  harnesses,
  kinds,
  protocolVersion,
  plan,
  dispatch,
  verify,
} from "./index.mjs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const caps = resolve(here, "../../capabilities");

console.log("harnesses:", harnesses().join(", "));
console.log("kinds    :", kinds().join(", "), "| protocol:", protocolVersion());

// 1) Plan a capability for one harness (generative — no execution).
const manifest = JSON.stringify({
  id: "demo",
  kind: "skill",
  description: "A concise-answers skill.",
  skill: { body: "Prefer concise answers." },
});
const p = plan(manifest, ".", "claude-code");
console.log(
  `\nplan(demo → claude-code): ${p.installability}, ${p.artifacts.length} artifact → ${p.artifacts[0]?.path}`
);

// 2) Dispatch a tool call through the REAL capabilities (spawns them in-process).
const secret = JSON.stringify({
  tool_name: "Bash",
  tool_input: { command: "echo AKIAIOSFODNN7EXAMPLE" },
});
const r = dispatch("opencode", "pre.tool.shell", secret, caps);
const decision = JSON.parse(r.stdout || "{}").decision;
console.log(`\ndispatch(opencode, secret): ${decision}  (ran: ${r.ran.join(", ")})`);

const safe = JSON.stringify({
  tool_name: "Bash",
  tool_input: { command: "ls -la" },
});
const ok = dispatch("claude-code", "pre.tool.shell", safe, caps);
console.log(`dispatch(claude-code, safe): exit ${ok.exit_code}`);

// 3) Verify a capability directory's signature (unsigned here).
const v = verify(resolve(caps, "secret-guard"));
console.log(`\nverify(secret-guard): ${v.status}`);
