//! `oh try` — previewing a source without adopting it.
//!
//! The property under test is mostly a *negative* one, and it is the reason the
//! command exists: you can point `try` at a stranger's repository and it will
//! tell you what would land on your machine without any of it landing. So the
//! first test here is not "does it print the right thing" but "did it write
//! anything at all" — a preview that quietly created a lockfile, or dropped a
//! `.claude/` into the directory you were standing in, would be worse than no
//! preview, because you would have trusted it.
//!
//! The rest pin what the report must contain. Each one exists because omitting
//! it turns the preview into the friendlier lie: the paths (so you know where it
//! lands), the degradations and the unsupported half (so "works everywhere" is
//! never implied), the permission asks (so exec/network/write are visible before
//! you consent, not after), and the harness set (so a preview that narrowed
//! itself says so).

use std::path::{Path, PathBuf};

fn oh(args: &[&str], cwd: &Path, home: &Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(args)
        .current_dir(cwd)
        // `try` roots its fetch cache at the home directory. Point it at the
        // scratch tree so a test run can never touch the developer's own, and
        // so "nothing was written" is checkable rather than asserted.
        .env("HOME", home)
        .env("USERPROFILE", home)
        .output()
        .expect("run oh")
}

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("oh-try-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("proj")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        Scratch { root }
    }
    fn proj(&self) -> PathBuf {
        self.root.join("proj")
    }
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }
    /// The capability source `try` is pointed at.
    fn src(&self) -> PathBuf {
        self.root.join("src")
    }
    fn write(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        oh(args, &self.proj(), &self.home())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn out(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A skill: installable on some harnesses, impossible on others.
fn a_skill(s: &Scratch) {
    s.write(
        "src/commit-style/SKILL.md",
        "---\nname: Commit Style\ndescription: House commit conventions.\n---\nUse conventional commits.\n",
    );
}

/// Every entry in a directory tree, relative to it — so "wrote nothing" can be
/// asserted against the whole tree rather than against the paths we guessed.
fn tree(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, acc: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            acc.push(
                p.strip_prefix(base)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            if p.is_dir() {
                walk(&p, base, acc);
            }
        }
    }
    let mut acc = Vec::new();
    walk(root, root, &mut acc);
    acc.sort();
    acc
}

/// The whole point. `try` reads a source and reports; it does not adopt it.
/// Nothing in the directory you ran it from, and nothing in your home tree
/// beyond the fetch cache, may change.
#[test]
fn try_writes_nothing() {
    let s = Scratch::new("nothing");
    a_skill(&s);
    let before_proj = tree(&s.proj());
    let o = s.run(&["try", s.src().to_str().unwrap(), "--harness", "claude"]);
    assert!(o.status.success(), "{}", out(&o));

    assert_eq!(
        tree(&s.proj()),
        before_proj,
        "try must not write into the directory it was run from"
    );
    // The source itself is someone else's tree — a preview does not annotate it.
    assert!(
        !s.src().join("open-harness.lock").exists(),
        "try must not leave a lockfile beside the source"
    );
    // Only `.open-harness/cache/` may appear at home, and for a local source not
    // even that: there is nothing to fetch.
    let at_home = tree(&s.home());
    assert!(
        at_home.iter().all(|p| p.starts_with(".open-harness/cache")),
        "try touched the home directory outside its fetch cache: {at_home:?}"
    );
}

/// The paths. A preview that says "installable" without saying *where* leaves
/// you to run `sync` to find out, which is the thing you were avoiding.
#[test]
fn try_reports_the_path_each_harness_would_get() {
    let s = Scratch::new("paths");
    a_skill(&s);
    let o = s.run(&["try", s.src().to_str().unwrap(), "--harness", "all"]);
    let text = out(&o);
    assert!(o.status.success(), "{text}");
    for expected in [
        ".claude/skills/commit-style/SKILL.md",
        ".cursor/skills/commit-style/SKILL.md",
        ".opencode/skills/commit-style/SKILL.md",
    ] {
        assert!(text.contains(expected), "missing {expected} in:\n{text}");
    }
}

/// The half that does not work. Four harnesses have no skills directory at all;
/// a preview that listed only the six successes would read as "works
/// everywhere", which is the specific dishonesty this project exists to avoid.
#[test]
fn try_reports_the_harnesses_that_cannot_host_it() {
    let s = Scratch::new("blocked");
    a_skill(&s);
    let text = out(&s.run(&["try", s.src().to_str().unwrap(), "--harness", "all"]));
    assert!(
        text.contains("unsupported"),
        "the unsupported targets must be named:\n{text}"
    );
    for h in ["aider", "gemini"] {
        assert!(
            text.contains(h),
            "{h} cannot host a skill and must be listed as such:\n{text}"
        );
    }
}

/// What it asks for, always — including when the answer is "nothing". Printing
/// the line only when a capability wants something would make its absence
/// ambiguous: not asked for, or not reported?
#[test]
fn try_reports_the_permission_asks_even_when_empty() {
    let s = Scratch::new("asks");
    a_skill(&s);
    let quiet = out(&s.run(&["try", s.src().to_str().unwrap(), "--harness", "claude"]));
    assert!(
        quiet.contains("network none")
            && quiet.contains("exec none")
            && quiet.contains("writes none"),
        "a capability that asks for nothing must say so:\n{quiet}"
    );

    let s2 = Scratch::new("asks2");
    s2.write(
        "src/web/capability.yaml",
        "id: web\nname: Web\nkind: tool\ntool:\n  provider: exec\n  delivery: cli\n  exec:\n    command: curl\n  usage: curl <url>\npermissions:\n  network: [\"*\"]\n  exec: [curl]\n  write: [\"./cache/**\"]\n",
    );
    let loud = out(&s2.run(&["try", s2.src().to_str().unwrap(), "--harness", "claude"]));
    assert!(
        loud.contains("network any"),
        "unrestricted network reach must be visible:\n{loud}"
    );
    assert!(
        loud.contains("curl") && loud.contains("./cache/**"),
        "exec and write asks must name what they are:\n{loud}"
    );
}

/// A runtime you do not have is a failure that would otherwise surface as a
/// denied tool call days later.
#[test]
fn try_reports_a_missing_runtime() {
    let s = Scratch::new("runtime");
    s.write(
        "src/needs/capability.yaml",
        "id: needs\nname: Needs\nruntime:\n  requires: [definitely-not-a-real-binary-xyzzy]\nrun:\n  command: definitely-not-a-real-binary-xyzzy\nevents:\n  - phase: pre\n    subject: tool\n    tool_class: any\n",
    );
    let text = out(&s.run(&["try", s.src().to_str().unwrap(), "--harness", "claude"]));
    assert!(
        text.contains("MISSING"),
        "an absent interpreter must be called out:\n{text}"
    );
}

/// Whether anyone stands behind it. Unsigned is a fine answer; not being told is
/// not.
#[test]
fn try_reports_the_signature_status() {
    let s = Scratch::new("sig");
    a_skill(&s);
    let text = out(&s.run(&["try", s.src().to_str().unwrap(), "--harness", "claude"]));
    assert!(
        text.contains("signature"),
        "the signature line is not optional:\n{text}"
    );
}

/// Targets default to the profile you already have — and say so. Previewing
/// against eleven harnesses when you use one buries the answer; narrowing
/// silently would be worse.
#[test]
fn try_takes_its_harnesses_from_the_project_profile() {
    let s = Scratch::new("profile");
    a_skill(&s);
    s.write(
        "proj/open-harness.yaml",
        "name: demo\nharnesses: [cursor]\nsources: []\n",
    );
    let text = out(&s.run(&["try", s.src().to_str().unwrap()]));
    assert!(
        text.contains("targets  cursor"),
        "the profile's harness list must drive the preview:\n{text}"
    );
    assert!(
        text.contains("open-harness.yaml"),
        "where the harness set came from must be stated:\n{text}"
    );
    assert!(
        !text.contains(".claude/skills"),
        "a profile that targets cursor must not preview claude:\n{text}"
    );
}

/// Without a profile it falls back to everything, and says that too.
#[test]
fn try_says_when_it_fell_back_to_every_harness() {
    let s = Scratch::new("fallback");
    a_skill(&s);
    let text = out(&s.run(&["try", s.src().to_str().unwrap()]));
    assert!(
        text.contains("no profile found"),
        "the fallback must be stated, not silent:\n{text}"
    );
}

/// The line that turns a preview into an install, printed with the spec exactly
/// as given so it can be pasted.
#[test]
fn try_prints_the_command_that_adopts_it() {
    let s = Scratch::new("adopt");
    a_skill(&s);
    let spec = s.src().to_str().unwrap().to_string();
    let text = out(&s.run(&["try", &spec, "--harness", "claude"]));
    assert!(
        text.contains("nothing was written"),
        "the preview must state that it wrote nothing:\n{text}"
    );
    assert!(
        text.contains(&format!("oh add {spec}")),
        "the adopt line must repeat the spec:\n{text}"
    );
}

/// `--global` previews the user scope: different paths, and some artifacts that
/// have no documented user-level home at all.
#[test]
fn try_can_preview_the_user_scope() {
    let s = Scratch::new("global");
    a_skill(&s);
    let text = out(&s.run(&[
        "try",
        s.src().to_str().unwrap(),
        "--harness",
        "claude",
        "--global",
    ]));
    assert!(
        text.contains("scope    user"),
        "the scope must be stated:\n{text}"
    );
    assert!(
        text.contains(".claude/skills/commit-style/SKILL.md"),
        "a user-scope skill lands under ~/.claude/skills:\n{text}"
    );
}

/// A dependency edge is reported, never fetched. Resolution stays flat whatever
/// the capability declares — cloning a graph is precisely the commitment `try`
/// exists to defer.
#[test]
fn try_reports_an_unmet_dependency_rather_than_acquiring_it() {
    let s = Scratch::new("deps");
    s.write(
        "src/needs-other/capability.yaml",
        "id: needs-other\nname: Needs Other\nkind: skill\nskill:\n  body: Depends on something absent.\ndependencies: [somebody/absent]\n",
    );
    let o = s.run(&["try", s.src().to_str().unwrap(), "--harness", "claude"]);
    let text = out(&o);
    // Success, not a hard error: an unmet edge is a fact about the capability,
    // and the preview's job is to report it. Asserting the exit code as well as
    // the text keeps this from passing on a *parse* error that merely happened
    // to mention the same name.
    assert!(o.status.success(), "{text}");
    assert!(
        text.contains("requires 'somebody/absent', which no source provides"),
        "the unmet edge must be named:\n{text}"
    );
    let at_home = tree(&s.home());
    assert!(
        at_home.iter().all(|p| p.starts_with(".open-harness/cache")),
        "nothing may be acquired for an unmet dependency: {at_home:?}"
    );
}

/// A spec is required, and a bad one fails here rather than half-way through a
/// fetch.
#[test]
fn try_refuses_a_missing_or_unpinnable_spec() {
    let s = Scratch::new("bad");
    let none = s.run(&["try"]);
    assert_eq!(none.status.code(), Some(2), "{}", out(&none));
    assert!(
        out(&none).contains("try requires a source"),
        "{}",
        out(&none)
    );

    // An unpinned remote archive cannot be verified, so it is refused at parse
    // time — the same rule `add` applies, since `try` is the step before it.
    let unpinned = s.run(&["try", "https://example.invalid/caps.tar"]);
    assert_eq!(unpinned.status.code(), Some(2), "{}", out(&unpinned));
    assert!(
        out(&unpinned).contains("sha256"),
        "the refusal must say how to pin it:\n{}",
        out(&unpinned)
    );
}

/// A source that resolves but carries nothing is not a success: you asked what
/// it would install and the answer is "nothing".
#[test]
fn try_exits_non_zero_on_an_empty_source() {
    let s = Scratch::new("empty");
    let o = s.run(&["try", s.src().to_str().unwrap(), "--harness", "claude"]);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(out(&o).contains("no capabilities"), "{}", out(&o));
}

/// A relative spec is the caller's, not the workdir's. `try` resolves in the
/// home directory so the project stays clean, which would otherwise silently
/// re-root `./caps` onto `$HOME/caps` — a path nobody named.
#[test]
fn try_resolves_a_relative_spec_against_the_caller() {
    let s = Scratch::new("relative");
    s.write(
        "proj/caps/commit-style/SKILL.md",
        "---\nname: Commit Style\ndescription: House commit conventions.\n---\nUse conventional commits.\n",
    );
    // A decoy at the same relative path under the home directory: if the spec
    // were resolved against `try`'s workdir, this is what would be previewed.
    s.write(
        "home/caps/decoy/capability.yaml",
        "id: decoy\nname: Decoy\nkind: skill\nskill:\n  body: wrong tree\n",
    );
    let text = out(&s.run(&["try", "./caps", "--harness", "claude"]));
    assert!(
        text.contains("commit-style"),
        "a relative spec must resolve against the caller's directory:\n{text}"
    );
    assert!(
        !text.contains("decoy"),
        "the home directory must not be searched for a relative spec:\n{text}"
    );
}
