//! `oh state` — the scoped key-value store capabilities keep their state in.
//!
//! A capability is a process that runs for a few milliseconds and exits, so
//! anything it must remember between events has to live outside it. Without a
//! primitive for that, every stateful capability invents its own file: a gate
//! writes `.gate.json`, a session recorder writes `.session-summary`, a nudge
//! counter writes something else — each with its own path convention, its own
//! idea of where a project's state lives, and its own (usually absent) locking.
//! That sprawl is the thing this module exists to prevent, and it is a mistake
//! this project's sibling made first.
//!
//! ## Three scopes
//!
//! * **user** — one store per machine. Cross-project preferences.
//! * **project** — one store per project root. The phase of a gate, the last
//!   review verdict: facts about *this* codebase that outlive a session.
//! * **session** — one store per harness session, inside a project. Working
//!   state for the task in hand, which a `post.session.end` capability can read
//!   back and summarise. The session id comes from the payload's `session`
//!   field, which is exactly why that field exists.
//!
//! ## Why none of it lives in the project tree
//!
//! State is *runtime data*, not configuration, and writing it into the
//! repository is a trap: it dirties `git status`, invites a `.gitignore` entry
//! for something that should never have been there, breaks on a read-only
//! checkout, and leaks one machine's working state into everyone's clone when
//! somebody commits it anyway. So a project's state is keyed *by* its path but
//! stored under `~/.open-harness/state/`, and the project tree is untouched.
//! `.open-harness/` inside a project stays what it already is — the sync
//! lockfile, which *is* configuration and *is* meant to be committed.
//!
//! ## Concurrency is not hypothetical here
//!
//! The dispatcher fans capabilities out **concurrently** on a single event, so
//! two capabilities writing the same scope is the normal case, not the edge
//! one. A read-modify-write without a lock silently loses one of them. Every
//! mutation therefore takes an advisory lock (atomic `create_new`, bounded
//! retry, stale-lock steal) and every write is atomic (temp file + rename), so
//! a crash can never leave a half-written store behind.

use crate::config;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Which store a key lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
    Session,
}

impl Scope {
    pub fn parse(s: &str) -> Result<Scope, String> {
        match s {
            "user" => Ok(Scope::User),
            "project" => Ok(Scope::Project),
            "session" => Ok(Scope::Session),
            other => Err(format!(
                "unknown scope `{other}` — expected one of: user, project, session"
            )),
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
            Scope::Session => "session",
        }
    }
}

/// One store's contents.
///
/// Wrapped in a `values:` map rather than written as a bare top-level mapping so
/// the file stays self-describing and a future metadata key cannot collide with
/// somebody's data key.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

/// How long a lock may sit before it is presumed abandoned by a crashed hook.
const STALE_LOCK: Duration = Duration::from_secs(30);
/// How long to wait for a lock held by a live sibling before giving up.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// The state root: `~/.open-harness/state`, or `$OPEN_HARNESS_STATE_DIR`.
///
/// The override exists so the test suite (and anyone sandboxing a run) can point
/// the whole store somewhere disposable without touching a real `$HOME`.
pub fn root() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("OPEN_HARNESS_STATE_DIR") {
        let p = PathBuf::from(dir);
        if p.as_os_str().is_empty() {
            return Err("OPEN_HARNESS_STATE_DIR is set but empty".to_string());
        }
        return Ok(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            "no home directory: set HOME (or USERPROFILE on Windows), or \
             OPEN_HARNESS_STATE_DIR"
                .to_string()
        })?;
    Ok(home.join(".open-harness").join("state"))
}

/// A stable, human-recognisable directory name for a project root.
///
/// The readable half is for a person looking in `~/.open-harness/state/projects`
/// and wanting to know whose state this is; the hash half is what actually
/// guarantees identity, since two checkouts of the same repository in different
/// directories are different projects and must not share a store.
pub fn project_key(project: &Path) -> String {
    // Canonicalise where possible so `.`, `..` and a symlinked path all land on
    // one key; fall back to the path as given when the directory doesn't exist
    // yet, which is better than refusing to record state for it.
    let canonical = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let text = canonical.to_string_lossy();
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    let digest = h.finalize();
    let hash: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();

    let slug: String = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(40)
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        hash
    } else {
        format!("{slug}-{hash}")
    }
}

/// A key must survive a YAML round-trip and stay greppable in `state list`.
pub fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("a state key cannot be empty".to_string());
    }
    if key.len() > 256 {
        return Err(format!(
            "state key is {} bytes; the limit is 256",
            key.len()
        ));
    }
    if let Some(c) = key.chars().find(|c| c.is_control()) {
        return Err(format!(
            "state key contains a control character ({:?}) — keys are single-line names",
            c
        ));
    }
    Ok(())
}

/// A session id becomes a *filename*, so it is validated as one.
///
/// Rejecting rather than sanitising: quietly rewriting `../../x` into something
/// safe would silently merge two sessions' state, and a wrong answer here is
/// worse than an error.
pub fn validate_session(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("--session is empty".to_string());
    }
    if id.len() > 128 {
        return Err(format!(
            "session id is {} bytes; the limit is 128",
            id.len()
        ));
    }
    if id == "." || id == ".." {
        return Err(format!("`{id}` is not a usable session id"));
    }
    if let Some(c) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.'))
    {
        return Err(format!(
            "session id contains {c:?} — only letters, digits, `-`, `_` and `.` are allowed \
             (it becomes a filename)"
        ));
    }
    Ok(())
}

