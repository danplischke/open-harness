//! `oh import` — a project's existing harness config → portable capabilities.
//!
//! Every other path into this tool assumes you are starting from nothing: write
//! a capability, compose a profile, sync it out. Real projects are not empty.
//! They already have `.claude/skills/`, `.cursor/rules/`, an `AGENTS.md`, a
//! `.mcp.json` — and asking someone to re-author all of it before they can try
//! open-harness is the difference between a tool a team can adopt on a Tuesday
//! and one they file under "interesting".
//!
//! This module is the inverse of `src/kinds/`: where each `Kind` renders one
//! normalized capability into eleven native dialects, [`scan`] reads those
//! dialects back into the normalized form.
//!
//! ## What honesty means here
//!
//! An importer is where silent loss is easiest and most damaging — it *looks*
//! like it worked, and you only find out later that a rule stopped applying.
//! So every file found under a known native path ends up in the report as
//! exactly one of:
//!
//! * **portable** — normalized into a capability, with any activation or
//!   argument-syntax translation done explicitly;
//! * **native** — carried verbatim and pinned to the one harness that can host
//!   it ([`crate::kinds::native`]). Hooks are the case that matters: a
//!   `.claude/settings.json` hook is opaque shell against Claude's own schema,
//!   not a `hook@1` capability, and reshaping it into one would be a lie;
//! * **skipped** — with the reason. A Gemini TOML command says so rather than
//!   quietly not appearing.
//!
//! Nothing is ever overwritten: [`write`] refuses a capability id that already
//! exists and reports the collision.

use crate::adapters::Harness;
use crate::kind::KindId;
use crate::kinds::rule::Activation;
use crate::{config, tools, yaml};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What became of one thing found in the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Normalized into a portable capability.
    Portable,
    /// Carried verbatim, pinned to one harness. Holds why it cannot travel.
    Native(String),
    /// Found and deliberately not imported. Holds why.
    Skipped(String),
}

/// A file to write inside the capability's directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutFile {
    pub path: String,
    pub contents: String,
}

/// One capability's worth of import.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub kind: KindId,
    /// Project-relative path this came from.
    pub from: String,
    /// Every harness that reads `from`. More than one when harnesses share a
    /// path (`AGENTS.md`, `.agents/skills/`).
    pub harnesses: Vec<&'static str>,
    pub status: Status,
    /// Empty when the status is `Skipped`.
    pub files: Vec<OutFile>,
    /// Anything the translation had to decide or drop, surfaced per item.
    pub notes: Vec<String>,
    /// The substance, with dialect metadata stripped — the document body for the
    /// single-file kinds, the whole manifest otherwise. [`dedupe`] compares
    /// this rather than the rendered files, because the *same* capability read
    /// out of two harnesses legitimately produces different frontmatter.
    pub substance: String,
}

