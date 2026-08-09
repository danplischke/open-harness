---
# The id defaults to the directory name; set it explicitly when they differ.
id: review-pr
name: Review PR
description: Review the current pull request for correctness and tests.
argument-hint: "[pr-number]"
---

Review pull request {{args}} for:

- correctness and edge cases
- missing or weak tests
- security issues in the diff

Focus on the changed lines. Summarize findings as a prioritized checklist.