/// Where a scope's store lives on disk.
///
/// `project` defaults to the current directory, which is where a harness invokes
/// a hook, so a capability that passes nothing still gets the right project.
pub fn store_path(
    scope: Scope,
    project: Option<&Path>,
    session: Option<&str>,
) -> Result<PathBuf, String> {
    let root = root()?;
    match scope {
        Scope::User => Ok(root.join("user.yaml")),
        Scope::Project | Scope::Session => {
            let cwd;
            let project = match project {
                Some(p) => p,
                None => {
                    cwd = std::env::current_dir()
                        .map_err(|e| format!("no project given and cwd is unreadable: {e}"))?;
                    &cwd
                }
            };
            let dir = root.join("projects").join(project_key(project));
            match scope {
                Scope::Project => Ok(dir.join("project.yaml")),
                _ => {
                    let id = session.ok_or_else(|| {
                        "the session scope needs --session ID (a capability reads it from the \
                         payload's `session` field)"
                            .to_string()
                    })?;
                    validate_session(id)?;
                    Ok(dir.join("sessions").join(format!("{id}.yaml")))
                }
            }
        }
    }
}

/// Read a store, treating "not there yet" as "empty".
pub fn load(path: &Path) -> Result<Store, String> {
    if !path.exists() {
        return Ok(Store::default());
    }
    config::load::<Store>(path)
}

/// Write a store atomically: a temp file in the same directory, then a rename.
///
/// Same-directory matters — a rename across filesystems is not atomic, and
/// `$TMPDIR` is routinely a different filesystem from `$HOME`.
fn save(path: &Path, store: &Store) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("state.yaml"),
        std::process::id()
    ));
    config::write(&tmp, store)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename into {}: {e}", path.display())
    })
}

/// An advisory lock over one store file, released on drop.
struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Take the lock for `target`, waiting for a live holder and stealing a dead one.
fn lock(target: &Path) -> Result<Lock, String> {
    let path = target.with_extension("lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let deadline = SystemTime::now() + LOCK_WAIT;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(Lock { path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // A hook that crashed or was killed mid-write leaves its lock
                // behind. Without this the store would be wedged until someone
                // deleted a file they have never heard of.
                let stale = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map(|m| {
                        SystemTime::now()
                            .duration_since(m)
                            .map(|age| age > STALE_LOCK)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if SystemTime::now() >= deadline {
                    return Err(format!(
                        "timed out waiting for the state lock at {} — another capability has \
                         held it for over {}s",
                        path.display(),
                        LOCK_WAIT.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("lock {}: {e}", path.display())),
        }
    }
}

/// Read one key. `None` means absent, which callers distinguish from empty.
pub fn get(path: &Path, key: &str) -> Result<Option<String>, String> {
    validate_key(key)?;
    Ok(load(path)?.values.get(key).cloned())
}

/// Write one key, replacing any previous value. Returns the value it displaced.
pub fn set(path: &Path, key: &str, value: &str) -> Result<Option<String>, String> {
    validate_key(key)?;
    let _guard = lock(path)?;
    let mut store = load(path)?;
    let previous = store.values.insert(key.to_string(), value.to_string());
    save(path, &store)?;
    Ok(previous)
}

/// Remove one key. Returns whether it was there.
pub fn delete(path: &Path, key: &str) -> Result<bool, String> {
    validate_key(key)?;
    let _guard = lock(path)?;
    let mut store = load(path)?;
    let existed = store.values.remove(key).is_some();
    if existed {
        save(path, &store)?;
    }
    Ok(existed)
}

/// Every key in one store, ordered (a `BTreeMap`, so `list` output is stable).
pub fn list(path: &Path) -> Result<BTreeMap<String, String>, String> {
    Ok(load(path)?.values)
}

/// One pruned session store.
pub struct Pruned {
    pub path: PathBuf,
    pub age_days: u64,
}

/// Delete session stores untouched for longer than `older_than_days`.
///
/// Session state is per-conversation and therefore unbounded: without this the
/// store grows forever, one file per session, and a design that leaks by
/// construction is a bug however small each file is. Project and user stores are
/// never pruned — they are keyed by things that persist.
pub fn prune(older_than_days: u64, project: Option<&Path>) -> Result<Vec<Pruned>, String> {
    let probe = store_path(Scope::Session, project, Some("probe"))?;
    let dir = probe
        .parent()
        .ok_or_else(|| "cannot locate the sessions directory".to_string())?
        .to_path_buf();
    let mut pruned = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(pruned); // nothing recorded for this project yet
    };
    let cutoff = Duration::from_secs(older_than_days.saturating_mul(86_400));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let Ok(age) = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| SystemTime::now().duration_since(m).unwrap_or_default())
        else {
            continue;
        };
        if age > cutoff {
            std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
            pruned.push(Pruned {
                path,
                age_days: age.as_secs() / 86_400,
            });
        }
    }
    pruned.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(pruned)
}
