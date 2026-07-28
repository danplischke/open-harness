//! Hardened capability execution (#3).
//!
//! The spike spawned a capability, wrote its stdin, and blocked on
//! `wait_with_output` — no timeout, no output bound, and only the process's exit
//! status distinguished failures. Production needs **bounded** (a hung
//! capability can't wedge the harness), **observable** (every failure has a
//! class), and **policy-driven** execution. This module owns that; the
//! dispatcher (`dispatch.rs`) owns fan-out, policy resolution, and merge.
//!
//! Execution is std-only (no async runtime): the child's stdin/stdout/stderr are
//! serviced by dedicated threads so a chatty or silent child can't deadlock the
//! pipe, and the parent polls `try_wait` until the deadline, then kills. Output
//! is read up to a byte cap; overflow is a first-class error rather than a
//! truncated-JSON parse failure.

use crate::manifest::{LoadedCapability, RunSpec};
use crate::model::{CanonicalPayload, Decision};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runtime limits applied to every capability, unless a manifest overrides the
/// timeout. Cloneable and cheap so the dispatcher can share it across threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunLimits {
    /// Default kill-after wall-clock timeout (ms) when a capability sets none.
    pub timeout_ms: u64,
    /// Max bytes retained from stdout/stderr each; overflow is an `OutputCap`.
    pub max_output_bytes: usize,
    /// How often the parent polls the child for exit (ms).
    pub poll_interval_ms: u64,
}

impl Default for RunLimits {
    fn default() -> Self {
        RunLimits {
            timeout_ms: 5_000,
            max_output_bytes: 1 << 20, // 1 MiB
            poll_interval_ms: 5,
        }
    }
}

/// The failure taxonomy — every way a capability can fail to yield a decision.
/// Each variant maps to a stable `class()` string surfaced via `--explain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The process could not be started (bad interpreter, not found, …).
    Spawn(String),
    /// Killed after exceeding its timeout (ms).
    Timeout(u64),
    /// Exited non-zero; carries the code and captured (capped) stderr.
    NonZeroExit(i32, String),
    /// stdout was not a valid `hook@1` decision document.
    BadJson(String),
    /// The capability's declared protocol is incompatible with the dispatcher.
    Protocol(String),
    /// stdout (or stderr) exceeded the output cap.
    OutputCap(String),
    /// An IO error servicing the child (stdin write, wait, thread panic).
    Io(String),
}

impl RunError {
    /// Stable, kebab-case class for reporting and tests.
    pub fn class(&self) -> &'static str {
        match self {
            RunError::Spawn(_) => "spawn-error",
            RunError::Timeout(_) => "timeout",
            RunError::NonZeroExit(_, _) => "non-zero-exit",
            RunError::BadJson(_) => "bad-json",
            RunError::Protocol(_) => "protocol-mismatch",
            RunError::OutputCap(_) => "output-cap",
            RunError::Io(_) => "io-error",
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(m) => write!(f, "spawn failed: {m}"),
            RunError::Timeout(ms) => write!(f, "timed out after {ms}ms (killed)"),
            RunError::NonZeroExit(code, err) => {
                write!(f, "exit {code}")?;
                if !err.is_empty() {
                    write!(f, ": {err}")?;
                }
                Ok(())
            }
            RunError::BadJson(m) => write!(f, "invalid decision JSON: {m}"),
            RunError::Protocol(m) => write!(f, "protocol: {m}"),
            RunError::OutputCap(m) => write!(f, "output cap exceeded: {m}"),
            RunError::Io(m) => write!(f, "io: {m}"),
        }
    }
}

impl std::error::Error for RunError {}