impl Item {
    fn skipped(
        id: impl Into<String>,
        kind: KindId,
        from: impl Into<String>,
        h: &'static str,
        why: impl Into<String>,
    ) -> Item {
        Item {
            id: id.into(),
            kind,
            from: from.into(),
            harnesses: vec![h],
            status: Status::Skipped(why.into()),
            files: Vec::new(),
            notes: Vec::new(),
            substance: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Import {
    pub items: Vec<Item>,
    /// Findings about the scan itself, not about one capability.
    pub notes: Vec<String>,
}

impl Import {
    pub fn portable(&self) -> impl Iterator<Item = &Item> {
        self.items
            .iter()
            .filter(|i| matches!(i.status, Status::Portable))
    }
    pub fn writable(&self) -> impl Iterator<Item = &Item> {
        self.items
            .iter()
            .filter(|i| !matches!(i.status, Status::Skipped(_)))
    }
}

// ---------------------------------------------------------------------------
// The native path tables — the inverse of each kind's target map.
// ---------------------------------------------------------------------------

/// `(directory, harness)` for skills: one sub-directory per skill, holding a
/// `SKILL.md`. Mirrors `kinds::skill::skill_dir`.
const SKILL_DIRS: &[(&str, Harness)] = &[
    (".claude/skills", Harness::Claude),
    // Codex and Antigravity share this one; recorded against Codex, with the
    // shared reader noted when the item is built.
    (".agents/skills", Harness::Codex),
    (".cursor/skills", Harness::Cursor),
    (".windsurf/skills", Harness::Windsurf),
    (".github/skills", Harness::Copilot),
    (".opencode/skills", Harness::OpenCode),
];

/// `(directory, file suffix, harness)` for commands. Mirrors `kinds::command`.
const COMMAND_DIRS: &[(&str, &str, Harness)] = &[
    (".claude/commands", ".md", Harness::Claude),
    (".opencode/commands", ".md", Harness::OpenCode),
    (".cursor/commands", ".md", Harness::Cursor),
    (".github/prompts", ".prompt.md", Harness::Copilot),
    (".windsurf/workflows", ".md", Harness::Windsurf),
    (".clinerules/workflows", ".md", Harness::Cline),
    (".pi/prompts", ".md", Harness::Pi),
];

/// `(directory, file suffix, harness)` for subagents. Mirrors `kinds::agent`.
const AGENT_DIRS: &[(&str, &str, Harness)] = &[
    (".claude/agents", ".md", Harness::Claude),
    (".opencode/agents", ".md", Harness::OpenCode),
    (".github/agents", ".agent.md", Harness::Copilot),
];

/// `(directory, file suffix, harness)` for rules. Mirrors `kinds::rule`.
const RULE_DIRS: &[(&str, &str, Harness)] = &[
    (".cursor/rules", ".mdc", Harness::Cursor),
    (".windsurf/rules", ".md", Harness::Windsurf),
    (".clinerules", ".md", Harness::Cline),
    (".claude/rules", ".md", Harness::Claude),
    (".github/instructions", ".instructions.md", Harness::Copilot),
];

/// `(file, harnesses)` for the always-on instruction brief. Mirrors
/// `kinds::instructions::instructions_path`, which is many-harnesses-to-one-file.
const INSTRUCTION_FILES: &[(&str, &[Harness])] = &[
    ("CLAUDE.md", &[Harness::Claude]),
    (".github/copilot-instructions.md", &[Harness::Copilot]),
    ("GEMINI.md", &[Harness::Gemini, Harness::Antigravity]),
    (
        "AGENTS.md",
        &[
            Harness::Codex,
            Harness::OpenCode,
            Harness::Cursor,
            Harness::Windsurf,
            Harness::Cline,
            Harness::Aider,
        ],
    ),
];

/// `(file, servers key, harness)` for MCP server declarations.
const MCP_FILES: &[(&str, &str, Harness)] = &[
    (".mcp.json", "mcpServers", Harness::Claude),
    (".cursor/mcp.json", "mcpServers", Harness::Cursor),
    (".vscode/mcp.json", "servers", Harness::Copilot),
];

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// Read `root`'s native harness config into normalized capabilities.
///
/// `only` restricts the scan to one harness; `None` scans every path table.
/// Reading is non-destructive — nothing is written until [`write`].
pub fn scan(root: &Path, only: Option<Harness>) -> Result<Import, String> {
    let mut imp = Import::default();
    let want = |h: Harness| only.is_none_or(|o| o == h);

    scan_skills(root, &want, &mut imp)?;
    scan_commands(root, &want, &mut imp)?;
    scan_agents(root, &want, &mut imp)?;
    scan_rules(root, &want, &mut imp)?;
    scan_instructions(root, &want, &mut imp);
    scan_mcp(root, &want, &mut imp);
    scan_claude_settings(root, &want, &mut imp);
    scan_cursor_hooks(root, &want, &mut imp);
    scan_gemini_commands(root, &want, &mut imp)?;

    dedupe(&mut imp);
    reconcile_instructions(&mut imp);

    if imp.items.is_empty() {
        imp.notes.push(format!(
            "no harness config found under {} — looked for {}",
            root.display(),
            searched_paths(only).join(", ")
        ));
    }
    Ok(imp)
}

/// The native paths a scan looks at, for the "found nothing" message. Being
/// specific here is the difference between "it doesn't work" and "ah, my rules
/// are somewhere else".
pub fn searched_paths(only: Option<Harness>) -> Vec<&'static str> {
    let want = |h: Harness| only.is_none_or(|o| o == h);
    let mut out: Vec<&'static str> = Vec::new();
    out.extend(SKILL_DIRS.iter().filter(|(_, h)| want(*h)).map(|(d, _)| *d));
    out.extend(
        COMMAND_DIRS
            .iter()
            .filter(|(_, _, h)| want(*h))
            .map(|(d, _, _)| *d),
    );
    out.extend(
        AGENT_DIRS
            .iter()
            .filter(|(_, _, h)| want(*h))
            .map(|(d, _, _)| *d),
    );
    out.extend(
        RULE_DIRS
            .iter()
            .filter(|(_, _, h)| want(*h))
            .map(|(d, _, _)| *d),
    );
    out.extend(
        INSTRUCTION_FILES
            .iter()
            .filter(|(_, hs)| hs.iter().any(|h| want(*h)))
            .map(|(f, _)| *f),
    );
    out.extend(
        MCP_FILES
            .iter()
            .filter(|(_, _, h)| want(*h))
            .map(|(f, _, _)| *f),
    );
    if want(Harness::Claude) {
        out.push(".claude/settings.json");
    }
    out
}

/// Files directly in `dir` whose name ends with `suffix`, sorted for a stable
/// report. Sub-directories are not descended into: `.clinerules/workflows/` is
/// commands, not more rules.
fn files_in(dir: &Path, suffix: &str) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("scan {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(suffix))
        })
        .collect();
    out.sort();
    Ok(out)
}

fn subdirs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("scan {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    Ok(out)
}

