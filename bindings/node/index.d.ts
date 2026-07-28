// TypeScript types for @open-harness/node.
// Field names are the core's (serde) names — snake_case on result objects.

export type Installability = "clean" | "degraded" | "unsupported";

export interface PlanArtifact {
  /** "file" (path + contents) or "registration" (native config snippet in contents). */
  kind: "file" | "registration";
  path: string;
  contents: string;
}

export interface Plan {
  harness: string;
  installability: Installability;
  /** Degradation / unsupported reason; empty when clean. */
  detail: string;
  artifacts: PlanArtifact[];
  notes: string[];
}

export interface DispatchResult {
  exit_code: number;
  stdout: string;
  stderr: string;
  ran: string[];
  skipped: string[];
  errored: string[];
}

export interface VerifyResult {
  /** e.g. "trusted (alice)" / "UNTRUSTED (oh:…)" / "INVALID — tampered" / "unsigned". */
  status: string;
  trusted: boolean;
  /** Passes when signatures are not required (Invalid never passes). */
  passes_lax: boolean;
  /** Passes under require-signed (only Trusted). */
  passes_strict: boolean;
  /** Declared permission manifest, summarized (empty if none). */
  permissions: string;
}

export function harnesses(): string[];
export function kinds(): string[];
export function protocolVersion(): string;

export function plan(manifestJson: string, dir: string, harness: string): Plan;
export function planAll(manifestJson: string, dir: string): Plan[];

/** Run the hook dispatcher (spawns capabilities). Throws on error. */
export function dispatch(
  harness: string,
  eventId: string,
  nativeStdinJson: string,
  capabilitiesDir: string
): DispatchResult;

export function dispatchWithLimits(
  harness: string,
  eventId: string,
  nativeStdinJson: string,
  capabilitiesDir: string,
  timeoutMs?: number,
  maxOutputBytes?: number
): DispatchResult;

/** Validate a decision document against hook@1. Throws on invalid. */
export function validateDecision(decisionJson: string): void;

/** Verify a capability directory against an optional trust store (JSON string). */
export function verify(dir: string, trustJson?: string | null): VerifyResult;
