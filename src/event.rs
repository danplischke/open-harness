//! Normalized event model.
//!
//! The central feasibility question is whether a *single* event vocabulary can
//! faithfully represent every harness's hook lifecycle. The answer this spike
//! arrives at: a flat event name (à la `PreToolUse`) is **not** enough, because
//! Cursor and Windsurf split "before a tool runs" into per-tool-class events
//! (`beforeShellExecution`, `pre_write_code`, ...). So a normalized event is a
//! coordinate of `(phase, subject, tool_class?)`, and the mapping to a given
//! harness can be 1→1 (Claude), 1→many (Cursor fan-out) or 1→0 (no target).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Pre,
    Post,
}

/// The thing an event is *about*. This is the axis that fragments hardest across
/// harnesses, so it is modeled explicitly rather than folded into a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Tool,
    Model,
    Prompt,
    Session,
    Subagent,
    Task,
}

/// Tool sub-class. Some harnesses only ever expose a generic "tool" event
/// (`Any`); others expose one event per class. Carrying the class lets a
/// normalized `pre.tool.any` binding fan out to every class on a granular
/// harness, and lets a `pre.tool.shell` binding target exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    Any,
    Shell,
    FileRead,
    FileWrite,
    FileEdit,
    Mcp,
    Web,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    Start,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Start,
    Resume,
    Cancel,
}

/// A normalized event coordinate. Flat (rather than a tagged enum) so it
/// serializes to stable, greppable JSON and parses from a dotted CLI id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormEvent {
    pub phase: Phase,
    pub subject: SubjectKind,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_class: Option<ToolClass>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub boundary: Option<Boundary>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub task_kind: Option<TaskKind>,
}

impl NormEvent {
    pub fn tool(phase: Phase, class: ToolClass) -> Self {
        NormEvent {
            phase,
            subject: SubjectKind::Tool,
            tool_class: Some(class),
            boundary: None,
            task_kind: None,
        }
    }

    pub fn simple(phase: Phase, subject: SubjectKind) -> Self {
        NormEvent {
            phase,
            subject,
            tool_class: None,
            boundary: None,
            task_kind: None,
        }
    }

    /// Whether a capability bound here can actually *deny* the action. Only
    /// pre-phase gates on tools/model/prompt are blocking; post events and
    /// session/task lifecycle notifications cannot veto anything.
    pub fn blocking(&self) -> bool {
        matches!(self.phase, Phase::Pre)
            && matches!(
                self.subject,
                SubjectKind::Tool | SubjectKind::Model | SubjectKind::Prompt
            )
    }

    /// Stable dotted id, e.g. `pre.tool.shell`, `post.session.end`.
    pub fn id(&self) -> String {
        let mut parts = vec![
            match self.phase {
                Phase::Pre => "pre".to_string(),
                Phase::Post => "post".to_string(),
            },
            match self.subject {
                SubjectKind::Tool => "tool",
                SubjectKind::Model => "model",
                SubjectKind::Prompt => "prompt",
                SubjectKind::Session => "session",
                SubjectKind::Subagent => "subagent",
                SubjectKind::Task => "task",
            }
            .to_string(),
        ];
        if let Some(tc) = self.tool_class {
            parts.push(tool_class_str(tc).to_string());
        }
        if let Some(b) = self.boundary {
            parts.push(
                match b {
                    Boundary::Start => "start",
                    Boundary::End => "end",
                }
                .to_string(),
            );
        }
        if let Some(t) = self.task_kind {
            parts.push(
                match t {
                    TaskKind::Start => "start",
                    TaskKind::Resume => "resume",
                    TaskKind::Cancel => "cancel",
                }
                .to_string(),
            );
        }
        parts.join(".")
    }
}

pub fn tool_class_str(tc: ToolClass) -> &'static str {
    match tc {
        ToolClass::Any => "any",
        ToolClass::Shell => "shell",
        ToolClass::FileRead => "file_read",
        ToolClass::FileWrite => "file_write",
        ToolClass::FileEdit => "file_edit",
        ToolClass::Mcp => "mcp",
        ToolClass::Web => "web",
    }
}

impl fmt::Display for NormEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.id())
    }
}

impl FromStr for NormEvent {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut it = s.split('.');
        let phase = match it.next() {
            Some("pre") => Phase::Pre,
            Some("post") => Phase::Post,
            other => return Err(format!("bad phase in event id: {other:?}")),
        };
        let subject = match it.next() {
            Some("tool") => SubjectKind::Tool,
            Some("model") => SubjectKind::Model,
            Some("prompt") => SubjectKind::Prompt,
            Some("session") => SubjectKind::Session,
            Some("subagent") => SubjectKind::Subagent,
            Some("task") => SubjectKind::Task,
            other => return Err(format!("bad subject in event id: {other:?}")),
        };
        let mut ev = NormEvent::simple(phase, subject);
        if let Some(rest) = it.next() {
            match subject {
                SubjectKind::Tool => {
                    ev.tool_class = Some(match rest {
                        "any" => ToolClass::Any,
                        "shell" => ToolClass::Shell,
                        "file_read" => ToolClass::FileRead,
                        "file_write" => ToolClass::FileWrite,
                        "file_edit" => ToolClass::FileEdit,
                        "mcp" => ToolClass::Mcp,
                        "web" => ToolClass::Web,
                        o => return Err(format!("bad tool class: {o}")),
                    })
                }
                SubjectKind::Session | SubjectKind::Subagent => {
                    ev.boundary = Some(match rest {
                        "start" => Boundary::Start,
                        "end" => Boundary::End,
                        o => return Err(format!("bad boundary: {o}")),
                    })
                }
                SubjectKind::Task => {
                    ev.task_kind = Some(match rest {
                        "start" => TaskKind::Start,
                        "resume" => TaskKind::Resume,
                        "cancel" => TaskKind::Cancel,
                        o => return Err(format!("bad task kind: {o}")),
                    })
                }
                _ => {}
            }
        }
        Ok(ev)
    }
}
