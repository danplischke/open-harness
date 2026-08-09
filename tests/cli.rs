//! CLI argument handling.
//!
//! The parser fills one flat `Opts` for every subcommand, so a mistyped or
//! value-less flag used to fall through to a default rather than being rejected.
//! That is the worst shape for a tool that installs code and gates on
//! signatures: the run continues, having quietly done the opposite of what was
//! asked. These pin the refusals.

use std::path::Path;

fn oh(args: &[&str], cwd: &Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run oh")
}

fn tmp(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-cli-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A flag we do not know is a typo. Absorbing it silently means running with the
/// opposite of what was asked — a misspelt `--require-signed` installs unsigned
/// code, a misspelt `--dry-run` writes to the project.
#[test]
fn an_unknown_flag_is_refused() {
    let wd = tmp("unknown");
    for bad in [
        "--require-signd",
        "--dry-runn",
        "--markdwon",
        "--nonsense",
        "-x",
    ] {
        let out = oh(&["matrix", bad], &wd);
        assert!(
            !out.status.success(),
            "`{bad}` must be refused, not ignored"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("unknown option") && err.contains(bad),
            "the message must name the flag: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&wd);
}

/// A flag whose value is missing used to read as `None` and be dropped, taking
/// the next argument with it — `oh run --harness` dispatched against no harness.
#[test]
fn a_flag_without_its_value_is_refused() {
    let wd = tmp("noval");
    for args in [
        vec!["run", "--harness"],
        vec!["check", "--capabilities"],
        vec!["sync", "--into"],
        vec!["run", "--harness", "--event", "pre.tool.any"],
    ] {
        let out = oh(&args, &wd);
        assert!(
            !out.status.success(),
            "{args:?} must be refused, not silently defaulted"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("needs a value"), "{args:?} -> {err}");
    }
    let _ = std::fs::remove_dir_all(&wd);
}

/// A numeric flag that will not parse used to fall back to `0`, which means
/// "use the default" — so a typo'd timeout silently ran with the wrong one.
#[test]
fn a_non_numeric_value_is_refused() {
    let wd = tmp("num");
    for args in [
        vec!["run", "--harness", "claude-code", "--timeout-ms", "5o00"],
        vec!["run", "--harness", "claude-code", "--max-output-kb", "lots"],
    ] {
        let out = oh(&args, &wd);
        assert!(!out.status.success(), "{args:?} must be refused");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("wants a number"), "{args:?} -> {err}");
    }
    let _ = std::fs::remove_dir_all(&wd);
}

/// `--header` without a colon used to be dropped on the floor, sending the
/// request without the header that was asked for — usually the auth one.
#[test]
fn a_malformed_header_argument_is_refused() {
    let wd = tmp("hdr");
    let out = oh(
        &[
            "mcp",
            "list",
            "--url",
            "http://127.0.0.1:1/mcp",
            "--header",
            "Authorization Bearer abc",
        ],
        &wd,
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--header"), "{err}");
    let _ = std::fs::remove_dir_all(&wd);
}

/// `discover` promises a malformed capability is a hard error, never a silent
/// skip. The CLI wrapper used to turn any such error into an empty set, so the
/// caller could not tell "nothing to load" from "could not read any of it".
#[test]
fn an_unreadable_capability_tree_fails_loudly() {
    let wd = tmp("broken");
    // A capability whose manifest will not parse.
    std::fs::create_dir_all(wd.join("caps/broken")).unwrap();
    std::fs::write(wd.join("caps/broken/capability.yaml"), "id: [unclosed\n").unwrap();

    let out = oh(&["check", "--capabilities", "caps"], &wd);
    assert!(
        !out.status.success(),
        "a malformed capability must fail the run"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("could not load capabilities"),
        "and say so, rather than reporting an empty set: {err}"
    );
    assert!(
        !err.contains("no capabilities found"),
        "‘none found’ is the wrong diagnosis for ‘could not read’: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// Valid invocations still work — the refusals above must not be over-eager.
#[test]
fn ordinary_invocations_still_parse() {
    let wd = tmp("ok");
    for args in [
        vec!["matrix"],
        vec!["matrix", "--markdown"],
        vec!["version"],
    ] {
        let out = oh(&args, &wd);
        assert!(
            out.status.success(),
            "{args:?} must still work: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // A positional argument (`oh mcp list`) is not a flag and must pass through.
    let out = oh(
        &["mcp", "list", "--id", "nope", "--capabilities", "caps"],
        &wd,
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("unknown option"),
        "a subcommand argument is not an unknown flag: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// `oh check | head` used to end in a Rust backtrace and exit 101.
///
/// Rust sets `SIGPIPE` to `SIG_IGN` before `main`, so a closed downstream pipe
/// comes back as an `EPIPE` write error — and `println!` panics on those. Every
/// shell idiom this tool invites (`| head`, `| less` then `q`, `| grep -m1`)
/// hit it. The fix restores `SIG_DFL`, so the process dies on the signal like
/// anything else in a pipeline would.
#[cfg(unix)]
#[test]
fn a_closed_pipe_is_not_a_panic() {
    use std::process::{Command, Stdio};

    // `head -1` exits after one line, closing the pipe under a command that
    // writes many.
    let mut producer = Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(["check"])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oh");

    let head = Command::new("head")
        .arg("-1")
        .stdin(Stdio::from(producer.stdout.take().expect("stdout")))
        .stdout(Stdio::piped())
        .output();
    let head = match head {
        Ok(h) => h,
        Err(_) => {
            eprintln!("no `head` available; skipped");
            let _ = producer.kill();
            let _ = producer.wait();
            return;
        }
    };
    assert!(!head.stdout.is_empty(), "head should have read a line");

    let mut err = String::new();
    if let Some(mut e) = producer.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut err);
    }
    let status = producer.wait().expect("wait");
    assert!(
        !err.contains("panicked"),
        "a closed pipe must not panic:\n{err}"
    );
    // 141 = 128 + SIGPIPE(13). Exit 0 is fine too if it finished first.
    assert!(
        matches!(status.code(), None | Some(0) | Some(141)),
        "unexpected exit {:?} — 101 means it panicked",
        status.code()
    );
}
