//! Canonical tool vocabulary shared across kinds (gap 6).
//!
//! One canonical tool name → the token each harness actually uses. Three kinds
//! need this exact mapping — the permission kind (allow/deny entries), the skill
//! kind (`allowed-tools`), and the agent kind (`tools`) — so it lives here once
//! rather than being re-derived (and left to drift) in each.
//!
//! The canonical set is nine tools with a per-harness table verified against each
//! vendor's docs. Unknown names are **passed through verbatim** (a spec-compliant
//! runtime ignores tool tokens it doesn't recognize), so an author is never
//! blocked by a name open-harness hasn't catalogued.
//!
//! Two documented gotchas are encoded here so consumers inherit them:
//!   * Claude renamed `Task` → `Agent` in v2.1.63 (`Task` still works as an alias);
//!     the canonical `task` therefore lowers to `Agent`.
//!   * Copilot tool tokens are VS Code `toolReferenceName`s (`readFile`, not
//!     `copilot_readFile`; `runInTerminal`, not the deprecated `runCommands`).

use crate::adapters::Harness;

/// The canonical tool names with verified per-harness mappings. Authors may use
/// any string; these are the ones that translate.
pub const CANONICAL: [&str; 9] = [
    "read",
    "edit",
    "write",
    "grep",
    "glob",
    "bash",
    "web-fetch",
    "web-search",
    "task",
];

/// Fold an author-supplied name to its canonical key: lowercased, with spaces and
/// underscores turned into hyphens, so `web_fetch`, `webfetch`, and `Web Fetch`
/// all resolve to `web-fetch`.
fn canonical_key(name: &str) -> String {
    let hyphenated = name.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    match hyphenated.as_str() {
        // No-separator spellings some ecosystems use.
        "webfetch" => "web-fetch".to_string(),
        "websearch" => "web-search".to_string(),
        other => other.to_string(),
    }
}

/// The native token for a canonical tool `name` on `harness`. Falls back to a
/// verbatim pass-through for names outside the canonical set (title-cased for
/// Claude, whose built-in tool names are PascalCase by convention).
pub fn native_tool(name: &str, harness: Harness) -> String {
    match mapped(&canonical_key(name), harness) {
        Some(token) => token.to_string(),
        None => match harness {
            Harness::Claude => title_case(name),
            _ => name.to_string(),
        },
    }
}

/// Map a list of canonical tool names to native tokens, de-duplicated with
/// first-seen order preserved. (Several canonical tools can collapse onto one
/// native token — e.g. Copilot maps both `grep` and `glob` to `search`.)
pub fn native_tools(names: &[String], harness: Harness) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let token = native_tool(n, harness);
        if !out.contains(&token) {
            out.push(token);
        }
    }
    out
}

/// The verified canonical → native table. `None` = this harness has no native
/// token for that canonical tool (caller passes the name through verbatim).
fn mapped(key: &str, harness: Harness) -> Option<&'static str> {
    use Harness::*;
    Some(match (harness, key) {
        // Claude Code — PascalCase built-ins.
        (Claude, "read") => "Read",
        (Claude, "edit") => "Edit",
        (Claude, "write") => "Write",
        (Claude, "grep") => "Grep",
        (Claude, "glob") => "Glob",
        (Claude, "bash") => "Bash",
        (Claude, "web-fetch") => "WebFetch",
        (Claude, "web-search") => "WebSearch",
        (Claude, "task") => "Agent", // renamed from `Task` in v2.1.63 (alias kept)

        // OpenCode — lowercase built-ins.
        (OpenCode, "read") => "read",
        (OpenCode, "edit") => "edit",
        (OpenCode, "write") => "write",
        (OpenCode, "grep") => "grep",
        (OpenCode, "glob") => "glob",
        (OpenCode, "bash") => "bash",
        (OpenCode, "web-fetch") => "webfetch",
        (OpenCode, "web-search") => "websearch",
        (OpenCode, "task") => "task",

        // Copilot — VS Code toolReferenceNames (several canonical → one native).
        (Copilot, "read") => "readFile",
        (Copilot, "edit") => "editFiles",
        (Copilot, "write") => "editFiles",
        (Copilot, "grep") => "search",
        (Copilot, "glob") => "search",
        (Copilot, "bash") => "runInTerminal",
        (Copilot, "web-fetch") => "fetch",
        // web-search: Copilot has no built-in web search — passthrough (unmapped).
        (Copilot, "task") => "agent",

        // Gemini CLI — snake_case built-ins.
        (Gemini, "read") => "read_file",
        (Gemini, "edit") => "replace",
        (Gemini, "write") => "write_file",
        (Gemini, "grep") => "search_file_content",
        (Gemini, "glob") => "glob",
        (Gemini, "bash") => "run_shell_command",
        (Gemini, "web-fetch") => "web_fetch",
        (Gemini, "web-search") => "google_web_search",
        // Gemini has no first-class subagent tool.
        _ => return None,
    })
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ---- tool-list deserialization --------------------------------------------

/// Split a native tool string into entries. Accepts comma **or** whitespace as
/// separators (both native spellings occur) and keeps a parenthesized spec whole
/// — `"Read Grep Bash(git diff:*)"` → `["Read", "Grep", "Bash(git diff:*)"]` —
/// so the internal spaces/colons of `Bash(git diff:*)` are preserved.
pub fn split_tool_string(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let t = cur.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
        cur.clear();
    };
    for c in s.chars() {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' => {
                depth = (depth - 1).max(0);
                cur.push(c);
            }
            ',' if depth == 0 => flush(&mut cur, &mut out),
            c if c.is_whitespace() && depth == 0 => flush(&mut cur, &mut out),
            _ => cur.push(c),
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// A serde deserializer for a tool list that accepts **either** a sequence
/// (`[read, grep]`) **or** a scalar string in the native Agent-Skills / Claude
/// spelling (`Read Grep Bash(git diff:*)`). Both are valid; without this the
/// string form fails to deserialize, which — combined with a silent default —
/// used to void the whole capability config.
pub fn deserialize_tool_list<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, SeqAccess, Visitor};
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a tool list: a sequence, or a space/comma-separated string")
        }
        fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
            Ok(split_tool_string(s))
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                out.push(s);
            }
            Ok(out)
        }
    }
    d.deserialize_any(V)
}
