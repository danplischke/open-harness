//! The (event × harness) support matrix, generated from the adapters (#18).
//!
//! This is the single source of truth for `oh matrix` and the docs site's
//! support table — the table is **generated**, never hand-maintained, so it
//! can't drift from the adapters. A CI check compares the committed
//! `docs/harness-matrix.md` against [`markdown`].

use crate::adapters::{Harness, Support, ALL};
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
    out
}
