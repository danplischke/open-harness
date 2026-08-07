//! `oh scaffold` — capability starter templates (#18).
//!
//! Generates a ready-to-edit capability directory so an author starts from a
//! working example, not a blank file. A **hook** scaffold is *runnable* — its
//! starter reads the canonical payload on stdin and writes a decision on stdout,
//! in Python, TypeScript/Node, or bash — so `oh sync` / the dispatcher can run
//! it immediately. Generative kinds (skill / rule / command / tool / permission)
//! get a valid manifest + body skeleton that plans cleanly.

use crate::kind::KindId;
use serde_json::json;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    /// Plain Node/JavaScript (`hook.mjs`) — the direct peer to Python.
    Node,
    /// TypeScript (`hook.ts`) run through Node's built-in type stripping — typed
    /// authoring with no build step and no runtime dependency.
    TypeScript,
    Bash,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> {
        Some(match s {
            "python" | "py" => Lang::Python,
            "node" | "js" | "javascript" | "mjs" => Lang::Node,
            "typescript" | "ts" => Lang::TypeScript,
            "bash" | "sh" | "shell" => Lang::Bash,
            _ => return None,
        })
    }
}

/// Scaffold a capability `id` of `kind` into `dir/<id>/`. Returns the created
/// file paths. Refuses to overwrite an existing capability.
///
/// Document kinds (skill / agent / command / rule / instructions) scaffold as a
/// **single file** whose frontmatter is the manifest — the idiomatic authoring
/// format, no sidecar `capability.json`. Executable kinds (hook, tool) and the
/// structured permission kind scaffold a `capability.json`.
pub fn scaffold(kind: KindId, lang: Lang, id: &str, dir: &Path) -> Result<Vec<String>, String> {
    validate_id(id)?;
    let cap_dir = dir.join(id);

    let files: Vec<(String, String)> = match kind {
        KindId::Hook => hook_files(lang, id),
        KindId::Skill => vec![(
            "SKILL.md".into(),
            doc(
                &[
                    "description: TODO describe when this skill applies.",
                    "allowed-tools: [Read]",
                ],
                &format!("# {}\n\nTODO: write the skill guidance here.\n", title(id)),
            ),
        )],
        KindId::Agent => vec![(
            "AGENT.md".into(),
            doc(
                &[
                    "description: TODO when should this subagent be used?",
                    "model-tier: m",
                    "tools: [read, grep, glob]",
                ],
                &format!(
                    "# {}\n\nTODO: write the subagent's system prompt — its role, what it should do, and what it must not do.\n",
                    title(id)
                ),
            ),
        )],
        KindId::Command => vec![(
            "COMMAND.md".into(),
            doc(
                &[
                    "description: TODO what this command does.",
                    "argument-hint: <args>",
                ],
                "TODO: prompt body using {{args}} / {{arg1}}.\n",
            ),
        )],
        KindId::Rule => vec![(
            "RULE.md".into(),
            doc(
                &["activation: glob", "globs: [src/**]"],
                "TODO: write the rule guidance (applied to the globs above).\n",
            ),
        )],
        KindId::Instructions => vec![(
            // Instructions need no frontmatter — id comes from the directory,
            // kind from the filename. Just write the brief.
            "INSTRUCTIONS.md".into(),
            format!(
                "# {}\n\nTODO: write the always-on project instructions (installed as CLAUDE.md / AGENTS.md / GEMINI.md / copilot-instructions.md).\n",
                title(id)
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

    // Refuse to overwrite an existing capability (the primary file of the kind).
    let primary = cap_dir.join(&files[0].0);
    if primary.exists() {
        return Err(format!("{} already exists", primary.display()));
    }
    write_files(&cap_dir, files)
}

/// Build a single-file capability document: `---` frontmatter lines + body.
fn doc(front: &[&str], body: &str) -> String {
    let mut s = String::from("---\n");
    for line in front {
        s.push_str(line);
        s.push('\n');
    }
    s.push_str("---\n\n");
    s.push_str(body);
    s
}

/// Scaffold a TypeScript capability as an **npm package** into `dir/<id>/`:
/// `package.json` + `tsconfig.json` + a typed `src/hook.ts` + a `test.mjs` that
/// runs the capability through the core in-process via `@open-harness/node`, plus
/// `capability.json` and a README. Dev loop: `npm run build && npm test`.
pub fn scaffold_ts_project(id: &str, dir: &Path) -> Result<Vec<String>, String> {
    validate_id(id)?;
    let cap_dir = dir.join(id);
    if cap_dir.join("capability.json").exists() {
        return Err(format!(
            "{} already exists",
            cap_dir.join("capability.json").display()
        ));
    }
    let title = title(id);

    // `@open-harness/node` is intentionally NOT a dependency: it is not published
    // to npm, so listing it would make `npm install` fail with a 404. It is
    // supplied for the dev/test loop via `npm link` (see README). The built
    // capability has no runtime dependency on it.
    let package = json!({
        "name": id,
        "version": "0.1.0",
        "description": format!("An open-harness capability ({title}), authored in TypeScript."),
        "type": "module",
        "private": true,
        "scripts": { "build": "tsc", "test": "npm run build && node test.mjs" },
        "devDependencies": {
            "@types/node": "^20",
            "typescript": "^5.4.0"
        }
    });
    let tsconfig = json!({
        "compilerOptions": {
            "target": "ES2022",
            "module": "ES2022",
            "moduleResolution": "bundler",
            "outDir": "dist",
            "rootDir": "src",
            "strict": true,
            "esModuleInterop": true,
            "skipLibCheck": true,
            "declaration": false
        },
        "include": ["src"]
    });
    let capability = json!({
        "id": id,
        "name": title,
        "description": "TODO: describe what this hook guards.",
        "kind": "hook",
        "protocol": "hook@1",
        "run": { "command": "node", "args": ["dist/hook.js"] },
        "events": [ { "phase": "pre", "subject": "tool", "tool_class": "any" } ]
    });

    let files = vec![
        ("package.json".to_string(), pretty(&package)),
        ("tsconfig.json".to_string(), pretty(&tsconfig)),
        ("capability.json".to_string(), pretty(&capability)),
        ("src/hook.ts".to_string(), TS_CAPABILITY.to_string()),
        ("test.mjs".to_string(), TS_TEST.replace("__ID__", id)),
        (
            "README.md".to_string(),
            TS_README.replace("__ID__", id).replace("__TITLE__", &title),
        ),
        (
            ".gitignore".to_string(),
            "node_modules/\ndist/\n".to_string(),
        ),
    ];
    write_files(&cap_dir, files)
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "invalid capability id '{id}' (use letters, digits, '-', '_')"
        ));
    }
    Ok(())
}

fn write_files(cap_dir: &Path, files: Vec<(String, String)>) -> Result<Vec<String>, String> {
    let mut written = Vec::new();
    for (name, contents) in files {
        let path = cap_dir.join(&name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
        written.push(path.to_string_lossy().into_owned());
    }
    Ok(written)
}

fn pretty(v: &serde_json::Value) -> String {
    format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default())
}

/// The manifest + runnable starter for a hook, per language.
fn hook_files(lang: Lang, id: &str) -> Vec<(String, String)> {
    let (script_name, run, starter) = match lang {
        Lang::Python => (
            "hook.py",
            r#""run": { "command": "python3", "args": ["hook.py"] }"#,
            PY_HOOK,
        ),
        Lang::Node => (
            "hook.mjs",
            r#""run": { "command": "node", "args": ["hook.mjs"] }"#,
            NODE_HOOK,
        ),
        Lang::TypeScript => (
            "hook.ts",
            r#""run": { "command": "node", "args": ["hook.ts"] }"#,
            TS_TYPED_HOOK,
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

const NODE_HOOK: &str = r#"#!/usr/bin/env node
// An open-harness hook capability (scaffolded, plain Node/JavaScript).
// CanonicalPayload in on stdin, Decision out on stdout. No dependencies.
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

// A TypeScript hook run *directly* by Node (>= 22.18 / 23.6) via built-in type
// stripping — `node hook.ts`, no compile step, no dependencies. The types are a
// small inline subset of the `hook@1` contract so the file is self-contained;
// for the full canonical types (`CanonicalPayload` / `Decision`) plus an
// in-process test, scaffold an npm package with `oh scaffold --project`.
//
// Type stripping runs *erasable* TypeScript only: keep to type annotations,
// `interface` / `type`, and `import type` — no enums, no `namespace`.
const TS_TYPED_HOOK: &str = r#"#!/usr/bin/env node
// An open-harness hook capability, authored in TypeScript.
// Run it with `node hook.ts` — Node strips the types; there is no build step and
// no runtime dependency. CanonicalPayload in on stdin, Decision out on stdout.

interface ToolInfo {
  name: string;
  input: unknown;
}
interface CanonicalPayload {
  protocol: string;
  harness: string;
  event: { phase: string; subject: string; tool_class?: string };
  blocking: boolean;
  tool?: ToolInfo;
  prompt?: string;
  cwd?: string;
}
type Verdict = "allow" | "deny" | "modify";
interface Decision {
  decision: Verdict;
  reason?: string;
  context_append?: string;
}

async function main(): Promise<void> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(chunk as Buffer);
  const raw = Buffer.concat(chunks).toString("utf8");

  let payload: Partial<CanonicalPayload> = {};
  try {
    payload = raw.trim() ? (JSON.parse(raw) as CanonicalPayload) : {};
  } catch {
    // Unparseable input: don't veto here (the dispatcher owns error policy).
  }

  const tool = payload.tool;
  void tool; // TODO: inspect tool?.name / tool?.input and decide.

  // { decision: "deny", reason: "..." } blocks a blocking event;
  // { decision: "allow", context_append: "..." } adds model context.
  const decision: Decision = { decision: "allow" };
  process.stdout.write(JSON.stringify(decision));
}

main();
"#;

const BASH_HOOK: &str = r#"#!/usr/bin/env sh
# An open-harness hook capability (scaffolded).
# CanonicalPayload in on stdin, Decision out on stdout.
payload=$(cat)
# TODO: inspect "$payload" (JSON) and decide. Example with jq:
#   cmd=$(printf '%s' "$payload" | jq -r '.tool.input.command // ""')

printf '{"decision":"allow"}\n'
"#;

// ---- TypeScript npm-package templates -------------------------------------

const TS_CAPABILITY: &str = r#"// An open-harness hook capability, authored in TypeScript.
// Reads a CanonicalPayload as JSON on stdin, writes a Decision as JSON on stdout.
// The types come from @open-harness/node (the native addon); the built capability
// itself has no runtime dependency — it is plain Node reading stdin.
import type { CanonicalPayload, Decision } from "@open-harness/node";

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(chunk as Buffer);
  return Buffer.concat(chunks).toString("utf8");
}

const raw = await readStdin();
let payload: Partial<CanonicalPayload> = {};
try {
  payload = raw.trim() ? JSON.parse(raw) : {};
} catch {
  // Unparseable input: do not veto here (the dispatcher owns error policy).
}

const tool = payload.tool;
void tool; // TODO: inspect tool?.name / tool?.input and decide.

// { decision: "deny", reason: "..." } blocks a blocking event;
// { decision: "allow", context_append: "..." } adds model context.
const decision: Decision = { decision: "allow" };
process.stdout.write(JSON.stringify(decision));
"#;

const TS_TEST: &str = r#"// Dev test: run THIS capability through the open-harness core, in-process, via
// the native addon (@open-harness/node). Requires `npm run build` first (the
// `test` script does it) and the addon available (see README).
import { dispatch } from "@open-harness/node";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const capsDir = dirname(here); // this package is the capability dir <parent>/__ID__

const native = JSON.stringify({ tool_name: "Bash", tool_input: { command: "ls" } });
const result = dispatch("claude-code", "pre.tool.shell", native, capsDir);

console.log("exit:", result.exit_code, "· ran:", result.ran.join(", "));
if (!result.ran.includes("__ID__")) {
  console.error("FAIL: capability __ID__ did not run");
  process.exit(1);
}
console.log("ok — the capability ran and returned exit", result.exit_code);
"#;

const TS_README: &str = r#"# __TITLE__ — an open-harness capability (TypeScript)

A hook capability authored in TypeScript: it reads a `CanonicalPayload` on stdin
and writes a `Decision` on stdout. `@open-harness/node` provides the types and an
in-process test harness.

## Develop

```sh
npm install                 # typescript + @types/node
npm link @open-harness/node # types + the in-process test (see the note below)
npm run build               # tsc → dist/hook.js
npm test                    # runs this capability through the core, in-process
```

Edit `src/hook.ts` — the `// TODO` is where you inspect the tool and decide.

## Install into a project

The built capability is a normal open-harness capability (`capability.json` +
`dist/hook.js`). Compose it into a profile and install with the `oh` CLI:

```sh
oh check --capabilities ..                            # how it lands per harness
oh sync  --profile open-harness.json --into /path/to/project
```

## Note on `@open-harness/node`

This package uses `@open-harness/node` (the native addon) for its **types** and
the **in-process test** only — never at runtime. It is **not on npm**, so it is
deliberately absent from `package.json` (a hard dependency would make
`npm install` 404). Build it from the open-harness repo and link it here:

```sh
# in the open-harness repo:
bash bindings/node/build.sh
cd bindings/node && npm link
# back in this package (after `npm install`):
npm link @open-harness/node
```

When it is eventually published you can instead `npm i -D @open-harness/node`
and drop the link. Either way the **built capability has no runtime dependency**
on it — only the dev/test loop does.
"#;