/// A capability id from a filename: lowercased, with anything outside
/// `[a-z0-9._-]` folded to `-`. Ids become directory names and appear in every
/// emitted native path, so they cannot carry spaces or separators.
fn safe_id(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in raw.trim().to_ascii_lowercase().chars() {
        let ok = c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-';
        if ok {
            out.push(c);
            last_dash = c == '-';
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "imported".to_string()
    } else {
        trimmed
    }
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

// ---- skills ---------------------------------------------------------------

/// `SKILL.md` is already the near-standard shape open-harness normalizes to, so
/// a skill imports as itself: the document is copied verbatim, frontmatter and
/// all. The skill kind passes unknown frontmatter keys through on every harness,
/// so nothing is lost and nothing is invented.
fn scan_skills(
    root: &Path,
    want: &impl Fn(Harness) -> bool,
    imp: &mut Import,
) -> Result<(), String> {
    for (dir, harness) in SKILL_DIRS {
        if !want(*harness) {
            continue;
        }
        for sub in subdirs(&root.join(dir))? {
            let doc = sub.join("SKILL.md");
            if !doc.is_file() {
                continue;
            }
            let id = safe_id(&sub.file_name().unwrap_or_default().to_string_lossy());
            let text = std::fs::read_to_string(&doc)
                .map_err(|e| format!("read {}: {e}", doc.display()))?;
            let mut harnesses = vec![harness.id()];
            // `.agents/skills` is read by two harnesses, not one.
            if *dir == ".agents/skills" {
                harnesses.push(Harness::Antigravity.id());
            }
            imp.items.push(Item {
                id,
                kind: KindId::Skill,
                from: rel(root, &doc),
                harnesses,
                status: Status::Portable,
                substance: yaml::parse_document(&text)
                    .map(|(_, b)| b.trim().to_string())
                    .unwrap_or_else(|_| text.trim().to_string()),
                files: vec![OutFile {
                    path: "SKILL.md".into(),
                    contents: text,
                }],
                notes: Vec::new(),
            });
        }
    }
    Ok(())
}

// ---- commands -------------------------------------------------------------

/// Commands need a real translation in both directions: the portable body uses
/// `{{args}}` / `{{argN}}`, while every markdown harness writes `$ARGUMENTS` /
/// `$N`. Importing without converting would leave a body that renders back out
/// as a literal `$ARGUMENTS` on Gemini and Pi, which read `{{args}}`.
fn scan_commands(
    root: &Path,
    want: &impl Fn(Harness) -> bool,
    imp: &mut Import,
) -> Result<(), String> {
    for (dir, suffix, harness) in COMMAND_DIRS {
        if !want(*harness) {
            continue;
        }
        for file in files_in(&root.join(dir), suffix)? {
            let name = file.file_name().unwrap_or_default().to_string_lossy();
            let id = safe_id(name.trim_end_matches(suffix));
            let text = std::fs::read_to_string(&file)
                .map_err(|e| format!("read {}: {e}", file.display()))?;
            let (fm, body) = match yaml::parse_document(&text) {
                Ok(p) => p,
                Err(e) => {
                    imp.items.push(Item::skipped(
                        id,
                        KindId::Command,
                        rel(root, &file),
                        harness.id(),
                        format!("frontmatter does not parse: {e}"),
                    ));
                    continue;
                }
            };

            let mut notes = Vec::new();
            // Pi already writes the portable spelling; everyone else uses `$`.
            let body = if *harness == Harness::Pi {
                body
            } else {
                let converted = to_braces(&body);
                if converted != body {
                    notes.push(
                        "converted `$ARGUMENTS` / `$N` to the portable `{{args}}` / `{{argN}}`"
                            .to_string(),
                    );
                }
                converted
            };

            // A name is what `oh check` and the emitted frontmatter show; the
            // id is the only thing a native file reliably carries.
            let mut out = Map::new();
            out.insert("name".into(), json!(id));
            carry(&fm, "name", &mut out);
            carry(&fm, "description", &mut out);
            carry(&fm, "model", &mut out);
            carry(&fm, "agent", &mut out);
            if let Some(v) = fm.get("argument-hint").or_else(|| fm.get("argument_hint")) {
                out.insert("argument-hint".into(), v.clone());
            }
            note_dropped(
                &fm,
                &[
                    "name",
                    "description",
                    "model",
                    "agent",
                    "argument-hint",
                    "argument_hint",
                ],
                harness,
                &mut notes,
            );

            imp.items.push(Item {
                id,
                kind: KindId::Command,
                from: rel(root, &file),
                harnesses: vec![harness.id()],
                status: Status::Portable,
                substance: body.trim().to_string(),
                files: vec![OutFile {
                    path: "COMMAND.md".into(),
                    contents: document(&out, &body),
                }],
                notes,
            });
        }
    }
    Ok(())
}

/// `$ARGUMENTS` → `{{args}}`, `$1` → `{{arg1}}`. The inverse of
/// `kinds::command::to_dollar`.
fn to_braces(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            let rest: String = chars[i + 1..].iter().collect();
            if let Some(tail) = rest.strip_prefix("ARGUMENTS") {
                out.push_str("{{args}}");
                i += 1 + "ARGUMENTS".len();
                let _ = tail;
                continue;
            }
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                out.push_str(&format!("{{{{arg{digits}}}}}"));
                i += 1 + digits.len();
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ---- agents ---------------------------------------------------------------

/// Subagents disagree on the *tool allowance* more than on anything else:
/// Claude and Copilot list tools, OpenCode gates them with a permission map.
/// Both are read back into the canonical vocabulary here, so the imported agent
/// re-emits correctly onto the other two.
fn scan_agents(
    root: &Path,
    want: &impl Fn(Harness) -> bool,
    imp: &mut Import,
) -> Result<(), String> {
    for (dir, suffix, harness) in AGENT_DIRS {
        if !want(*harness) {
            continue;
        }
        for file in files_in(&root.join(dir), suffix)? {
            let name = file.file_name().unwrap_or_default().to_string_lossy();
            let id = safe_id(name.trim_end_matches(suffix));
            let text = std::fs::read_to_string(&file)
                .map_err(|e| format!("read {}: {e}", file.display()))?;
            let (fm, body) = match yaml::parse_document(&text) {
                Ok(p) => p,
                Err(e) => {
                    imp.items.push(Item::skipped(
                        id,
                        KindId::Agent,
                        rel(root, &file),
                        harness.id(),
                        format!("frontmatter does not parse: {e}"),
                    ));
                    continue;
                }
            };

            let mut notes = Vec::new();
            let mut out = Map::new();
            out.insert("name".into(), json!(id));
            carry(&fm, "name", &mut out);
            carry(&fm, "description", &mut out);
            carry(&fm, "model", &mut out);
            carry(&fm, "mode", &mut out);
            carry(&fm, "temperature", &mut out);

            if let Some(list) = read_tool_list(&fm, "tools") {
                let canonical = to_canonical(&list, *harness);
                if canonical != list {
                    notes.push(format!(
                        "mapped {}'s tool tokens to the canonical vocabulary: {} → {}",
                        harness.id(),
                        list.join(", "),
                        canonical.join(", ")
                    ));
                }
                out.insert("tools".into(), json!(canonical));
            }
            // OpenCode's permission map is a tool → verdict object.
            if let Some(perm) = fm.get("permission").and_then(|v| v.as_object()) {
                let mut mapped = Map::new();
                for (k, v) in perm {
                    for c in tools::canonical_tools(k, *harness) {
                        mapped.insert(c, v.clone());
                    }
                }
                if !mapped.is_empty() {
                    out.insert("permissions".into(), Value::Object(mapped));
                    notes.push(
                        "read OpenCode's `permission` map back into canonical tool verdicts"
                            .to_string(),
                    );
                }
            }
            note_dropped(
                &fm,
                &[
                    "name",
                    "description",
                    "model",
                    "mode",
                    "temperature",
                    "tools",
                    "permission",
                ],
                harness,
                &mut notes,
            );

            imp.items.push(Item {
                id,
                kind: KindId::Agent,
                from: rel(root, &file),
                harnesses: vec![harness.id()],
                status: Status::Portable,
                substance: body.trim().to_string(),
                files: vec![OutFile {
                    path: "AGENT.md".into(),
                    contents: document(&out, &body),
                }],
                notes,
            });
        }
    }
    Ok(())
}

fn read_tool_list(fm: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    match fm.get(key) {
        Some(Value::Array(a)) => Some(
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
        ),
        Some(Value::String(s)) => Some(tools::split_tool_string(s)),
        _ => None,
    }
}

fn to_canonical(list: &[String], harness: Harness) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in list {
        for c in tools::canonical_tools(t, harness) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

// ---- rules ----------------------------------------------------------------

/// The kind with the most translation to do. Five harnesses spell the same four
/// activations five different ways, and three of them cannot express all four —
/// so a rule imported from Copilot or Cline may genuinely not carry the
/// activation its author wanted. Where the native file is ambiguous the item
/// says so rather than picking silently.
fn scan_rules(
    root: &Path,
    want: &impl Fn(Harness) -> bool,
    imp: &mut Import,
) -> Result<(), String> {
    for (dir, suffix, harness) in RULE_DIRS {
        if !want(*harness) {
            continue;
        }
        for file in files_in(&root.join(dir), suffix)? {
            let name = file.file_name().unwrap_or_default().to_string_lossy();
            let id = safe_id(name.trim_end_matches(suffix));
            let text = std::fs::read_to_string(&file)
                .map_err(|e| format!("read {}: {e}", file.display()))?;
            let (fm, body) = match yaml::parse_document(&text) {
                Ok(p) => p,
                Err(e) => {
                    imp.items.push(Item::skipped(
                        id,
                        KindId::Rule,
                        rel(root, &file),
                        harness.id(),
                        format!("frontmatter does not parse: {e}"),
                    ));
                    continue;
                }
            };

            let (activation, globs, mut notes) = read_activation(&fm, *harness);
            let mut out = Map::new();
            out.insert("name".into(), json!(id));
            carry(&fm, "description", &mut out);
            out.insert("activation".into(), json!(activation_str(activation)));
            if !globs.is_empty() {
                out.insert("globs".into(), json!(globs));
            }
            note_dropped(
                &fm,
                &[
                    "name",
                    "description",
                    "globs",
                    "alwaysApply",
                    "trigger",
                    "applyTo",
                    "paths",
                ],
                harness,
                &mut notes,
            );

            imp.items.push(Item {
                id,
                kind: KindId::Rule,
                from: rel(root, &file),
                harnesses: vec![harness.id()],
                status: Status::Portable,
                substance: body.trim().to_string(),
                files: vec![OutFile {
                    path: "RULE.md".into(),
                    contents: document(&out, &body),
                }],
                notes,
            });
        }
    }
    Ok(())
}

fn activation_str(a: Activation) -> &'static str {
    match a {
        Activation::Always => "always",
        Activation::Glob => "glob",
        Activation::AgentRequested => "agent_requested",
        Activation::Manual => "manual",
    }
}

/// Read a native rule file's activation back to the normalized one. The inverse
/// of the render tables in `kinds::rule`.
fn read_activation(
    fm: &Map<String, Value>,
    harness: Harness,
) -> (Activation, Vec<String>, Vec<String>) {
    let mut notes = Vec::new();
    let csv = |v: &Value| -> Vec<String> {
        match v {
            Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect(),
            Value::String(s) => s
                .split(',')
                .map(|g| g.trim().to_string())
                .filter(|g| !g.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    };

    match harness {
        // Cursor is the only source that can express all four unambiguously.
        Harness::Cursor => {
            let always = fm
                .get("alwaysApply")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let globs = fm.get("globs").map(csv).unwrap_or_default();
            let has_desc = fm
                .get("description")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty());
            if always {
                (Activation::Always, Vec::new(), notes)
            } else if !globs.is_empty() {
                (Activation::Glob, globs, notes)
            } else if has_desc {
                (Activation::AgentRequested, Vec::new(), notes)
            } else {
                (Activation::Manual, Vec::new(), notes)
            }
        }
        Harness::Windsurf => {
            let globs = fm.get("globs").map(csv).unwrap_or_default();
            match fm.get("trigger").and_then(|v| v.as_str()) {
                Some("always_on") => (Activation::Always, Vec::new(), notes),
                Some("glob") => (Activation::Glob, globs, notes),
                Some("model_decision") => (Activation::AgentRequested, Vec::new(), notes),
                Some("manual") => (Activation::Manual, Vec::new(), notes),
                other => {
                    if let Some(t) = other {
                        notes.push(format!("unknown Windsurf trigger `{t}`; read as always-on"));
                    }
                    (Activation::Always, Vec::new(), notes)
                }
            }
        }
        Harness::Copilot => match fm.get("applyTo") {
            Some(v) => {
                let globs = csv(v);
                // `applyTo: "**"` is how the rule kind spells always-on here, and
                // an `applyTo` present but empty scopes to nothing in particular
                // — both read back as always.
                if globs.is_empty() || globs.iter().any(|g| g == "**") {
                    (Activation::Always, Vec::new(), notes)
                } else {
                    (Activation::Glob, globs, notes)
                }
            }
            None => {
                // Copilot writes no `applyTo` for both manual and always. It is
                // genuinely ambiguous, and guessing "always" silently would widen
                // where the rule applies.
                notes.push(
                    "no `applyTo`: Copilot cannot distinguish manual from always here — imported \
                     as manual, the narrower reading. Set `activation: always` if that is wrong"
                        .to_string(),
                );
                (Activation::Manual, Vec::new(), notes)
            }
        },
        // Cline and Claude: a `paths:` array means glob, nothing means always.
        _ => match fm.get("paths") {
            Some(v) => {
                let globs = csv(v);
                if globs.is_empty() {
                    (Activation::Always, Vec::new(), notes)
                } else {
                    (Activation::Glob, globs, notes)
                }
            }
            None => (Activation::Always, Vec::new(), notes),
        },
    }
}

// ---- instructions ---------------------------------------------------------

/// The always-on brief. Plain Markdown with no frontmatter, and several
/// harnesses share one file — so the item records every reader.
fn scan_instructions(root: &Path, want: &impl Fn(Harness) -> bool, imp: &mut Import) {
    for (file, harnesses) in INSTRUCTION_FILES {
        let readers: Vec<&'static str> = harnesses
            .iter()
            .filter(|h| want(**h))
            .map(|h| h.id())
            .collect();
        if readers.is_empty() {
            continue;
        }
        let path = root.join(file);
        if !path.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.trim().is_empty() {
            imp.items.push(Item::skipped(
                safe_id(file),
                KindId::Instructions,
                rel(root, &path),
                readers[0],
                "file is empty",
            ));
            continue;
        }
        // `AGENTS.md` → `agents-instructions`, but `copilot-instructions.md`
        // must not become `copilot-instructions-instructions`.
        let stem = file
            .trim_end_matches(".md")
            .rsplit('/')
            .next()
            .unwrap_or("project")
            .to_ascii_lowercase();
        let id = safe_id(&if stem.ends_with("instructions") {
            stem
        } else {
            format!("{stem}-instructions")
        });
        let mut out = Map::new();
        out.insert("name".into(), json!(id));
        out.insert(
            "description".into(),
            json!(format!("Project instructions imported from {file}")),
        );
        imp.items.push(Item {
            id,
            kind: KindId::Instructions,
            from: rel(root, &path),
            harnesses: readers,
            status: Status::Portable,
            substance: text.trim().to_string(),
            files: vec![OutFile {
                path: "INSTRUCTIONS.md".into(),
                contents: document(&out, &text),
            }],
            notes: Vec::new(),
        });
    }
}

// ---- MCP servers ----------------------------------------------------------

/// MCP is the one genuinely shared standard in this space, so a declared server
/// imports as a portable `tool` capability with no translation loss.
fn scan_mcp(root: &Path, want: &impl Fn(Harness) -> bool, imp: &mut Import) {
    for (file, key, harness) in MCP_FILES {
        if !want(*harness) {
            continue;
        }
        let path = root.join(file);
        if !path.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let doc: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                imp.items.push(Item::skipped(
                    safe_id(file),
                    KindId::Tool,
                    rel(root, &path),
                    harness.id(),
                    format!("not valid JSON: {e}"),
                ));
                continue;
            }
        };
        let Some(servers) = doc.get(key).and_then(|v| v.as_object()) else {
            imp.items.push(Item::skipped(
                safe_id(file),
                KindId::Tool,
                rel(root, &path),
                harness.id(),
                format!("no `{key}` object"),
            ));
            continue;
        };

        for (name, spec) in servers {
            let id = safe_id(name);
            let mut server = Map::new();
            match spec.get("url").and_then(|v| v.as_str()) {
                Some(url) => {
                    server.insert("transport".into(), json!("http"));
                    server.insert("url".into(), json!(url));
                }
                None => {
                    server.insert("transport".into(), json!("stdio"));
                    server.insert(
                        "command".into(),
                        json!(spec.get("command").and_then(|v| v.as_str()).unwrap_or("")),
                    );
                    server.insert(
                        "args".into(),
                        spec.get("args").cloned().unwrap_or_else(|| json!([])),
                    );
                }
            }
            if let Some(env) = spec.get("env") {
                server.insert("env".into(), env.clone());
            }

            let manifest = json!({
                "id": id,
                "name": name,
                "description": format!("MCP server '{name}' imported from {file}"),
                "kind": "tool",
                "version": "0.0.0",
                "tool": { "provider": "mcp", "delivery": "auto", "server": Value::Object(server) },
            });
            // The substance is the *server*, not the manifest: the manifest's
            // description names the file it came from, which would make the same
            // server declared in two places look like two different servers.
            let substance = serde_json::to_string(&manifest["tool"]["server"]).unwrap_or_default();
            let contents = match config::to_yaml(&manifest) {
                Ok(y) => y,
                Err(e) => {
                    imp.items.push(Item::skipped(
                        id,
                        KindId::Tool,
                        rel(root, &path),
                        harness.id(),
                        format!("could not render the manifest: {e}"),
                    ));
                    continue;
                }
            };
            imp.items.push(Item {
                id,
                kind: KindId::Tool,
                from: rel(root, &path),
                harnesses: vec![harness.id()],
                status: Status::Portable,
                substance,
                files: vec![OutFile {
                    path: crate::manifest::MANIFEST_NAME.to_string(),
                    contents,
                }],
                notes: vec![
                    "the server's command may resolve against this machine's install — check it \
                     before relying on it elsewhere"
                        .to_string(),
                ],
            });
        }
    }
}

// ---- Claude settings: permissions (portable) and hooks (not) --------------

fn scan_claude_settings(root: &Path, want: &impl Fn(Harness) -> bool, imp: &mut Import) {
    if !want(Harness::Claude) {
        return;
    }
    let path = root.join(".claude/settings.json");
    if !path.is_file() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let doc: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            imp.items.push(Item::skipped(
                "claude-settings",
                KindId::Permission,
                rel(root, &path),
                Harness::Claude.id(),
                format!("not valid JSON: {e}"),
            ));
            return;
        }
    };

    if let Some(perms) = doc.get("permissions").and_then(|v| v.as_object()) {
        import_permissions(root, &path, perms, imp);
    }
    if let Some(hooks) = doc.get("hooks") {
        import_claude_hooks(root, &path, hooks, imp);
    }
}

