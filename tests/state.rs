//! `oh state` — the scoped key-value store.
//!
//! The behaviours worth pinning are the ones a capability author would
//! otherwise get wrong by hand: that the three scopes really are separate, that
//! nothing is written into the project tree, that an absent key is
//! distinguishable from an empty one, that a session id cannot escape its
//! directory, and that two capabilities writing at once do not lose a write —
//! the dispatcher fans out concurrently, so that last one is the normal case.

use open_harness::state::{self, Scope};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// `OPEN_HARNESS_STATE_DIR` is process-global, and `cargo test` runs these in
/// parallel threads of one process — so a sandbox that just set the variable
/// would be silently redirected by the next test to start. Every sandbox holds
/// this lock for its lifetime, which serialises exactly the tests that depend
/// on the variable and leaves the rest parallel.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// An isolated state root, so no test touches a real `$HOME`.
struct Sandbox {
    root: PathBuf,
    project: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        // A panicking test poisons the lock; the data it guards is the env var,
        // which the next sandbox overwrites anyway, so recovering is correct.
        let guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("oh-state-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("state");
        let project = base.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::env::set_var("OPEN_HARNESS_STATE_DIR", &root);
        Sandbox {
            root,
            project,
            _guard: guard,
        }
    }

    fn path(&self, scope: Scope, session: Option<&str>) -> PathBuf {
        state::store_path(scope, Some(&self.project), session).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.root.parent().unwrap_or(&self.root));
    }
}

/// Set/get/delete, and the distinction that makes `get` usable from a hook:
/// an absent key is `None`, an empty value is `Some("")`.
#[test]
fn a_value_round_trips_and_absent_is_not_the_same_as_empty() {
    let sb = Sandbox::new("roundtrip");
    let p = sb.path(Scope::Project, None);

    assert_eq!(state::get(&p, "gate.phase").unwrap(), None);
    assert_eq!(state::set(&p, "gate.phase", "plan").unwrap(), None);
    assert_eq!(
        state::get(&p, "gate.phase").unwrap(),
        Some("plan".to_string())
    );

    // Overwriting reports what it displaced, so a caller can act on a change.
    assert_eq!(
        state::set(&p, "gate.phase", "implement").unwrap(),
        Some("plan".to_string())
    );

    state::set(&p, "empty", "").unwrap();
    assert_eq!(state::get(&p, "empty").unwrap(), Some(String::new()));
    assert_ne!(
        state::get(&p, "empty").unwrap(),
        state::get(&p, "never-set").unwrap(),
        "an empty value must not read as an absent key"
    );

    assert!(state::delete(&p, "gate.phase").unwrap());
    assert!(!state::delete(&p, "gate.phase").unwrap());
}

/// Multi-line values survive: a session summary is the motivating case, and it
/// is exactly the shape a naive `key=value` file would corrupt.
#[test]
fn a_multi_line_value_survives_the_yaml_round_trip() {
    let sb = Sandbox::new("multiline");
    let p = sb.path(Scope::Project, None);
    let summary = "goal: ship oh state\nblocker: none\nnext: wire the gates\n  indented: yes\n";
    state::set(&p, "session.summary", summary).unwrap();
    assert_eq!(
        state::get(&p, "session.summary").unwrap().as_deref(),
        Some(summary)
    );
}

/// The three scopes are genuinely separate stores, not three prefixes in one.
#[test]
fn the_scopes_do_not_leak_into_each_other() {
    let sb = Sandbox::new("scopes");
    let user = sb.path(Scope::User, None);
    let project = sb.path(Scope::Project, None);
    let session = sb.path(Scope::Session, Some("sess-1"));
    let other_session = sb.path(Scope::Session, Some("sess-2"));

    state::set(&user, "k", "user").unwrap();
    state::set(&project, "k", "project").unwrap();
    state::set(&session, "k", "session").unwrap();

    assert_eq!(state::get(&user, "k").unwrap().unwrap(), "user");
    assert_eq!(state::get(&project, "k").unwrap().unwrap(), "project");
    assert_eq!(state::get(&session, "k").unwrap().unwrap(), "session");
    assert_eq!(
        state::get(&other_session, "k").unwrap(),
        None,
        "one session's state must not be visible to another"
    );
}

/// Two checkouts of the same repository are two projects. Keying on the
/// directory name alone would silently merge their state.
#[test]
fn two_checkouts_with_the_same_name_get_different_stores() {
    let sb = Sandbox::new("checkouts");
    let a = sb.root.parent().unwrap().join("a/myrepo");
    let b = sb.root.parent().unwrap().join("b/myrepo");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();

    let pa = state::store_path(Scope::Project, Some(&a), None).unwrap();
    let pb = state::store_path(Scope::Project, Some(&b), None).unwrap();
    assert_ne!(pa, pb, "same basename, different path — different store");

    state::set(&pa, "k", "from-a").unwrap();
    state::set(&pb, "k", "from-b").unwrap();
    assert_eq!(state::get(&pa, "k").unwrap().unwrap(), "from-a");
    assert_eq!(state::get(&pb, "k").unwrap().unwrap(), "from-b");

    // The readable half of the key is still there, for a human browsing the dir.
    assert!(state::project_key(&a).starts_with("myrepo-"));
}

