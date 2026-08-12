//! Project scope versus user scope.
//!
//! Most capabilities you want are not per-project — a commit-message skill or a
//! secret guard belongs everywhere. `oh sync --global` writes the user-level
//! locations each harness already reads.
//!
//! The thing most worth pinning here is what it *refuses* to do. A user-scope
//! install writes into someone's home directory, so a plausible-but-wrong path
//! is not a cosmetic bug: it is a file nothing will ever read, sitting
//! permanently in `~`. Every path in the table came from vendor documentation
//! (see `docs/src/scopes.md`); everything else defers.

use open_harness::adapters::Harness;
use open_harness::manifest::discover;
use open_harness::sync::{self, DeferReason, Scope};
use std::path::{Path, PathBuf};

fn tmp(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-scope-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn corpus() -> Vec<open_harness::manifest::LoadedCapability> {
    discover(&Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities")).unwrap()
}

fn paths(scope: Scope, harnesses: &[Harness]) -> Vec<String> {
    sync::plan_scoped(&corpus(), harnesses, scope)
        .files
        .into_iter()
        .map(|f| f.path)
        .collect()
}

fn oh(args: &[&str], cwd: &Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run oh")
}

/// The documented user-level homes, per harness. These are the claims that
/// would put a useless file in someone's `~` if they were wrong.
#[test]
fn artifacts_land_in_each_harnesss_documented_user_location() {
    let p = paths(Scope::User, &[Harness::Claude]);
    for expected in [
        ".claude/skills/commit-style/SKILL.md",
        ".claude/agents/database-engineer.md",
        ".claude/commands/review-pr.md",
        ".claude/settings.json",
        // NOT `~/CLAUDE.md` — the user-level brief lives inside `.claude/`.
        ".claude/CLAUDE.md",
    ] {
        assert!(
            p.contains(&expected.to_string()),
            "missing {expected}: {p:?}"
        );
    }
    assert!(
        !p.contains(&"CLAUDE.md".to_string()),
        "the project-root filename must not be reused at user scope: {p:?}"
    );

    assert!(paths(Scope::User, &[Harness::Cursor])
        .contains(&".cursor/rules/rpc-conventions.mdc".to_string()));

    // OpenCode's user config is XDG, not `~/.opencode/`.
    let p = paths(Scope::User, &[Harness::OpenCode]);
    for expected in [
        ".config/opencode/AGENTS.md",
        ".config/opencode/agents/database-engineer.md",
        ".config/opencode/commands/review-pr.md",
        ".config/opencode/skills/commit-style/SKILL.md",
    ] {
        assert!(
            p.contains(&expected.to_string()),
            "missing {expected}: {p:?}"
        );
    }
    assert!(
        !p.iter().any(|x| x.starts_with(".opencode/")),
        "the project directory name must not leak into user scope: {p:?}"
    );

    assert!(paths(Scope::User, &[Harness::Gemini]).contains(&".gemini/GEMINI.md".to_string()));
    assert!(paths(Scope::User, &[Harness::Codex]).contains(&".codex/AGENTS.md".to_string()));
}

/// `AGENTS.md` is a single shared file in a project — six harnesses read it, and
/// the sync layer converges them onto one path. At user scope they diverge into
/// different files, so the remap has to run per harness, before grouping.
#[test]
fn a_shared_project_file_splits_per_harness_at_user_scope() {
    let both = &[Harness::Codex, Harness::OpenCode];

    let project = paths(Scope::Project, both);
    assert_eq!(
        project.iter().filter(|p| p.ends_with("AGENTS.md")).count(),
        1,
        "in a project it is one shared file: {project:?}"
    );

    let user = paths(Scope::User, both);
    assert!(user.contains(&".codex/AGENTS.md".to_string()), "{user:?}");
    assert!(
        user.contains(&".config/opencode/AGENTS.md".to_string()),
        "{user:?}"
    );
}

/// The four targets the kinds already knew were user-level — Codex prompts and
/// three MCP configs — were unwritable in either scope before this existed.
#[test]
fn a_kinds_own_user_level_target_becomes_writable() {
    let project = sync::plan_scoped(&corpus(), &[Harness::Codex], Scope::Project);
    assert!(
        project
            .deferred
            .iter()
            .any(|d| d.reason == DeferReason::GlobalPath && d.detail.contains("~/.codex/prompts/")),
        "in a project it is still outside the tree"
    );
    assert!(
        paths(Scope::User, &[Harness::Codex]).contains(&".codex/prompts/review-pr.md".to_string()),
        "at user scope it is finally writable"
    );
}

/// Where a harness documents no user-level home, the artifact is reported —
/// never written to a guessed path. Windsurf is the sharpest case: its global
/// rules are one 6,000-character file, so N rules genuinely cannot be N files.
#[test]
fn an_undocumented_user_location_defers_instead_of_guessing() {
    for h in [Harness::Windsurf, Harness::Cline, Harness::Copilot] {
        let plan = sync::plan_scoped(&corpus(), &[h], Scope::User);
        assert!(
            plan.deferred
                .iter()
                .any(|d| d.reason == DeferReason::NoUserLocation),
            "{} should defer rather than guess",
            h.id()
        );
        assert!(
            !plan.files.iter().any(|f| f.path.contains("rules")),
            "{} wrote a rules file to a guessed user path: {:?}",
            h.id(),
            plan.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }
}

/// open-harness's own directory is not a harness's config, so it needs no table
/// entry and lands identically in both scopes — beside the scope's lockfile.
#[test]
fn open_harness_own_directory_is_scope_independent() {
    let user = paths(Scope::User, &[Harness::Claude, Harness::Cursor]);
    assert!(
        user.iter().any(|p| p.starts_with(".open-harness/")),
        "our own managed files should place at user scope too: {user:?}"
    );
}

/// Project scope must be exactly what it was — this feature adds a second
/// scope, it does not change the first.
#[test]
fn project_scope_is_unchanged() {
    let default = sync::plan_sync(&corpus(), &open_harness::adapters::ALL);
    let explicit = sync::plan_scoped(&corpus(), &open_harness::adapters::ALL, Scope::Project);
    assert_eq!(default.files, explicit.files);
    assert_eq!(default.deferred.len(), explicit.deferred.len());
    assert!(
        default.files.iter().any(|f| f.path == "CLAUDE.md"),
        "a project still gets the repo-root brief"
    );
}

/// The two scopes keep separate lockfiles, which is what makes them independent:
/// uninstalling one cannot reach into the other.
#[test]
fn the_scopes_have_independent_lockfiles_and_uninstalls() {
    let root = tmp("independent");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let caps = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("capabilities")
        .to_string_lossy()
        .into_owned();

    let out = oh(
        &[
            "sync",
            "--global",
            "--into",
            "home",
            "--capabilities",
            &caps,
        ],
        &root,
    );
    assert!(out.status.success() || out.status.code() == Some(1));
    let out = oh(
        &["sync", "--into", "project", "--capabilities", &caps],
        &root,
    );
    assert!(out.status.success() || out.status.code() == Some(1));

    assert!(home.join(".claude/skills/commit-style/SKILL.md").is_file());
    assert!(project
        .join(".claude/skills/commit-style/SKILL.md")
        .is_file());
    assert!(
        home.join(sync::LOCK_REL).is_file(),
        "user scope has its own lock"
    );
    assert!(project.join(sync::LOCK_REL).is_file());

    // Removing the user scope must leave the project alone.
    let out = oh(
        &["sync", "--global", "--into", "home", "--uninstall"],
        &root,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!home.join(".claude/skills/commit-style/SKILL.md").exists());
    assert!(
        project
            .join(".claude/skills/commit-style/SKILL.md")
            .is_file(),
        "a user-scope uninstall must not reach into a project"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A user-scope sync is idempotent, like a project one.
#[test]
fn a_user_scope_sync_converges() {
    let root = tmp("converge");
    std::fs::create_dir_all(root.join("home")).unwrap();
    let caps = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("capabilities")
        .to_string_lossy()
        .into_owned();
    let args = [
        "sync",
        "--global",
        "--into",
        "home",
        "--capabilities",
        &caps,
    ];

    let first = String::from_utf8_lossy(&oh(&args, &root).stdout).to_string();
    assert!(first.contains("created"), "{first}");
    let second = String::from_utf8_lossy(&oh(&args, &root).stdout).to_string();
    assert!(
        second.contains("0 created, 0 updated"),
        "a re-sync should converge, not duplicate:\n{second}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