/// Run one capability to a decision, bounded by `limits` (with a per-manifest
/// timeout override). Every failure path returns a classified [`RunError`].
pub fn run_capability(
    cap: &LoadedCapability,
    payload: &CanonicalPayload,
    limits: &RunLimits,
) -> Result<Decision, RunError> {
    // Refuse an incompatible protocol before spawning anything.
    crate::model::negotiate(cap.manifest.protocol.as_deref()).map_err(RunError::Protocol)?;

    let run = cap
        .manifest
        .run
        .as_ref()
        .ok_or_else(|| RunError::Spawn("hook capability has no `run` entrypoint".into()))?;

    let input = serde_json::to_string(payload).map_err(|e| RunError::Io(e.to_string()))?;

    // Resolve a concrete (program, argv) for this OS — no shebang reliance.
    let (program, argv) = resolve_program(run);
    let mut child = Command::new(&program)
        .args(&argv)
        .current_dir(&cap.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RunError::Spawn(format!("{program}: {e}")))?;

    // Service the three pipes on their own threads so neither a silent child
    // (that never reads stdin) nor a chatty one (that floods stdout) deadlocks.
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdin_thread = stdin.map(|mut h| {
        std::thread::spawn(move || {
            // A child that ignores stdin is fine; ignore the broken-pipe error.
            let _ = h.write_all(input.as_bytes());
        })
    });
    let cap_bytes = limits.max_output_bytes;
    let stdout_thread = stdout.map(|h| std::thread::spawn(move || read_capped(h, cap_bytes)));
    let stderr_thread = stderr.map(|h| std::thread::spawn(move || read_capped(h, cap_bytes)));

    // Poll for exit until the deadline; kill on timeout.
    let timeout_ms = cap.manifest.timeout_ms.unwrap_or(limits.timeout_ms);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let poll = Duration::from_millis(limits.poll_interval_ms.max(1));
    let exited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None; // timed out
                }
                std::thread::sleep(poll);
            }
            Err(e) => return Err(RunError::Io(format!("wait: {e}"))),
        }
    };

    let Some(status) = exited else {
        // Detach the IO threads instead of joining: a capability that forked
        // grandchildren can keep the pipes open past the kill, and the runtime
        // must not block on that. Captured output is irrelevant on a timeout.
        drop(stdin_thread);
        drop(stdout_thread);
        drop(stderr_thread);
        return Err(RunError::Timeout(timeout_ms));
    };

    if let Some(t) = stdin_thread {
        let _ = t.join();
    }
    let (stdout_bytes, stdout_over) = join_reader(stdout_thread).unwrap_or_default();
    let (stderr_bytes, _stderr_over) = join_reader(stderr_thread).unwrap_or_default();

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        let mut err = String::from_utf8_lossy(&stderr_bytes).trim().to_string();
        if _stderr_over {
            err.push_str(" …(truncated)");
        }
        return Err(RunError::NonZeroExit(code, err));
    }
    if stdout_over {
        return Err(RunError::OutputCap(format!(
            "stdout exceeded {cap_bytes} bytes"
        )));
    }

    let stdout = String::from_utf8_lossy(&stdout_bytes);
    crate::model::parse_decision(&stdout).map_err(RunError::BadJson)
}

type Reader = std::thread::JoinHandle<(Vec<u8>, bool)>;

/// Join a reader thread, mapping a panic to an empty/overflowed-false result.
fn join_reader(t: Option<Reader>) -> Option<(Vec<u8>, bool)> {
    t.map(|h| h.join().unwrap_or_else(|_| (Vec::new(), false)))
}

// ---- cross-platform interpreter resolution (#4) ---------------------------

/// Resolve a capability's `run` into a concrete `(program, argv)` for the
/// current OS — **no `#!` shebang required** (Windows has none). Order:
///   1. explicit `interpreter` → run `command` (the script) through it;
///   2. else infer the interpreter from the script extension (`.py`, `.js`, …);
///   3. else run `command` directly, remapping known interpreter aliases across
///      platforms (`python3` → `python` on Windows) and leaving anything else
///      (compiled binaries, `./guard`) untouched.
pub fn resolve_program(run: &RunSpec) -> (String, Vec<String>) {
    if let Some(logical) = run.interpreter.as_deref() {
        return (resolve_interpreter(logical), script_argv(run));
    }
    if let Some(logical) = interpreter_for_ext(&run.command) {
        return (resolve_interpreter(logical), script_argv(run));
    }
    let program = match interpreter_candidates(&run.command) {
        Some(cands) => first_on_path(&cands).unwrap_or_else(|| run.command.clone()),
        None => run.command.clone(), // pass through; Command does PATH resolution
    };
    (program, run.args.clone())
}

