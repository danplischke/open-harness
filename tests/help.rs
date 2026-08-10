//! `oh`'s help surface.
//!
//! Help used to be one block listing every flag the CLI has, printed on stderr
//! with exit 2, and `--help` itself was rejected as an unknown option. These
//! pin the replacement — and, more importantly, pin it as *generated*: the
//! drift gates below fail if a command is added without help, if help
//! documents a command that does not exist, or if it documents a flag the
//! parser has never heard of.

use open_harness::help;
use std::path::Path;

fn oh(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(args)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("run oh")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The command names `main` actually dispatches, read out of the source. This
/// is the gate's whole point: the list cannot be maintained by hand next to the
/// thing it is supposed to check.
/// `src/main.rs` with line endings normalized.
///
/// These gates match on multi-line source patterns, and git checks the repo out
/// with CRLF on Windows — so an anchor like `"\n    o\n}"` finds nothing there
/// and the gate panics instead of checking anything. Normalizing once is the
/// difference between a gate that runs everywhere and one that is dead weight
/// on a third of CI.
fn main_rs() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
        .expect("read main.rs")
        .replace("\r\n", "\n")
}

fn dispatched_commands() -> Vec<String> {
    let src = main_rs();
    let body = src
        .split_once("    match cmd {")
        .expect("main's dispatch match")
        .1;
    let body = body.split_once("\n    }").expect("end of the match").0;

    let mut names = Vec::new();
    // `help`/`--help`/`-h` are handled before the match (they must beat the flag
    // parser), so the `matches!` guard is part of the dispatch surface too.
    if let Some((_, tail)) = src.split_once("if matches!(cmd, ") {
        for part in tail
            .split_once(')')
            .map(|(p, _)| p)
            .unwrap_or("")
            .split('|')
        {
            if let Some(name) = part
                .trim()
                .strip_prefix('"')
                .and_then(|p| p.strip_suffix('"'))
            {
                names.push(name.to_string());
            }
        }
    }
    for line in body.lines() {
        let line = line.trim();
        let Some((pattern, _)) = line.split_once("=>") else {
            continue;
        };
        // `"a" | "b" => cmd_x(...)` — every quoted alternative is a command.
        for part in pattern.split('|') {
            let part = part.trim();
            if let Some(name) = part.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
                names.push(name.to_string());
            }
        }
    }
    assert!(
        names.len() > 20,
        "the dispatch scan found only {names:?} — the parse is wrong, not the CLI"
    );
    names
}

#[test]
fn every_dispatched_command_has_help() {
    let missing: Vec<String> = dispatched_commands()
        .into_iter()
        .filter(|c| help::command(c).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "these commands are dispatched but undocumented: {missing:?} — add them to help::COMMANDS"
    );
}

#[test]
fn every_documented_command_is_dispatched() {
    let dispatched = dispatched_commands();
    let phantom: Vec<&str> = help::COMMANDS
        .iter()
        .map(|c| c.name)
        .filter(|n| !dispatched.iter().any(|d| d == n))
        .collect();
    assert!(
        phantom.is_empty(),
        "help documents commands that do not exist: {phantom:?}"
    );
}