/// `.claude/settings.json` `permissions` → the portable `permission` kind.
/// Claude writes `ToolName(pattern)`; the tool half maps to the canonical
/// vocabulary and the pattern is carried through.
fn import_permissions(root: &Path, path: &Path, perms: &Map<String, Value>, imp: &mut Import) {
    let mut rules: Vec<Value> = Vec::new();
    let mut notes = Vec::new();
    for verdict in ["deny", "ask", "allow"] {
        let Some(list) = perms.get(verdict).and_then(|v| v.as_array()) else {
            continue;
        };
        for entry in list.iter().filter_map(|v| v.as_str()) {
            let (tool, pattern) = match entry.split_once('(') {
                Some((t, rest)) => (t.trim(), rest.trim_end_matches(')').to_string()),
                None => (entry.trim(), "*".to_string()),
            };
            let canonical = tools::canonical_tools(tool, Harness::Claude);
            for c in &canonical {
                rules.push(json!({
                    "tool": c.split('(').next().unwrap_or(c),
                    "pattern": pattern,
                    "verdict": verdict,
                }));
            }
        }
    }
    if rules.is_empty() {
        return;
    }
    // `defaultMode` has no portable equivalent beyond the three verdicts.
    if let Some(mode) = perms.get("defaultMode").and_then(|v| v.as_str()) {
        notes.push(format!(
            "Claude's `defaultMode: {mode}` has no portable equivalent and was not imported; \
             the capability's `default` stays `ask`"
        ));
    }
    let manifest = json!({
        "id": "claude-permissions",
        "name": "claude-permissions",
        "description": "Tool permissions imported from .claude/settings.json",
        "kind": "permission",
        "version": "0.0.0",
        "permission": { "default": "ask", "rules": rules },
    });
    match config::to_yaml(&manifest) {
        Ok(contents) => imp.items.push(Item {
            id: "claude-permissions".into(),
            kind: KindId::Permission,
            from: rel(root, path),
            harnesses: vec![Harness::Claude.id()],
            status: Status::Portable,
            substance: contents.clone(),
            files: vec![OutFile {
                path: crate::manifest::MANIFEST_NAME.to_string(),
                contents,
            }],
            notes,
        }),
        Err(e) => imp.items.push(Item::skipped(
            "claude-permissions",
            KindId::Permission,
            rel(root, path),
            Harness::Claude.id(),
            format!("could not render the manifest: {e}"),
        )),
    }
}

