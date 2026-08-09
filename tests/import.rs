//! `oh import` — reading a project's existing harness config back into
//! portable capabilities.
//!
//! An importer is the easiest place in this codebase to lose something
//! silently: it looks like it worked, and the rule that stopped applying is
//! only noticed months later. So most of what these tests pin is not "did it
//! import" but "did it *say* what it did" — the translation it performed, the
//! frontmatter it could not carry, the ambiguity it refused to guess at, and
//! the things it deliberately left behind.

use open_harness::adapters::Harness;
use open_harness::importer::{self, Status};
use open_harness::kind::KindId;
use std::path::{Path, PathBuf};

fn tmp(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("oh-import-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn put(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn oh(args: &[&str], cwd: &Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_oh"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run oh")
}

fn item<'a>(imp: &'a importer::Import, id: &str) -> &'a importer::Item {
    imp.items
        .iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("no item `{id}` in {:?}", ids(imp)))
}

fn ids(imp: &importer::Import) -> Vec<&str> {
    imp.items.iter().map(|i| i.id.as_str()).collect()
}

fn body_of(item: &importer::Item, file: &str) -> String {
    item.files
        .iter()
        .find(|f| f.path == file)
        .unwrap_or_else(|| panic!("`{}` has no {file}", item.id))
        .contents
        .clone()
}

// ---------------------------------------------------------------- skills ----

/// `SKILL.md` is already the shape open-harness normalizes to, so importing one
/// must not paraphrase it — the document is carried as it stands.
#[test]
fn a_skill_imports_as_itself() {
    let root = tmp("skill");
    let doc = "---\nname: commit-style\ndescription: How we write commits\nallowed-tools: Read, Grep\n---\nUse imperative mood.\n";
    put(&root, ".claude/skills/commit-style/SKILL.md", doc);

    let imp = importer::scan(&root, None).unwrap();
    let it = item(&imp, "commit-style");
    assert_eq!(it.kind, KindId::Skill);
    assert_eq!(it.status, Status::Portable);
    assert_eq!(
        body_of(it, "SKILL.md"),
        doc,
        "the document must be verbatim"
    );
}

/// The same skill under two harnesses' directories is one capability, not two
/// colliding ids — but only when the two are actually the same file.
#[test]
fn the_same_skill_under_two_harnesses_merges_once() {
    let root = tmp("merge");
    let doc = "---\nname: x\ndescription: d\n---\nbody\n";
    put(&root, ".claude/skills/x/SKILL.md", doc);
    put(&root, ".cursor/skills/x/SKILL.md", doc);

    let imp = importer::scan(&root, None).unwrap();
    assert_eq!(ids(&imp), vec!["x"], "it should merge into one capability");
    let it = item(&imp, "x");
    assert_eq!(it.harnesses, vec!["claude-code", "cursor"]);
    assert!(
        it.notes.iter().any(|n| n.contains(".cursor/skills/x")),
        "the other location should be recorded: {:?}",
        it.notes
    );
}