/// argv when `command` is a script: the script followed by the declared args.
fn script_argv(run: &RunSpec) -> Vec<String> {
    let mut argv = Vec::with_capacity(run.args.len() + 1);
    argv.push(run.command.clone());
    argv.extend(run.args.iter().cloned());
    argv
}

/// Resolve a *logical* interpreter to a concrete executable path (best-first,
/// per-OS), falling back to the first candidate / the name itself.
fn resolve_interpreter(logical: &str) -> String {
    match interpreter_candidates(logical) {
        Some(cands) => first_on_path(&cands).unwrap_or_else(|| cands[0].clone()),
        None => which(logical).unwrap_or_else(|| logical.to_string()),
    }
}

/// Concrete candidate executables for a logical interpreter, best-first, per-OS.
/// `None` for anything that isn't a recognized interpreter.
fn interpreter_candidates(logical: &str) -> Option<Vec<String>> {
    let list = |v: &[&str]| Some(v.iter().map(|s| s.to_string()).collect());
    match logical {
        "python" | "python3" => {
            if cfg!(windows) {
                list(&["python", "python3", "py"])
            } else {
                list(&["python3", "python"])
            }
        }
        "node" | "nodejs" => list(&["node"]),
        "deno" => list(&["deno"]),
        "bun" => list(&["bun"]),
        "ruby" => list(&["ruby"]),
        "bash" => {
            if cfg!(windows) {
                list(&["bash"])
            } else {
                list(&["bash", "sh"])
            }
        }
        "sh" => {
            if cfg!(windows) {
                list(&["sh", "bash"])
            } else {
                list(&["sh"])
            }
        }
        _ => None,
    }
}

/// Infer a logical interpreter from a script's file extension.
fn interpreter_for_ext(command: &str) -> Option<&'static str> {
    let ext = Path::new(command)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match ext.as_str() {
        "py" => Some("python"),
        "js" | "cjs" | "mjs" => Some("node"),
        "ts" => Some("node"),
        "rb" => Some("ruby"),
        "sh" | "bash" => Some("bash"),
        _ => None,
    }
}

/// The first candidate found on `PATH` (its full path), or `None`.
fn first_on_path(cands: &[String]) -> Option<String> {
    cands.iter().find_map(|c| which(c))
}

/// A minimal `which`: resolve `program` to a full path via `PATH`, honoring
/// `PATHEXT` on Windows. A name containing a path separator is treated as a path.
fn which(program: &str) -> Option<String> {
    if program.contains('/') || program.contains('\\') {
        let p = Path::new(program);
        return p.is_file().then(|| program.to_string());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let direct = dir.join(program);
        if direct.is_file() {
            return Some(direct.to_string_lossy().into_owned());
        }
        if cfg!(windows) {
            let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into());
            for ext in exts.split(';').filter(|e| !e.is_empty()) {
                let cand = dir.join(format!("{program}{ext}"));
                if cand.is_file() {
                    return Some(cand.to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

/// Read a stream, keeping at most `cap` bytes but draining the rest so the child
/// never blocks on a full pipe. Returns `(bytes, overflowed)`.
fn read_capped<R: Read>(mut r: R, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut overflowed = false;
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if kept.len() < cap {
                    let room = cap - kept.len();
                    kept.extend_from_slice(&buf[..n.min(room)]);
                    if n > room {
                        overflowed = true;
                    }
                } else {
                    overflowed = true;
                    // keep draining, discard
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    (kept, overflowed)
}