/// Hooks are the honest failure. A `.claude/settings.json` hook is a shell
/// command wired to Claude's own hook schema — it is not a `hook@1` capability,
/// its command speaks Claude's payload shape, and open-harness cannot make it
/// portable by rewriting a path. It is carried verbatim as a `native`
/// capability so it survives a sync, and reported Unsupported everywhere else.
fn import_claude_hooks(root: &Path, path: &Path, hooks: &Value, imp: &mut Import) {
    let events: Vec<String> = hooks
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    if events.is_empty() {
        return;
    }
    let body = serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap_or_default();
    let reason = "Claude Code hooks are shell commands against Claude's own hook schema, not the \
                  open-harness `hook@1` stdio contract — the command would receive a payload it \
                  cannot read on any other harness"
        .to_string();
    let manifest = json!({
        "id": "claude-hooks",
        "name": "claude-hooks",
        "description": format!("Native Claude Code hooks imported from .claude/settings.json ({})", events.join(", ")),
        "kind": "native",
        "version": "0.0.0",
        "native": {
            "harness": "claude-code",
            "reason": reason,
            "artifacts": [{ "path": ".claude/hooks/imported.json", "body": body }],
        },
    });
    match config::to_yaml(&manifest) {
        Ok(contents) => imp.items.push(Item {
            id: "claude-hooks".into(),
            kind: KindId::Native,
            from: rel(root, path),
            harnesses: vec![Harness::Claude.id()],
            status: Status::Native(format!(
                "{} hook event(s) ({}) — not portable",
                events.len(),
                events.join(", ")
            )),
            substance: contents.clone(),
            files: vec![OutFile {
                path: crate::manifest::MANIFEST_NAME.to_string(),
                contents,
            }],
            notes: vec![
                "to make these portable, re-author them as `hook@1` capabilities: \
                 `oh scaffold --kind hook`"
                    .to_string(),
            ],
        }),
        Err(e) => imp.items.push(Item::skipped(
            "claude-hooks",
            KindId::Native,
            rel(root, path),
            Harness::Claude.id(),
            format!("could not render the manifest: {e}"),
        )),
    }
}

