//! `oh scaffold` — capability starter templates (#18).
//!
//! Generates a ready-to-edit capability directory so an author starts from a
//! working example, not a blank file. A **hook** scaffold is *runnable* — its
//! starter reads the canonical payload on stdin and writes a decision on stdout,
//! in Python, TypeScript/Node, or bash — so `oh sync` / the dispatcher can run
//! it immediately. Generative kinds (skill / rule / command / tool / permission)
//! get a valid manifest + body skeleton that plans cleanly.

use crate::kind::KindId;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    TypeScript,
    Bash,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> {
        Some(match s {
            "python" | "py" => Lang::Python,
            "typescript" | "ts" | "node" | "js" => Lang::TypeScript,
            "bash" | "sh" | "shell" => Lang::Bash,
            _ => return None,
        })
    }
}

/// Scaffold a capability `id` of `kind` into `dir/<id>/`. Returns the created
/// file paths. Refuses to overwrite an existing `capability.json`.
pub fn scaffold(kind: KindId, lang: Lang, id: &str, dir: &Path) -> Result<Vec<String>, String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "invalid capability id '{id}' (use letters, digits, '-', '_')"
        ));
    }
    let cap_dir = dir.join(id);
    let manifest_path = cap_dir.join("capability.json");
    if manifest_path.exists() {
        return Err(format!("{} already exists", manifest_path.display()));
    }

    let files: Vec<(String, String)> = match kind {
        KindId::Hook => hook_files(lang, id),
        KindId::Skill => vec![
            (
                "capability.json".into(),
                manifest(id, "skill", r#""skill": { "body_file": "SKILL.md" }"#),
            ),
            (
                "SKILL.md".into(),
                format!(
                    "# {title}\n\nTODO: write the skill guidance here.\n",
                    title = title(id)
                ),
            ),
        ],
        KindId::Rule => vec![
            (
                "capability.json".into(),
                manifest(
                    id,
                    "rule",
                    r#""rule": { "activation": "glob", "globs": ["src/**"], "body_file": "RULE.md" }"#,
                ),
            ),
            (
                "RULE.md".into(),
                "TODO: write the rule guidance (applied to the globs above).\n".into(),
            ),
        ],
        KindId::Command => vec![(
            "capability.json".into(),
            manifest(
                id,
                "command",
                r#""command": { "argument_hint": "<args>", "body": "TODO: prompt body using {{args}} / {{arg1}}." }"#,
            ),
        )],
        KindId::Tool => vec![(
            "capability.json".into(),
            manifest(
                id,
                "tool",
                r#""tool": { "provider": "exec", "delivery": "cli", "exec": { "command": "TODO-your-cli" }, "usage": "TODO-your-cli <args>" }"#,
            ),
        )],
        KindId::Permission => vec![(
            "capability.json".into(),
            manifest(
                id,
                "permission",
                r#""permission": { "default": "ask", "rules": [ { "tool": "bash", "pattern": "git *", "verdict": "allow" } ] }"#,
            ),
        )],
    };

    std::fs::create_dir_all(&cap_dir).map_err(|e| format!("mkdir {}: {e}", cap_dir.display()))?;
    let mut written = Vec::new();
    for (name, contents) in files {
        let path = cap_dir.join(&name);
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
        // Make hook starters executable-ish is unnecessary (invoked via interpreter),
        // so no chmod here.
        written.push(path.to_string_lossy().into_owned());
    }
    Ok(written)
}

/// The manifest + runnable starter for a hook, per language.
fn hook_files(lang: Lang, id: &str) -> Vec<(String, String)> {
    let (script_name, run, starter) = match lang {
        Lang::Python => (
            "hook.py",
            r#""run": { "command": "python3", "args": ["hook.py"] }"#,
            PY_HOOK,
        ),
        Lang::TypeScript => (
            "hook.mjs",
            r#""run": { "command": "node", "args": ["hook.mjs"] }"#,
            TS_HOOK,
        ),
        Lang::Bash => (
            "hook.sh",
            r#""run": { "command": "sh", "args": ["hook.sh"] }"#,
            BASH_HOOK,
        ),
    };
    let manifest = format!(
        "{{\n  \"id\": \"{id}\",\n  \"name\": \"{title}\",\n  \"description\": \"TODO: describe what this hook guards.\",\n  \"kind\": \"hook\",\n  \"protocol\": \"hook@1\",\n  {run},\n  \"events\": [\n    {{ \"phase\": \"pre\", \"subject\": \"tool\", \"tool_class\": \"any\" }}\n  ]\n}}\n",
        title = title(id),
    );
    vec![
        ("capability.json".into(), manifest),
        (script_name.into(), starter.to_string()),
    ]
}

/// A generative-kind manifest with a kind config block spliced in.
fn manifest(id: &str, kind: &str, config_block: &str) -> String {
    format!(
        "{{\n  \"id\": \"{id}\",\n  \"name\": \"{title}\",\n  \"description\": \"TODO: describe this {kind}.\",\n  \"kind\": \"{kind}\",\n  {config_block}\n}}\n",
        title = title(id),
    )
}

fn title(id: &str) -> String {
    let mut c = id.replace(['-', '_'], " ");
    if let Some(first) = c.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    c
}

const PY_HOOK: &str = r#"#!/usr/bin/env python3
"""An open-harness hook capability (scaffolded).

Contract: read a CanonicalPayload as JSON on stdin, write a Decision as JSON on
stdout. Nothing here knows or cares which harness invoked it.
"""
import sys
import json


def main() -> None:
    raw = sys.stdin.read()
    payload = json.loads(raw) if raw.strip() else {}
    tool = payload.get("tool") or {}
    _ = tool  # TODO: inspect tool.get("name") / tool.get("input") and decide.

    # Return {"decision": "deny", "reason": "..."} to block a blocking event,
    # or {"decision": "allow", "context_append": "..."} to add model context.
    print(json.dumps({"decision": "allow"}))


if __name__ == "__main__":
    main()
"#;

const TS_HOOK: &str = r#"#!/usr/bin/env node
// An open-harness hook capability (scaffolded).
// CanonicalPayload in on stdin, Decision out on stdout.
let raw = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d) => (raw += d));
process.stdin.on("end", () => {
  let payload = {};
  try {
    payload = raw.trim() ? JSON.parse(raw) : {};
  } catch {
    process.stdout.write(JSON.stringify({ decision: "allow" }));
    return;
  }
  const tool = (payload && payload.tool) || {};
  void tool; // TODO: inspect tool.name / tool.input and decide.

  // {"decision":"deny","reason":"..."} blocks; {"decision":"allow"} permits.
  process.stdout.write(JSON.stringify({ decision: "allow" }));
});
"#;

const BASH_HOOK: &str = r#"#!/usr/bin/env sh
# An open-harness hook capability (scaffolded).
# CanonicalPayload in on stdin, Decision out on stdout.
payload=$(cat)
# TODO: inspect "$payload" (JSON) and decide. Example with jq:
#   cmd=$(printf '%s' "$payload" | jq -r '.tool.input.command // ""')

printf '{"decision":"allow"}\n'
"#;
