# Introduction

Different coding harnesses — Claude Code, Codex, Gemini CLI, Cursor, Windsurf,
Cline, OpenCode, Pi, Aider, Copilot — configure the same concepts (hooks, rules,
commands, tools, skills, permissions) in mutually incompatible ways. Today an OSS
author who wants their setup to work everywhere has to maintain a different
abstraction for each harness.

**open-harness** is the missing integration layer. An author defines a capability
**once**; open-harness fans it out to each harness's native config. A developer
**composes** capabilities from different sources — OSS and their own repos — and
the customization is **programming-language-agnostic**.

It stands *beside* the existing standards — AGENTS.md (instructions), MCP
(tools), SKILL.md (skills) — and owns the un-standardized middle.

## The `oh` CLI at a glance

```sh
oh init                                   # write an open-harness.yaml profile
oh scaffold --kind hook --lang python --id my-guard   # a runnable capability starter
oh doctor                                 # check interpreters + capability health
oh matrix                                 # the (event × harness) support grid
oh sync --profile open-harness.yaml --into .          # install into a project
oh check --into . --ci                    # drift detection (fails CI on drift)
```

## Honest by design

open-harness never emits a lowest-common-denominator config. Where a harness
can't express a concept, that is **declared and surfaced** — a loud "degraded"
note or an "unsupported" reason — never silently dropped. The
[support matrix](./harness-matrix.md) (generated from the adapters) is the map of
exactly where each concept lands and where it breaks.