fn scan_cursor_hooks(root: &Path, want: &impl Fn(Harness) -> bool, imp: &mut Import) {
    if !want(Harness::Cursor) {
        return;
    }
    let path = root.join(".cursor/hooks.json");
    if !path.is_file() {
        return;
    }
    let Ok(body) = std::fs::read_to_string(&path) else {
        return;
    };
    if serde_json::from_str::<Value>(&body).is_err() {
        imp.items.push(Item::skipped(
            "cursor-hooks",
            KindId::Native,
            rel(root, &path),
            Harness::Cursor.id(),
            "not valid JSON",
        ));
        return;
    }
    let manifest = json!({
        "id": "cursor-hooks",
        "name": "cursor-hooks",
        "description": "Native Cursor hooks imported from .cursor/hooks.json",
        "kind": "native",
        "version": "0.0.0",
        "native": {
            "harness": "cursor",
            "reason": "Cursor hooks are commands against Cursor's own per-event payload shape \
                       (snake_case, event-specific) and its `permission` reply — not the \
                       open-harness `hook@1` stdio contract",
            "artifacts": [{ "path": ".cursor/hooks.json", "body": body }],
        },
    });
    match config::to_yaml(&manifest) {
        Ok(contents) => imp.items.push(Item {
            id: "cursor-hooks".into(),
            kind: KindId::Native,
            from: rel(root, &path),
            harnesses: vec![Harness::Cursor.id()],
            status: Status::Native("Cursor's native hook config — not portable".into()),
            substance: contents.clone(),
            files: vec![OutFile {
                path: crate::manifest::MANIFEST_NAME.to_string(),
                contents,
            }],
            notes: vec![
                "to make these portable, re-author them as `hook@1` capabilities: \
                 `oh scaffold --kind hook`"
                    .to_string(),
            ],
        }),
        Err(e) => imp.items.push(Item::skipped(
            "cursor-hooks",
            KindId::Native,
            rel(root, &path),
            Harness::Cursor.id(),
            format!("could not render the manifest: {e}"),
        )),
    }
}