/// The rule that keeps a repository clean: state is keyed by the project, never
/// stored in it.
#[test]
fn nothing_is_ever_written_into_the_project_tree() {
    let sb = Sandbox::new("notree");
    for (scope, session) in [
        (Scope::Project, None),
        (Scope::Session, Some("sess-1")),
        (Scope::User, None),
    ] {
        let p = sb.path(scope, session);
        state::set(&p, "k", "v").unwrap();
        assert!(
            p.starts_with(&sb.root),
            "{scope:?} state landed at {} — outside the state root",
            p.display()
        );
        assert!(
            !p.starts_with(&sb.project),
            "{scope:?} state was written into the project tree at {}",
            p.display()
        );
    }
    let leaked: Vec<PathBuf> = walk(&sb.project);
    assert!(
        leaked.is_empty(),
        "the project tree should be untouched, found {leaked:?}"
    );
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// A session id becomes a filename, so a traversal attempt is refused outright
/// rather than sanitised — sanitising would silently merge two sessions.
#[test]
fn a_session_id_cannot_escape_its_directory() {
    let sb = Sandbox::new("traversal");
    for bad in [
        "../../etc/passwd",
        "..",
        ".",
        "a/b",
        "a\\b",
        "",
        "with space",
        "semi;colon",
    ] {
        let r = state::store_path(Scope::Session, Some(&sb.project), Some(bad));
        assert!(r.is_err(), "session id {bad:?} should be refused");
    }
    // A UUID — what Claude Code actually sends — is accepted.
    assert!(state::validate_session("1d559174-f1c0-5421-8619-a95975b4bb37").is_ok());
}

/// The session scope without an id is an error, not a silent fallback to some
/// shared "default session" that would mix conversations together.
#[test]
fn the_session_scope_refuses_to_guess_an_id() {
    let sb = Sandbox::new("nosession");
    let err = state::store_path(Scope::Session, Some(&sb.project), None).unwrap_err();
    assert!(err.contains("--session"), "the error names the fix: {err}");
}

/// Keys are single-line names; a control character would break `state list`.
#[test]
fn keys_are_validated() {
    assert!(state::validate_key("gate.phase").is_ok());
    assert!(state::validate_key("").is_err());
    assert!(state::validate_key("two\nlines").is_err());
    assert!(state::validate_key(&"x".repeat(257)).is_err());
}

/// The reason the lock exists. The dispatcher runs capabilities concurrently,
/// so this is the ordinary case: every writer must survive, not just the last.
#[test]
fn concurrent_writers_do_not_lose_each_other() {
    let sb = Sandbox::new("concurrent");
    let p = sb.path(Scope::Project, None);
    // Create the store first so every thread races on the same file rather than
    // on its creation.
    state::set(&p, "seed", "0").unwrap();

    let threads: Vec<_> = (0..8)
        .map(|i| {
            let path = p.clone();
            std::thread::spawn(move || {
                state::set(&path, &format!("key-{i}"), &format!("value-{i}")).unwrap();
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }

    let all = state::list(&p).unwrap();
    for i in 0..8 {
        assert_eq!(
            all.get(&format!("key-{i}")).map(String::as_str),
            Some(format!("value-{i}").as_str()),
            "writer {i} was lost; the store held {all:?}"
        );
    }
    assert_eq!(all.len(), 9, "8 writers plus the seed");
}

/// A lock left behind by a killed hook must not wedge the store forever.
#[test]
fn a_stale_lock_is_stolen_rather_than_wedging_the_store() {
    let sb = Sandbox::new("stalelock");
    let p = sb.path(Scope::Project, None);
    state::set(&p, "k", "v").unwrap();

    let lock = p.with_extension("lock");
    std::fs::write(&lock, b"").unwrap();
    // Backdate it well past the staleness threshold.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    filetime_set(&lock, old);

    state::set(&p, "k", "v2").expect("a stale lock should be stolen, not waited on forever");
    assert_eq!(state::get(&p, "k").unwrap().unwrap(), "v2");
}

/// Backdate a file's mtime, without pulling in a dependency for it.
///
/// The failure is asserted rather than ignored: a swallowed error here would
/// leave both the prune and stale-lock tests passing vacuously, asserting
/// nothing about the behaviour they exist to cover.
fn filetime_set(path: &Path, when: std::time::SystemTime) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(when))
        .unwrap_or_else(|e| panic!("backdate {}: {e}", path.display()));
}

/// Session state is the one unbounded scope, so it must be prunable.
#[test]
fn prune_removes_only_abandoned_session_stores() {
    let sb = Sandbox::new("prune");
    let fresh = sb.path(Scope::Session, Some("fresh"));
    let old = sb.path(Scope::Session, Some("old"));
    let project = sb.path(Scope::Project, None);
    state::set(&fresh, "k", "v").unwrap();
    state::set(&old, "k", "v").unwrap();
    state::set(&project, "k", "v").unwrap();

    filetime_set(
        &old,
        std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 86_400),
    );

    let pruned = state::prune(30, Some(&sb.project)).unwrap();
    assert_eq!(pruned.len(), 1, "only the abandoned session should go");
    assert!(pruned[0].path.ends_with("old.yaml"));
    assert!(fresh.exists(), "a live session must survive");
    assert!(
        project.exists(),
        "project state is not session state and is never pruned"
    );
}

/// Pruning a project that has never recorded a session is a no-op, not an error.
#[test]
fn prune_on_an_untouched_project_is_a_no_op() {
    let sb = Sandbox::new("prune-empty");
    assert!(state::prune(30, Some(&sb.project)).unwrap().is_empty());
}
