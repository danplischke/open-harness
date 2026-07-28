// An OpenCode plugin that enforces open-harness hooks **in process**, backed by
// the native core — the win #14 is about: no shelling out to `oh-dispatch` for
// orchestration (the core spawns the language-agnostic capabilities directly).
//
// Drop this in `.opencode/plugins/` (and `npm i @open-harness/node`), or point
// OPEN_HARNESS_CAPS at your capabilities directory.
//
// OpenCode's plugin contract: export an async factory returning event handlers.
import { dispatch } from "@open-harness/node";
import { resolve } from "node:path";

const CAPS = process.env.OPEN_HARNESS_CAPS || resolve(process.cwd(), "capabilities");

export const OpenHarness = async () => ({
  // Fires before OpenCode runs a tool. We map it to the normalized pre.tool
  // event, run every bound capability through the core, and honor a deny.
  "tool.execute.before": async (input, _output) => {
    const payload = JSON.stringify({
      tool_name: input.tool,
      tool_input: input.args,
    });
    const result = dispatch("opencode", "pre.tool.shell", payload, CAPS);
    // OpenCode is an in-process harness: the decision is canonical JSON on stdout.
    const decision = JSON.parse(result.stdout || "{}");
    if (decision.decision === "deny") {
      throw new Error(decision.reason || "blocked by open-harness");
    }
    // A capability that errored on a blocking event fails closed (already a deny
    // above); errored ids are surfaced for logging.
    if (result.errored.length) {
      console.warn("[open-harness] capability errors:", result.errored.join("; "));
    }
  },
});