/// Gemini's commands are TOML. open-harness reads YAML, JSON and Markdown, and
/// hand-rolling a TOML parser to import a handful of prompt files is the wrong
/// trade — so these are reported, with the reason, rather than skipped in
/// silence or half-parsed with a regex.
fn scan_gemini_commands(
    root: &Path,
    want: &impl Fn(Harness) -> bool,
    imp: &mut Import,
) -> Result<(), String> {
    if !want(Harness::Gemini) {
        return Ok(());
    }
    for file in files_in(&root.join(".gemini/commands"), ".toml")? {
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        imp.items.push(Item::skipped(
            safe_id(name.trim_end_matches(".toml")),
            KindId::Command,
            rel(root, &file),
            Harness::Gemini.id(),
            "Gemini commands are TOML, which open-harness does not read — copy the `prompt` \
             into a COMMAND.md by hand (`oh scaffold --kind command`)",
        ));
    }
    Ok(())
}

// ---- shared helpers -------------------------------------------------------

fn carry(fm: &Map<String, Value>, key: &str, out: &mut Map<String, Value>) {
    if let Some(v) = fm.get(key) {
        out.insert(key.to_string(), v.clone());
    }
}

/// Note frontmatter keys the normalized model has no home for. They are dropped
/// — but *said*, because a silently discarded `disable-model-invocation` changes
/// how the thing behaves.
fn note_dropped(
    fm: &Map<String, Value>,
    kept: &[&str],
    harness: &Harness,
    notes: &mut Vec<String>,
) {
    let dropped: Vec<&str> = fm
        .keys()
        .map(|k| k.as_str())
        .filter(|k| !kept.contains(k))
        .collect();
    if !dropped.is_empty() {
        notes.push(format!(
            "{}-specific frontmatter not carried into the portable form: {} — re-add it under \
             `overrides.{}` if you need it",
            harness.id(),
            dropped.join(", "),
            harness.id()
        ));
    }
}

/// A single-file capability document: YAML frontmatter plus the body.
fn document(front: &Map<String, Value>, body: &str) -> String {
    let pairs: Vec<(String, String)> = front
        .iter()
        .map(|(k, v)| (k.clone(), yaml::render_value(v)))
        .collect();
    let mut s = yaml::frontmatter(&pairs);
    s.push('\n');
    s.push_str(body.trim_end());
    s.push('\n');
    s
}

