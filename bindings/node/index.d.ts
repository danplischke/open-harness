// TypeScript types for @open-harness/node.
// Field names are the core's (serde) names — snake_case on result objects.

// ---- Authoring contract ----------------------------------------------------
// The shapes a capability reads on stdin (CanonicalPayload) and writes on
// stdout (Decision). These mirror the frozen `hook@1` wire protocol, so a
// TypeScript hook can type its input and output against them.

export type Phase = "pre" | "post";
export type SubjectKind =
  | "tool"
  | "model"
  | "prompt"
  | "session"
  | "subagent"
  | "task";
export type ToolClass =
  | "any"
  | "shell"
  | "file_read"
  | "file_write"
  | "file_edit"
  | "mcp"
  | "web";
export type Boundary = "start" | "end";
export type TaskKind = "start" | "resume" | "cancel";

/** A normalized event coordinate `(phase, subject, tool_class?)`. */
export interface NormEvent {
  phase: Phase;
  subject: SubjectKind;
  tool_class?: ToolClass;
  boundary?: Boundary;
  task_kind?: TaskKind;
}

export interface ToolInfo {
  name: string;
  /** Harness-native tool input; shape varies by tool. */
  input: unknown;
}

/** What the dispatcher hands a capability on stdin. */
export interface CanonicalPayload {
  /** Always "open-harness/hook@1". */
  protocol: string;
  harness: string;
  event: NormEvent;
  /** Whether a `deny` will actually be honored on this event. */
  blocking: boolean;
  tool?: ToolInfo;
  prompt?: string;
  cwd?: string;
  /** The untouched native payload — an escape hatch, at the cost of portability. */
  raw: unknown;
}

export type Verdict = "allow" | "deny" | "modify";

/** What a capability writes on stdout. `{ decision: "allow" }` is the no-op. */
export interface Decision {
  decision: Verdict;
  reason?: string;
  /** Extra model context to append (on any event). */
  context_append?: string;
  /** Replacement tool input when `decision` is "modify". */
  modified_input?: unknown;
}

// ---- Planning & results ----------------------------------------------------

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
