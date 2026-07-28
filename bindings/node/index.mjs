// ESM entry — re-exports the CommonJS wrapper.
import mod from "./index.cjs";

export const {
  harnesses,
  kinds,
  protocolVersion,
  plan,
  planAll,
  dispatch,
  dispatchWithLimits,
  validateDecision,
  verify,
} = mod;

export default mod;