/// ...and when they are *not* the same file, picking one silently would drop
/// whichever version the author meant. The conflict is reported instead.
#[test]
fn two_different_skills_with_one_id_do_not_silently_merge() {
    let root = tmp("conflict");
    put(
        &root,
        ".claude/skills/x/SKILL.md",
        "---\nname: x\ndescription: claude version\n---\nA\n",
    );
    put(
        &root,
        ".cursor/skills/x/SKILL.md",
        "---\nname: x\ndescription: cursor version\n---\nB — quite different\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    assert_eq!(imp.items.len(), 2);
    let skipped: Vec<&importer::Item> = imp
        .items
        .iter()
        .filter(|i| matches!(i.status, Status::Skipped(_)))
        .collect();
    assert_eq!(skipped.len(), 1, "exactly one should be held back");
    let Status::Skipped(why) = &skipped[0].status else {
        unreachable!()
    };
    assert!(
        why.contains("content differs") && why.contains(".claude/skills/x/SKILL.md"),
        "the reason should name the conflicting file: {why}"
    );
    assert!(
        skipped[0].files.is_empty(),
        "a skipped item must not be written"
    );
}

/// The commonest real-world shape: a project that already fans one concept out
/// to several harnesses by hand. Each dialect renders different *metadata* —
/// Claude's `allowed-tools` are PascalCase, OpenCode's are not — so comparing
/// rendered files would report a pile of phantom conflicts. What matters is
/// whether the substance agrees.
#[test]
fn the_same_capability_in_two_dialects_merges_and_says_which_metadata_won() {
    let root = tmp("dialects");
    put(
        &root,
        ".claude/skills/x/SKILL.md",
        "---\nname: x\ndescription: d\nallowed-tools: Read, Grep\n---\nThe body.\n",
    );
    put(
        &root,
        ".opencode/skills/x/SKILL.md",
        "---\nname: x\ndescription: d\nallowed-tools: read, grep\n---\nThe body.\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    assert_eq!(
        ids(&imp),
        vec!["x"],
        "differing frontmatter is not a conflict"
    );
    let it = item(&imp, "x");
    assert_eq!(it.harnesses, vec!["claude-code", "opencode"]);
    let note = it
        .notes
        .iter()
        .find(|n| n.contains("same content"))
        .unwrap_or_else(|| panic!("{:?}", it.notes));
    assert!(
        note.contains("the metadata came from .claude/skills/x/SKILL.md"),
        "it must say whose metadata was kept: {note}"
    );
}

/// A repo with both `CLAUDE.md` and `AGENTS.md` saying the same thing is very
/// common. They have different ids, so ordinary dedup never sees them — but the
/// instructions kind writes one file per harness, so importing both would make
/// two capabilities fight over `CLAUDE.md`.
#[test]
fn two_identical_instruction_briefs_become_one_capability() {
    let root = tmp("two-briefs");
    put(&root, "CLAUDE.md", "# Brief\nRun cargo test.\n");
    put(&root, "AGENTS.md", "# Brief\nRun cargo test.\n");

    let imp = importer::scan(&root, None).unwrap();
    assert_eq!(imp.items.len(), 1, "{:?}", ids(&imp));
    let it = &imp.items[0];
    assert!(it.harnesses.contains(&"claude-code") && it.harnesses.contains(&"codex"));
    assert!(
        it.notes.iter().any(|n| n.contains("AGENTS.md")),
        "{:?}",
        it.notes
    );
}

/// ...and when the two briefs genuinely differ, merging would silently pick a
/// winner. Both are kept, and both are told only one can install.
#[test]
fn differing_instruction_briefs_are_kept_but_warned_about() {
    let root = tmp("two-diff-briefs");
    put(&root, "CLAUDE.md", "# Claude brief\nOne thing.\n");
    put(
        &root,
        "AGENTS.md",
        "# Agents brief\nSomething else entirely.\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    assert_eq!(imp.items.len(), 2);
    for it in &imp.items {
        assert!(
            it.notes.iter().any(|n| n.contains("only one can install")),
            "`{}` should warn about the clash: {:?}",
            it.id,
            it.notes
        );
    }
}

// -------------------------------------------------------------- commands ----

/// Every markdown harness writes `$ARGUMENTS` / `$N`; the portable body uses
/// `{{args}}` / `{{argN}}`. Importing without converting leaves a body that
/// renders a literal `$ARGUMENTS` onto Gemini and Pi, which read braces.
#[test]
fn a_command_translates_dollar_arguments_to_the_portable_spelling() {
    let root = tmp("cmd");
    put(
        &root,
        ".claude/commands/review.md",
        "---\ndescription: Review a PR\nargument-hint: <pr>\n---\nReview PR $1 for $ARGUMENTS now.\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    let it = item(&imp, "review");
    let doc = body_of(it, "COMMAND.md");
    assert!(
        doc.contains("Review PR {{arg1}} for {{args}} now."),
        "argument syntax not converted:\n{doc}"
    );
    assert!(!doc.contains('$'), "no `$` spelling should survive:\n{doc}");
    assert!(doc.contains("argument-hint: <pr>"), "the hint is portable");
    assert!(
        it.notes.iter().any(|n| n.contains("converted")),
        "the conversion must be reported: {:?}",
        it.notes
    );
}

/// Pi already writes the portable spelling, so there is nothing to convert —
/// and claiming a conversion happened would be as wrong as not doing one.
#[test]
fn a_pi_command_is_left_alone_because_it_already_speaks_braces() {
    let root = tmp("pi");
    put(
        &root,
        ".pi/prompts/greet.md",
        "---\ndescription: Greet\n---\nSay hello to {{args}}.\n",
    );
    let imp = importer::scan(&root, None).unwrap();
    let it = item(&imp, "greet");
    assert!(body_of(it, "COMMAND.md").contains("{{args}}"));
    assert!(
        !it.notes.iter().any(|n| n.contains("converted")),
        "nothing was converted: {:?}",
        it.notes
    );
}

// ----------------------------------------------------------------- rules ----

/// Cursor is the only source that can express all four activations, so it is
/// the one place every branch of the inverse mapping can be checked.
#[test]
fn all_four_cursor_activations_are_read_back() {
    let root = tmp("cursor-rules");
    put(
        &root,
        ".cursor/rules/a.mdc",
        "---\ndescription: d\nglobs:\nalwaysApply: true\n---\nalways\n",
    );
    put(
        &root,
        ".cursor/rules/g.mdc",
        "---\ndescription: d\nglobs: src/**/*.rs,tests/**\nalwaysApply: false\n---\nglob\n",
    );
    put(
        &root,
        ".cursor/rules/m.mdc",
        "---\ndescription: pick me when relevant\nglobs:\nalwaysApply: false\n---\nmodel\n",
    );
    put(
        &root,
        ".cursor/rules/x.mdc",
        "---\nglobs:\nalwaysApply: false\n---\nmanual\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    assert!(body_of(item(&imp, "a"), "RULE.md").contains("activation: always"));
    assert!(body_of(item(&imp, "m"), "RULE.md").contains("activation: agent_requested"));
    assert!(body_of(item(&imp, "x"), "RULE.md").contains("activation: manual"));

    let g = body_of(item(&imp, "g"), "RULE.md");
    assert!(g.contains("activation: glob"), "{g}");
    assert!(
        g.contains("src/**/*.rs") && g.contains("tests/**"),
        "the comma-separated globs must survive as a list:\n{g}"
    );
}

#[test]
fn windsurf_triggers_and_cline_paths_are_read_back() {
    let root = tmp("other-rules");
    put(
        &root,
        ".windsurf/rules/w.md",
        "---\ntrigger: model_decision\ndescription: d\n---\nw\n",
    );
    put(
        &root,
        ".clinerules/c.md",
        "---\npaths:\n  - \"src/auth/**\"\n---\nc\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    assert!(body_of(item(&imp, "w"), "RULE.md").contains("activation: agent_requested"));
    let c = body_of(item(&imp, "c"), "RULE.md");
    assert!(
        c.contains("activation: glob") && c.contains("src/auth/**"),
        "{c}"
    );
}

/// Copilot writes no `applyTo` for *both* manual and always-on. That is a real
/// ambiguity in the source format, and resolving it silently toward "always"
/// would widen where the rule applies. It takes the narrower reading and says so.
#[test]
fn an_ambiguous_copilot_rule_takes_the_narrow_reading_and_declares_it() {
    let root = tmp("copilot-rules");
    put(
        &root,
        ".github/instructions/amb.instructions.md",
        "This rule has no applyTo.\n",
    );
    put(
        &root,
        ".github/instructions/all.instructions.md",
        "---\napplyTo: \"**\"\n---\nEverywhere.\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    let amb = item(&imp, "amb");
    assert!(body_of(amb, "RULE.md").contains("activation: manual"));
    assert!(
        amb.notes
            .iter()
            .any(|n| n.contains("cannot distinguish manual from always")),
        "the ambiguity must be declared, not resolved in silence: {:?}",
        amb.notes
    );
    assert!(body_of(item(&imp, "all"), "RULE.md").contains("activation: always"));
}

/// `.clinerules/workflows/` holds commands, not rules. Descending into it would
/// import every workflow twice, once as the wrong kind.
#[test]
fn cline_workflows_are_commands_not_more_rules() {
    let root = tmp("cline-split");
    put(&root, ".clinerules/r.md", "a rule\n");
    put(&root, ".clinerules/workflows/w.md", "a workflow\n");

    let imp = importer::scan(&root, None).unwrap();
    assert_eq!(item(&imp, "r").kind, KindId::Rule);
    assert_eq!(item(&imp, "w").kind, KindId::Command);
}

// ---------------------------------------------------------------- agents ----

#[test]
fn an_agents_tool_list_comes_back_as_the_canonical_vocabulary() {
    let root = tmp("agent");
    put(
        &root,
        ".claude/agents/db.md",
        "---\nname: db\ndescription: DBA\nmodel: opus\ntools: Read, Grep, Bash(psql:*)\n---\nReview migrations.\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    let doc = body_of(item(&imp, "db"), "AGENT.md");
    assert!(
        doc.contains("read") && doc.contains("grep"),
        "Claude's PascalCase tokens should fold to canonical names:\n{doc}"
    );
    assert!(
        doc.contains("bash(psql:*)"),
        "a parenthesized spec must survive the mapping:\n{doc}"
    );
}

/// OpenCode gates an agent's tools with a permission map rather than a list.
/// Both have to reach the same normalized field or the agent re-emits wrong.
#[test]
fn an_opencode_permission_map_comes_back_as_canonical_verdicts() {
    let root = tmp("opencode-agent");
    put(
        &root,
        ".opencode/agents/rev.md",
        "---\ndescription: Reviewer\nmode: subagent\npermission:\n  edit: deny\n  bash: ask\n---\nReview.\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    let it = item(&imp, "rev");
    let doc = body_of(it, "AGENT.md");
    assert!(doc.contains("permissions:"), "{doc}");
    assert!(doc.contains("deny") && doc.contains("ask"), "{doc}");
    assert!(doc.contains("mode: subagent"), "{doc}");
    assert!(
        it.notes.iter().any(|n| n.contains("permission")),
        "{:?}",
        it.notes
    );
}

/// Frontmatter the normalized model has no home for is dropped — that is
/// unavoidable. Dropping it *quietly* is not: `disable-model-invocation`
/// changes how the agent behaves.
#[test]
fn frontmatter_that_cannot_travel_is_named_rather_than_silently_dropped() {
    let root = tmp("dropped");
    put(
        &root,
        ".claude/agents/a.md",
        "---\nname: a\ndescription: d\ndisable-model-invocation: true\nhandoffs: [b]\n---\nbody\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    let it = item(&imp, "a");
    let note = it
        .notes
        .iter()
        .find(|n| n.contains("not carried"))
        .unwrap_or_else(|| panic!("nothing reported as dropped: {:?}", it.notes));
    assert!(note.contains("disable-model-invocation"), "{note}");
    assert!(note.contains("handoffs"), "{note}");
    assert!(
        note.contains("overrides.claude-code"),
        "it should say where to put it back: {note}"
    );
}

// ---------------------------------------------------- instructions & MCP ----

/// `AGENTS.md` is the cross-tool standard: one file, six readers. The item
/// records all of them so the report does not imply it came from just one.
#[test]
fn a_shared_instructions_file_records_every_harness_that_reads_it() {
    let root = tmp("agents-md");
    put(&root, "AGENTS.md", "# Brief\nRun cargo test.\n");

    let imp = importer::scan(&root, None).unwrap();
    let it = item(&imp, "agents-instructions");
    assert_eq!(it.kind, KindId::Instructions);
    assert!(it.harnesses.contains(&"codex") && it.harnesses.contains(&"aider"));
    assert!(it.harnesses.len() >= 6, "{:?}", it.harnesses);
    assert!(body_of(it, "INSTRUCTIONS.md").contains("Run cargo test."));
}

/// The id must not stutter for a file already named `...instructions.md`.
#[test]
fn the_copilot_instruction_files_id_does_not_stutter() {
    let root = tmp("copilot-md");
    put(&root, ".github/copilot-instructions.md", "brief\n");
    let imp = importer::scan(&root, None).unwrap();
    assert_eq!(ids(&imp), vec!["copilot-instructions"]);
}

#[test]
fn an_empty_instructions_file_is_reported_not_imported_as_nothing() {
    let root = tmp("empty-md");
    put(&root, "CLAUDE.md", "   \n\n");
    let imp = importer::scan(&root, None).unwrap();
    assert!(matches!(imp.items[0].status, Status::Skipped(_)));
}

#[test]
fn an_mcp_server_imports_as_a_portable_tool() {
    let root = tmp("mcp");
    put(
        &root,
        ".mcp.json",
        r#"{"mcpServers":{"everything":{"command":"npx","args":["-y","srv"]}}}"#,
    );

    let imp = importer::scan(&root, None).unwrap();
    let it = item(&imp, "everything");
    assert_eq!(it.kind, KindId::Tool);
    let y = body_of(it, "capability.yaml");
    assert!(y.contains("provider: mcp") && y.contains("npx"), "{y}");
}

#[test]
fn a_malformed_mcp_file_is_reported_with_its_parse_error() {
    let root = tmp("bad-mcp");
    put(&root, ".mcp.json", "{ not json");
    let imp = importer::scan(&root, None).unwrap();
    let Status::Skipped(why) = &imp.items[0].status else {
        panic!("expected a skip, got {:?}", imp.items[0].status);
    };
    assert!(why.contains("not valid JSON"), "{why}");
}

// -------------------------------------------- claude settings: two halves ----

/// `.claude/settings.json` holds one thing that travels and one that cannot,
/// and they must not be conflated: permissions are portable, hooks are not.
#[test]
fn claude_settings_splits_into_portable_permissions_and_native_hooks() {
    let root = tmp("settings");
    put(
        &root,
        ".claude/settings.json",
        r#"{
          "permissions": {"deny":["Bash(rm -rf:*)"],"allow":["Read"],"defaultMode":"acceptEdits"},
          "hooks": {"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"./g.sh"}]}]}
        }"#,
    );

    let imp = importer::scan(&root, None).unwrap();

    let perms = item(&imp, "claude-permissions");
    assert_eq!(perms.status, Status::Portable);
    let y = body_of(perms, "capability.yaml");
    assert!(y.contains("verdict: deny") && y.contains("rm -rf:*"), "{y}");
    assert!(
        y.contains("tool: bash"),
        "the tool half should be canonical:\n{y}"
    );
    assert!(
        perms.notes.iter().any(|n| n.contains("defaultMode")),
        "a setting with no portable equivalent must be named: {:?}",
        perms.notes
    );

    let hooks = item(&imp, "claude-hooks");
    assert_eq!(hooks.kind, KindId::Native);
    let Status::Native(why) = &hooks.status else {
        panic!("hooks must be carried as native, got {:?}", hooks.status);
    };
    assert!(why.contains("PreToolUse"), "{why}");
    let hy = body_of(hooks, "capability.yaml");
    assert!(
        hy.contains("harness: claude-code") && hy.contains("hook@1"),
        "the native capability must pin the harness and say why it cannot travel:\n{hy}"
    );
    assert!(
        hooks
            .notes
            .iter()
            .any(|n| n.contains("oh scaffold --kind hook")),
        "it should point at the portable alternative: {:?}",
        hooks.notes
    );
}

/// Carried-as-native is only honest if it really is refused elsewhere. This
/// checks the end state rather than the promise.
#[test]
fn imported_hooks_are_actually_unsupported_on_every_other_harness() {
    let root = tmp("hooks-blocked");
    put(
        &root,
        ".claude/settings.json",
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"./g.sh"}]}]}}"#,
    );
    let imp = importer::scan(&root, None).unwrap();
    let out = root.join("capabilities");
    importer::write(&out, &imp, false).unwrap();

    let caps = open_harness::manifest::discover(&out).expect("the import must load back");
    let cap = caps
        .iter()
        .find(|c| c.manifest.id == "claude-hooks")
        .unwrap();
    for h in open_harness::adapters::ALL {
        let plan = open_harness::kind::kind_impl(cap.manifest.kind).plan(cap, h);
        let blocked = matches!(
            plan.installability,
            open_harness::kind::Installability::Unsupported(_)
        );
        assert_eq!(
            blocked,
            h != Harness::Claude,
            "{} should{} be blocked",
            h.id(),
            if h == Harness::Claude { "" } else { " not" }
        );
    }
}

// -------------------------------------------------- deliberate omissions ----

/// Gemini's commands are TOML, which this project does not read. Half-parsing
/// them with a regex or omitting them without a word are both worse than saying
/// so.
#[test]
fn a_gemini_toml_command_is_reported_as_skipped_with_the_reason() {
    let root = tmp("gemini");
    put(
        &root,
        ".gemini/commands/sum.toml",
        "description = \"S\"\nprompt = \"Summarize {{args}}\"\n",
    );
    let imp = importer::scan(&root, None).unwrap();
    let Status::Skipped(why) = &item(&imp, "sum").status else {
        panic!("expected a skip");
    };
    assert!(why.contains("TOML"), "{why}");
    assert!(
        why.contains("oh scaffold --kind command"),
        "it should say what to do instead: {why}"
    );
}

#[test]
fn unparseable_frontmatter_is_a_reported_skip_not_a_crash() {
    let root = tmp("bad-fm");
    put(
        &root,
        ".cursor/rules/broken.mdc",
        "---\n\tdescription: [unclosed\n---\nbody\n",
    );
    let imp = importer::scan(&root, None).unwrap();
    let it = item(&imp, "broken");
    assert!(
        matches!(it.status, Status::Skipped(_)),
        "got {:?}",
        it.status
    );
}

// ----------------------------------------------------------- the CLI end ----

#[test]
fn dry_run_writes_nothing() {
    let root = tmp("dry");
    put(&root, "AGENTS.md", "brief\n");
    let imp = importer::scan(&root, None).unwrap();
    let out = root.join("capabilities");
    let report = importer::write(&out, &imp, true).unwrap();
    assert_eq!(report.created.len(), 1, "it should still report the plan");
    assert!(!out.exists(), "dry-run must not create {}", out.display());
}

/// An importer that clobbers the capability you already hand-edited is worse
/// than one that refuses.
#[test]
fn an_existing_capability_is_never_overwritten() {
    let root = tmp("collide");
    put(&root, "AGENTS.md", "new brief\n");
    let out = root.join("capabilities");
    put(
        &out,
        "agents-instructions/INSTRUCTIONS.md",
        "MINE — hand written\n",
    );

    let imp = importer::scan(&root, None).unwrap();
    let report = importer::write(&out, &imp, false).unwrap();
    assert_eq!(report.collisions, vec!["agents-instructions"]);
    assert!(report.created.is_empty());
    assert_eq!(
        std::fs::read_to_string(out.join("agents-instructions/INSTRUCTIONS.md")).unwrap(),
        "MINE — hand written\n"
    );
}

#[test]
fn narrowing_to_one_harness_ignores_the_others() {
    let root = tmp("narrow");
    put(&root, ".claude/skills/c/SKILL.md", "---\nname: c\n---\nc\n");
    put(
        &root,
        ".cursor/rules/r.mdc",
        "---\nalwaysApply: true\n---\nr\n",
    );

    let only_cursor = importer::scan(&root, Some(Harness::Cursor)).unwrap();
    assert_eq!(ids(&only_cursor), vec!["r"]);
    let only_claude = importer::scan(&root, Some(Harness::Claude)).unwrap();
    assert_eq!(ids(&only_claude), vec!["c"]);
}

/// "Found nothing" has to name where it looked, or it reads as "this tool is
/// broken" rather than "your config is somewhere else".
#[test]
fn an_empty_project_says_what_it_searched_for() {
    let root = tmp("empty");
    let imp = importer::scan(&root, None).unwrap();
    assert!(imp.items.is_empty());
    let note = imp
        .notes
        .first()
        .expect("a note explaining the empty result");
    assert!(
        note.contains(".claude/skills") && note.contains("AGENTS.md"),
        "{note}"
    );
}

/// A skip means the project has something open-harness now does not. Exiting 0
/// would report a partial import as a complete one.
#[test]
fn the_cli_exits_non_zero_when_something_was_left_behind() {
    let root = tmp("cli-exit");
    put(&root, "AGENTS.md", "brief\n");
    put(&root, ".gemini/commands/s.toml", "prompt = \"x\"\n");

    let out = oh(&["import", "--dry-run"], &root);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a partial import must not look complete"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("SKIPPED"), "{text}");

    // ...and zero when everything was taken.
    let clean = tmp("cli-clean");
    put(&clean, "AGENTS.md", "brief\n");
    let out = oh(&["import", "--dry-run"], &clean);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The whole point: what comes out has to go back in. Import a project's config
/// and every capability must load, and land on the harnesses it should.
#[test]
fn imported_capabilities_load_and_install_across_harnesses() {
    let root = tmp("roundtrip");
    put(
        &root,
        ".cursor/rules/rpc.mdc",
        "---\ndescription: RPC\nglobs: src/rpc/**/*.ts\nalwaysApply: false\n---\nVersion your methods.\n",
    );
    put(
        &root,
        ".claude/commands/review.md",
        "---\ndescription: Review\n---\nReview $ARGUMENTS.\n",
    );
    put(&root, "AGENTS.md", "# Brief\n");

    let imp = importer::scan(&root, None).unwrap();
    let out = root.join("capabilities");
    importer::write(&out, &imp, false).unwrap();

    let caps = open_harness::manifest::discover(&out).expect("every import must load back");
    assert_eq!(
        caps.len(),
        3,
        "{:?}",
        caps.iter().map(|c| &c.manifest.id).collect::<Vec<_>>()
    );
    for cap in &caps {
        assert!(
            !cap.manifest.name.is_empty(),
            "`{}` imported without a name",
            cap.manifest.id
        );
    }

    // A Cursor rule must reach the harnesses that can host rules at all.
    let rule = caps.iter().find(|c| c.manifest.id == "rpc").unwrap();
    let k = open_harness::kind::kind_impl(rule.manifest.kind);
    for h in [Harness::Windsurf, Harness::Copilot, Harness::Cline] {
        let plan = k.plan(rule, h);
        assert!(
            !matches!(
                plan.installability,
                open_harness::kind::Installability::Unsupported(_)
            ),
            "the imported rule should install on {}",
            h.id()
        );
        let has_glob = plan.artifacts.iter().any(|a| match a {
            open_harness::kind::Artifact::File { contents, .. } => contents.contains("src/rpc/**"),
            _ => false,
        });
        assert!(has_glob, "the glob was lost on the way to {}", h.id());
    }
}

/// End to end through the binary, since that is how anyone will meet it.
#[test]
fn the_cli_imports_and_then_syncs_what_it_imported() {
    let root = tmp("e2e");
    put(
        &root,
        ".claude/commands/review.md",
        "---\ndescription: Review\n---\nReview PR $1.\n",
    );

    let out = oh(&["import"], &root);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(root.join("capabilities/review/COMMAND.md").is_file());

    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let out = oh(
        &[
            "sync",
            "--into",
            "project",
            "--capabilities",
            "capabilities",
        ],
        &root,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Gemini reads braces; the `$1` we imported must have become `{{arg1}}`.
    let gemini = std::fs::read_to_string(project.join(".gemini/commands/review.toml"))
        .expect("the command should reach Gemini");
    assert!(gemini.contains("{{arg1}}"), "{gemini}");
    // ...and Claude gets its own spelling back.
    let claude = std::fs::read_to_string(project.join(".claude/commands/review.md")).unwrap();
    assert!(claude.contains("$1"), "{claude}");
}
