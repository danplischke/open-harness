---
name: Project Conventions
description: Always-on house rules, installed as CLAUDE.md / AGENTS.md / GEMINI.md / copilot-instructions.md.
version: "0.1.0"
---
# Project conventions

These are the always-on rules for working in this repository. Read them first.

## Commits

- Use [Conventional Commits](https://www.conventionalcommits.org/): `feat:`,
  `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
- Keep the subject under 72 characters; explain the *why* in the body.

## Code

- No `unwrap()` / `expect()` in library code — return a `Result` and let the
  caller decide.
- Every new behavior ships with a test. A bug fix ships with the failing test
  that now passes.

## Reviews

- Prefer the smallest change that solves the problem.
- If a change is user-visible, update the docs in the same PR.