/// A documented flag the parser rejects is worse than no documentation: the
/// help tells you to type something that exits 2.
#[test]
fn every_documented_flag_is_one_the_parser_accepts() {
    let src = main_rs();
    let parser = src
        .split_once("fn parse_opts")
        .expect("parse_opts")
        .1
        .split_once("\n    o\n}")
        .expect("end of parse_opts")
        .0;

    let mut unknown = Vec::new();
    for c in help::COMMANDS {
        for f in c.flags {
            let arm = format!("\"{}\"", f.name());
            if !parser.contains(&arm) {
                unknown.push(format!("{} {}", c.name, f.name()));
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "help documents flags parse_opts does not accept: {unknown:?}"
    );
}

#[test]
fn every_group_in_the_overview_holds_at_least_one_command() {
    let text = help::overview();
    for g in help::GROUPS {
        assert!(
            help::COMMANDS.iter().any(|c| c.group == *g),
            "group `{g}` is declared but empty"
        );
        assert!(text.contains(g), "the overview omits group `{g}`");
    }
    // And no command hides in a group the overview never prints.
    for c in help::COMMANDS {
        assert!(
            help::GROUPS.contains(&c.group),
            "`{}` is in unknown group `{}`",
            c.name,
            c.group
        );
    }
}

/// Asking for help is not a usage error. It goes to stdout and exits 0, so
/// `oh sync --help | less` works and a script does not see a failure.
#[test]
fn asking_for_help_succeeds_on_stdout() {
    for args in [
        vec!["help"],
        vec!["--help"],
        vec!["-h"],
        vec![],
        vec!["help", "sync"],
        vec!["sync", "--help"],
        vec!["mcp", "-h"],
    ] {
        let out = oh(&args);
        assert!(out.status.success(), "`oh {args:?}` should exit 0");
        assert!(
            !out.stdout.is_empty(),
            "`oh {args:?}` should print help on stdout"
        );
        assert!(
            out.stderr.is_empty(),
            "`oh {args:?}` should print nothing on stderr, got {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `oh <cmd> --help` must beat the flag parser, which would otherwise reject
/// `--help` as an unknown option — the behaviour before this existed.
#[test]
fn every_command_answers_its_own_help_flag() {
    for c in help::COMMANDS {
        let out = oh(&[c.name, "--help"]);
        assert!(out.status.success(), "`oh {} --help` failed", c.name);
        let text = stdout(&out);
        assert!(
            text.starts_with(&format!("oh {} — ", c.name)),
            "`oh {} --help` printed the wrong page:\n{text}",
            c.name
        );
        assert!(
            text.contains("usage:"),
            "`oh {} --help` has no usage line",
            c.name
        );
        for f in c.flags {
            assert!(
                text.contains(f.spec),
                "`oh {} --help` omits `{}`",
                c.name,
                f.spec
            );
        }
    }
}

/// Per-command help must show that command's flags and *not* the whole flag
/// namespace — the specific failure of the old single-block help.
#[test]
fn per_command_help_is_scoped_to_that_command() {
    let text = stdout(&oh(&["keygen", "--help"]));
    assert!(text.contains("--label"), "keygen documents --label");
    for unrelated in ["--into", "--uninstall", "--mcp-arg", "--require-signed"] {
        assert!(
            !text.contains(unrelated),
            "`oh keygen --help` should not mention `{unrelated}`:\n{text}"
        );
    }
}

#[test]
fn a_mistyped_command_suggests_the_real_one() {
    // Transpositions included: `snyc` is the common way to mistype `sync`, and
    // plain Levenshtein scores it 2, which is too far to suggest.
    for (typo, meant) in [
        ("snyc", "sync"),
        ("mtrix", "matrix"),
        ("instal", "install"),
        ("resolv", "resolve"),
        ("verfy", "verify"),
    ] {
        let out = oh(&[typo]);
        assert_eq!(out.status.code(), Some(2), "`oh {typo}` should exit 2");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains(&format!("did you mean `oh {meant}`")),
            "`oh {typo}` should suggest `{meant}`, said:\n{err}"
        );
    }
}

#[test]
fn a_command_nothing_resembles_gets_no_wild_guess() {
    let out = oh(&["xyzzy"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown command `xyzzy`"));
    assert!(
        !err.contains("did you mean"),
        "an unrelated word should not be matched to a command: {err}"
    );
    assert!(err.contains("oh help"), "it should point at the overview");
}

#[test]
fn help_for_a_command_that_does_not_exist_is_a_usage_error() {
    let out = oh(&["help", "nonsense"]);
    assert_eq!(out.status.code(), Some(2));
}

/// Completions are generated from the same table as the help, so they cannot
/// drift from the CLI — that is the reason they are generated at all.
#[test]
fn completion_scripts_cover_every_command() {
    for shell in ["bash", "zsh", "fish"] {
        let out = oh(&["completions", shell]);
        assert!(out.status.success(), "`oh completions {shell}` failed");
        let script = stdout(&out);
        for c in help::COMMANDS {
            assert!(
                script.contains(c.name),
                "the {shell} script omits `{}`",
                c.name
            );
        }
    }
}

#[test]
fn an_unknown_shell_is_refused_rather_than_guessed() {
    let out = oh(&["completions", "tcsh"]);
    assert_eq!(out.status.code(), Some(2));
    let out = oh(&["completions"]);
    assert_eq!(out.status.code(), Some(2));
}

/// Syntax-check a file with `bash -n`. `None` when this machine's `bash` could
/// not be used at all, as distinct from `Some(false)` for "bash read it and
/// said no".
fn bash_accepts(path: &std::path::Path) -> Option<(bool, String)> {
    let out = std::process::Command::new("bash")
        .arg("-n")
        .arg(path)
        .output()
        .ok()?;
    Some((
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

/// A generated script that a shell cannot parse is worse than none.
///
/// The verdict is only trusted after a known-good script gets a pass from the
/// same binary. On a Windows runner `bash` resolves to something that spawns
/// happily and then fails on everything — the WSL stub, or a Git Bash that
/// cannot open a `D:\...` path — so a bare "it spawned" check reads that as
/// "your script is broken". Probing first tells the two apart, and keeps the
/// real check running wherever bash actually works.
#[test]
fn the_bash_completion_script_parses() {
    let dir = std::env::temp_dir();
    let probe = dir.join(format!("oh-bash-probe-{}.bash", std::process::id()));
    std::fs::write(&probe, "echo hello\n").unwrap();
    let usable = bash_accepts(&probe);
    let _ = std::fs::remove_file(&probe);
    match usable {
        Some((true, _)) => {}
        _ => {
            eprintln!("no usable bash on this machine; skipped the syntax check");
            return;
        }
    }

    let script = stdout(&oh(&["completions", "bash"]));
    let path = dir.join(format!("oh-completions-{}.bash", std::process::id()));
    std::fs::write(&path, &script).unwrap();
    let check = bash_accepts(&path);
    let _ = std::fs::remove_file(&path);
    let (ok, stderr) = check.expect("bash worked for the probe a moment ago");
    assert!(ok, "bash rejected the generated script: {stderr}");
}
