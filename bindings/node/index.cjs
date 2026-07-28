"use strict";
// Loads the platform-specific native addon and wraps the JSON-string boundary in
// a clean typed API (results are parsed; inputs that are "arbitrary data" stay
// JSON strings, matching the core's design).
const { platform, arch } = process;
const file = `./open-harness.${platform}-${arch}.node`;

let native;
try {
  native = require(file);
} catch (cause) {
  throw new Error(
    `open-harness: native addon '${file}' not found — build it with bindings/node/build.sh (${cause.message})`
  );
}

const parse = (s) => JSON.parse(s);

module.exports = {
  harnesses: () => native.harnesses(),
  kinds: () => native.kinds(),
  protocolVersion: () => native.protocolVersion(),
  plan: (manifestJson, dir, harness) => parse(native.plan(manifestJson, dir, harness)),
  planAll: (manifestJson, dir) => parse(native.planAll(manifestJson, dir)),
  dispatch: (harness, eventId, nativeStdinJson, capabilitiesDir) =>
    parse(native.dispatch(harness, eventId, nativeStdinJson, capabilitiesDir)),
  dispatchWithLimits: (
    harness,
    eventId,
    nativeStdinJson,
    capabilitiesDir,
    timeoutMs = 0,
    maxOutputBytes = 0
  ) =>
    parse(
      native.dispatchWithLimits(
        harness,
        eventId,
        nativeStdinJson,
        capabilitiesDir,
        timeoutMs,
        maxOutputBytes
      )
    ),
  validateDecision: (decisionJson) => native.validateDecision(decisionJson),
  verify: (dir, trustJson) => parse(native.verify(dir, trustJson ?? null)),
};