/// Collapse items that describe the same capability found under more than one
/// harness's path.
///
/// A skill living in both `.claude/skills/x/` and `.cursor/skills/x/` is one
/// capability, and importing it twice would create two colliding ids. Identical
/// content merges into a single item recording both readers. **Differing**
/// content does not merge: the second copy is reported as skipped, naming the
/// file it conflicts with, because picking one silently would drop whichever
/// version the author actually meant.
fn dedupe(imp: &mut Import) {
    let mut seen: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
    let mut drop_to_skip: Vec<(usize, String)> = Vec::new();

    for i in 0..imp.items.len() {
        let key = (imp.items[i].id.clone(), imp.items[i].kind.as_str());
        match seen.get(&key) {
            None => {
                seen.insert(key, i);
            }
            Some(&first) => {
                if imp.items[first].substance == imp.items[i].substance {
                    let identical = imp.items[first].files == imp.items[i].files;
                    let extra: Vec<&'static str> = imp.items[i].harnesses.clone();
                    for h in extra {
                        if !imp.items[first].harnesses.contains(&h) {
                            imp.items[first].harnesses.push(h);
                        }
                    }
                    let from = imp.items[i].from.clone();
                    let kept_from = imp.items[first].from.clone();
                    imp.items[first].notes.push(if identical {
                        format!("the identical file is also at {from}")
                    } else {
                        // Same substance, different dialect metadata — which is
                        // exactly what a project that already fans one concept
                        // out to several harnesses looks like. Saying which copy
                        // supplied the metadata matters, because the others'
                        // frontmatter is what got dropped.
                        format!(
                            "the same content is also at {from}, in that harness's own spelling; \
                             the metadata came from {kept_from}"
                        )
                    });
                    drop_to_skip.push((i, String::new()));
                } else {
                    let other = imp.items[first].from.clone();
                    drop_to_skip.push((
                        i,
                        format!(
                            "a different `{}` with the same id was already imported from {other}; \
                             their content differs, so this one is left for you to merge by hand",
                            imp.items[i].kind.as_str()
                        ),
                    ));
                }
            }
        }
    }

    // Walk backwards so the indices stay valid.
    for (i, why) in drop_to_skip.into_iter().rev() {
        if why.is_empty() {
            imp.items.remove(i);
        } else {
            imp.items[i].status = Status::Skipped(why);
            imp.items[i].files.clear();
        }
    }
}

/// Reconcile several instruction briefs, which [`dedupe`] cannot: they have
/// different ids (`claude-instructions`, `agents-instructions`) and so never
/// meet there, but the *instructions kind targets one file per harness*. Two of
/// them would both try to write `CLAUDE.md` and `AGENTS.md`, and only one could
/// win.
///
/// A repo carrying both `CLAUDE.md` and `AGENTS.md` is extremely common, and
/// usually they say the same thing — so identical briefs merge into one
/// capability naming every file it came from. When they genuinely differ,
/// merging would silently pick a winner, so both are kept and each is told that
/// only one can install. That surfaces at `oh sync` as a first-producer-wins
/// warning; saying it here means it is not a surprise.
fn reconcile_instructions(imp: &mut Import) {
    let mut first: Option<usize> = None;
    let mut merge: Vec<usize> = Vec::new();
    let mut distinct: Vec<usize> = Vec::new();

    for i in 0..imp.items.len() {
        if imp.items[i].kind != KindId::Instructions
            || !matches!(imp.items[i].status, Status::Portable)
        {
            continue;
        }
        match first {
            None => first = Some(i),
            Some(f) => {
                if imp.items[f].substance == imp.items[i].substance {
                    merge.push(i);
                } else {
                    distinct.push(i);
                }
            }
        }
    }
    let Some(f) = first else { return };

    for &i in &merge {
        let from = imp.items[i].from.clone();
        let readers = imp.items[i].harnesses.clone();
        for h in readers {
            if !imp.items[f].harnesses.contains(&h) {
                imp.items[f].harnesses.push(h);
            }
        }
        imp.items[f]
            .notes
            .push(format!("{from} holds the same brief; merged into this one"));
    }
    for &i in merge.iter().rev() {
        imp.items.remove(i);
    }

    if !distinct.is_empty() {
        // Indices shifted by the removals above, so re-find by kind.
        let ids: Vec<String> = imp
            .items
            .iter()
            .filter(|it| it.kind == KindId::Instructions && matches!(it.status, Status::Portable))
            .map(|it| it.id.clone())
            .collect();
        if ids.len() > 1 {
            for it in imp.items.iter_mut().filter(|it| {
                it.kind == KindId::Instructions && matches!(it.status, Status::Portable)
            }) {
                it.notes.push(format!(
                    "this project has {} differing instruction briefs ({}) — the instructions \
                     kind writes one file per harness, so only one can install. Merge them, or \
                     keep one and drop the rest",
                    ids.len(),
                    ids.join(", ")
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Written {
    pub created: Vec<String>,
    /// Ids that already existed under the output directory. Never overwritten.
    pub collisions: Vec<String>,
    pub dry_run: bool,
}

/// Write the importable items into `out_dir`, one directory per capability.
///
/// An id whose directory already exists is a **collision**: it is reported and
/// left alone. An importer that clobbers the capability you already hand-edited
/// is worse than one that refuses.
pub fn write(out_dir: &Path, imp: &Import, dry_run: bool) -> Result<Written, String> {
    let mut w = Written {
        dry_run,
        ..Default::default()
    };
    for item in imp.writable() {
        let dir = out_dir.join(&item.id);
        if dir.exists() {
            w.collisions.push(item.id.clone());
            continue;
        }
        if !dry_run {
            std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
            for f in &item.files {
                let path = dir.join(&f.path);
                std::fs::write(&path, &f.contents)
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
            }
        }
        w.created.push(item.id.clone());
    }
    Ok(w)
}
