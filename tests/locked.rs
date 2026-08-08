//! `oh resolve --locked`: the lockfile as a contract rather than an output.
//!
//! Driven through the real binary, because the behaviour under test *is* the
//! CLI's — exit status, and the refusal to write.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn oh() -> Command {
    Command::new(env!("CARGO_BIN_EXE_oh"))
}

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-locked-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir temp");
    p
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// A workdir with one skill and a profile pointing at it.
fn project(tag: &str, body: &str) -> PathBuf {
    let wd = tmp(tag);
    write(
        &wd.join("caps/greet/SKILL.md"),
        &format!("---\ndescription: greeting\n---\n\n{body}\n"),
    );
    write(
        &wd.join("open-harness.yaml"),
        "name: p\nharnesses: [claude-code]\nsources:\n  - local:\n      path: caps\n",
    );
    wd
}

fn resolve(wd: &Path, locked: bool) -> std::process::Output {
    let mut cmd = oh();
    cmd.arg("resolve")
        .arg("--profile")
        .arg(wd.join("open-harness.yaml"));
    if locked {
        cmd.arg("--locked");
    }
    cmd.output().expect("run oh")
}

#[test]
fn locked_without_a_lockfile_refuses_and_says_what_to_run() {
    let wd = project("no-lock", "hello");
    let out = resolve(&wd, true);
    assert!(!out.status.success(), "--locked with no lockfile must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--locked requires an existing") && err.contains("oh resolve"),
        "names the missing file and the fix: {err}"
    );
    assert!(
        !wd.join("open-harness.lock").exists(),
        "--locked never writes the lockfile"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn locked_passes_when_the_lock_matches() {
    let wd = project("match", "hello");
    assert!(resolve(&wd, false).status.success(), "seed the lockfile");
    let before = std::fs::read_to_string(wd.join("open-harness.lock")).unwrap();

    let out = resolve(&wd, true);
    assert!(
        out.status.success(),
        "an up-to-date lock passes: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(wd.join("open-harness.lock")).unwrap(),
        before,
        "and the lockfile is untouched"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn locked_fails_on_a_stale_lock_and_refuses_to_rewrite_it() {
    // This is the whole point: a --locked run that quietly rewrote the lock it
    // was meant to verify would verify nothing.
    let wd = project("stale", "hello");
    assert!(resolve(&wd, false).status.success(), "seed the lockfile");
    let before = std::fs::read_to_string(wd.join("open-harness.lock")).unwrap();

    write(
        &wd.join("caps/greet/SKILL.md"),
        "---\ndescription: greeting\n---\n\nCHANGED BODY\n",
    );

    let out = resolve(&wd, true);
    assert!(!out.status.success(), "a stale lock must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("lockfile is stale"),
        "says the lock is stale: {err}"
    );
    assert!(
        err.contains("greet") && err.contains("content changed"),
        "names what drifted: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(wd.join("open-harness.lock")).unwrap(),
        before,
        "and refuses to rewrite it"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn locked_reports_a_capability_that_appeared() {
    let wd = project("added", "hello");
    assert!(resolve(&wd, false).status.success(), "seed the lockfile");

    write(
        &wd.join("caps/extra/SKILL.md"),
        "---\ndescription: extra\n---\n\nnew\n",
    );

    let out = resolve(&wd, true);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("+ extra") && err.contains("not in the lock"),
        "names the capability that appeared: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[test]
fn locked_reports_a_capability_that_vanished() {
    let wd = project("removed", "hello");
    write(
        &wd.join("caps/extra/SKILL.md"),
        "---\ndescription: extra\n---\n\nnew\n",
    );
    assert!(resolve(&wd, false).status.success(), "seed the lockfile");
    std::fs::remove_dir_all(wd.join("caps/extra")).unwrap();

    let out = resolve(&wd, true);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("- extra") && err.contains("no source provides it"),
        "names the capability that vanished: {err}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}
