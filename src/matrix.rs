//! The (event × harness) support matrix, generated from the adapters (#18).
//!
//! This is the single source of truth for `oh matrix` and the docs site's
//! support table — the table is **generated**, never hand-maintained, so it
//! can't drift from the adapters. A CI check compares the committed
//! `docs/harness-matrix.md` against [`markdown`].

use crate::adapters::{Harness, Provenance, Support, ALL};
use crate::event::{Boundary, NormEvent, Phase, SubjectKind, ToolClass};

/// A representative slice of the normalized event space — the rows of the matrix.
pub fn representative_events() -> Vec<NormEvent> {
    vec![
        NormEvent::tool(Phase::Pre, ToolClass::Any),
        NormEvent::tool(Phase::Pre, ToolClass::Shell),
        NormEvent::tool(Phase::Post, ToolClass::Any),
        NormEvent::simple(Phase::Pre, SubjectKind::Model),
        NormEvent::simple(Phase::Pre, SubjectKind::Prompt),
        NormEvent {
            phase: Phase::Pre,
            subject: SubjectKind::Session,
            tool_class: None,
            boundary: Some(Boundary::Start),
            task_kind: None,
        },
        NormEvent {
            phase: Phase::Pre,
            subject: SubjectKind::Subagent,
            tool_class: None,
            boundary: Some(Boundary::Start),
            task_kind: None,
        },
        NormEvent {
            phase: Phase::Pre,
            subject: SubjectKind::Task,
            tool_class: None,
            boundary: None,
            task_kind: Some(crate::event::TaskKind::Start),
        },
    ]
}

/// One matrix cell: how the event lands on the harness.
pub fn cell(h: Harness, ev: &NormEvent) -> String {
    match h.support(ev) {
        Support::Native(_, _) => "native".to_string(),
        Support::Fanout(list) => format!("fanout×{}", list.len()),
        Support::Unsupported(_) => "—".to_string(),
    }
}

/// Render the support matrix as a GitHub-flavored Markdown table.
pub fn markdown() -> String {
    let events = representative_events();
    let mut out = String::new();

    out.push_str("| event \\ harness |");
    for h in ALL {
        out.push_str(&format!(" {} |", h.id()));
    }
    out.push('\n');

    out.push_str("|---|");
    for _ in ALL {
        out.push_str("---|");
    }
    out.push('\n');

    for ev in &events {
        out.push_str(&format!("| `{}` |", ev.id()));
        for h in ALL {
            out.push_str(&format!(" {} |", cell(h, ev)));
        }
        out.push('\n');
    }

    out.push_str(
        "\nLegend: `native` = 1:1 · `fanout×N` = one normalized event registers on N native events · `—` = no target.\n",
    );
    out.push('\n');
    out.push_str(&provenance_markdown());
    out
}

/// The provenance report as a GitHub-flavored Markdown table.
///
/// This sits directly under the support grid rather than behind a flag: a cell
/// saying `native` means "the adapter targets a native event", which is a
/// claim about the *encoding*, not evidence that the harness was ever run. The
/// two tables together say what is supported and how well it is known.
pub fn provenance_markdown() -> String {
    let mut out = String::from("## Adapter provenance\n\n");
    out.push_str(
        "How each adapter was established. `native` above describes what the adapter targets; \
         this describes how much that is known — they are different claims.\n\n",
    );
    out.push_str("| harness | provenance | established against |\n|---|---|---|\n");
    for h in ALL {
        let p = h.provenance();
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            h.id(),
            p.label(),
            p.source()
        ));
    }
    out.push_str(&format!(
        "\nLegend: `live-captured` = a payload recorded from a real install (`oh capture`) \
         and committed as a fixture · `doc-fixture` = a fixture built from the vendor's primary \
         docs, decoded by the conformance suite, but never recorded from a live run · \
         `doc-only` = encoded from documentation, no recorded payload.\n\n\
         {} of {} adapters are backed by a recorded payload. Upgrading one means running \
         `oh capture` against a real install and committing the fixture — see \
         [`tests/fixtures/`](https://github.com/danplischke/open-harness/tree/main/tests/fixtures).\n",
        ALL.iter().filter(|h| h.provenance().has_fixture()).count(),
        ALL.len()
    ));
    out
}

/// The provenance report as plain text, for `oh matrix`.
pub fn provenance_text() -> String {
    let mut out = String::from("adapter provenance (how each adapter was established):\n");
    let width = ALL.iter().map(|h| h.id().len()).max().unwrap_or(0);
    let lwidth = ALL
        .iter()
        .map(|h| h.provenance().label().len())
        .max()
        .unwrap_or(0);
    for h in ALL {
        let p = h.provenance();
        out.push_str(&format!(
            "  {:width$}  {:lwidth$}  {}\n",
            h.id(),
            p.label(),
            p.caveat()
        ));
    }
    let backed = ALL.iter().filter(|h| h.provenance().has_fixture()).count();
    out.push_str(&format!(
        "\n{backed} of {} adapters are backed by a recorded payload; `live-captured` means it came \
         from a real\ninstall. Record one with `oh capture --harness H --event E --out FILE`.\n",
        ALL.len()
    ));
    out
}

/// Whether any target harness is unbacked by a recorded payload — used to
/// footnote `oh check` and `oh emit` rather than let the caller assume the
/// mapping was verified against the real thing.
pub fn unverified<'a>(harnesses: impl IntoIterator<Item = &'a Harness>) -> Vec<&'static str> {
    harnesses
        .into_iter()
        .filter(|h| matches!(h.provenance(), Provenance::DocOnly(_)))
        .map(|h| h.id())
        .collect()
}
