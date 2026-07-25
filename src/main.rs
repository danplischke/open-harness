//! `oh-dispatch` — the CLI face of the spike.
//!
//! Subcommands:
//!   run     Act as a harness's single native hook entrypoint (reads native
//!           JSON on stdin, runs capabilities, writes the native response).
//!   emit    Print the native registration config for a harness (or `all`).
//!   check   Per-capability installability across every harness.
//!   matrix  The raw (event × harness) support grid — the feasibility finding.

use open_harness::adapters::{Harness, Support, ALL};
use open_harness::event::{Boundary, NormEvent, Phase, SubjectKind, ToolClass};
use open_harness::manifest::{discover, LoadedCapability};
use std::io::Read;
use std::path::PathBuf;
use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    match cmd {
        "run" => cmd_run(&rest),
        "emit" => cmd_emit(&rest),
        "check" => cmd_check(&rest),
        "matrix" => cmd_matrix(),
        _ => {
            eprintln!(
                "oh-dispatch <run|emit|check|matrix> [--harness H] [--event E] [--capabilities DIR] [--explain]"
            );
            exit(2);
        }
    }
}

struct Opts {
    harness: Option<String>,
    event: Option<String>,
    capabilities: PathBuf,
    explain: bool,
}

fn parse_opts(rest: &[String]) -> Opts {
    let mut o = Opts {
        harness: None,
        event: None,
        capabilities: PathBuf::from("capabilities"),
        explain: false,
    };
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--harness" => {
                o.harness = rest.get(i + 1).cloned();
                i += 2;
            }
            "--event" => {
                o.event = rest.get(i + 1).cloned();
                i += 2;
            }
            "--capabilities" => {
                if let Some(p) = rest.get(i + 1) {
                    o.capabilities = PathBuf::from(p);
                }
                i += 2;
            }
            "--explain" => {
                o.explain = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    o
}

fn load_caps(o: &Opts) -> Vec<LoadedCapability> {
    match discover(&o.capabilities) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "could not load capabilities from {}: {e}",
                o.capabilities.display()
            );
            Vec::new()
        }
    }
}

fn cmd_run(rest: &[String]) {
    let o = parse_opts(rest);
    let harness = o
        .harness
        .as_deref()
        .and_then(Harness::from_id)
        .unwrap_or_else(|| {
            eprintln!("--harness required (e.g. claude-code, codex, cursor, ...)");
            exit(2)
        });
    let ev: NormEvent = o
        .event
        .as_deref()
        .unwrap_or("pre.tool.any")
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("bad --event: {e}");
            exit(2)
        });

    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let native: serde_json::Value = if buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&buf).unwrap_or_else(|e| {
            eprintln!("stdin was not valid JSON: {e}");
            exit(2)
        })
    };

    let caps = load_caps(&o);
    let outcome = open_harness::dispatch(harness, &ev, &caps, &native);

    if o.explain {
        eprintln!(
            "[explain] harness={} event={} ran={:?} skipped={:?} errored={:?} -> exit={}",
            harness.id(),
            ev.id(),
            outcome.ran,
            outcome.skipped,
            outcome.errored,
            outcome.response.exit_code
        );
    }
    if !outcome.response.stdout.is_empty() {
        print!("{}", outcome.response.stdout);
    }
    if !outcome.response.stderr.is_empty() {
        eprint!("{}", outcome.response.stderr);
    }
    exit(outcome.response.exit_code);
}

fn cmd_emit(rest: &[String]) {
    let o = parse_opts(rest);
    let caps = load_caps(&o);
    if caps.is_empty() {
        eprintln!("no capabilities found under {}", o.capabilities.display());
        exit(1);
    }
    let harnesses: Vec<Harness> = match o.harness.as_deref() {
        None | Some("all") => ALL.to_vec(),
        Some(id) => vec![Harness::from_id(id).unwrap_or_else(|| {
            eprintln!("unknown harness {id}");
            exit(2)
        })],
    };
    for cap in &caps {
        let events: Vec<NormEvent> = cap.manifest.events.iter().map(|b| b.event()).collect();
        for h in &harnesses {
            println!("{}", h.emit_registration(&events, &cap.manifest.id));
        }
    }
}

fn cmd_check(rest: &[String]) {
    let o = parse_opts(rest);
    let caps = load_caps(&o);
    if caps.is_empty() {
        eprintln!("no capabilities found under {}", o.capabilities.display());
        exit(1);
    }
    for cap in &caps {
        println!("capability: {} ({})", cap.manifest.id, cap.manifest.name);
        for h in ALL {
            let mut required_gaps = Vec::new();
            let mut optional_gaps = Vec::new();
            let mut fanouts = Vec::new();
            for b in &cap.manifest.events {
                let ev = b.event();
                match h.support(&ev) {
                    Support::Native(_, _) => {}
                    Support::Fanout(list) => fanouts.push((ev.id(), list.len())),
                    Support::Unsupported(reason) => {
                        if b.required {
                            required_gaps.push(format!("{} ({reason})", ev.id()));
                        } else {
                            optional_gaps.push(ev.id());
                        }
                    }
                }
            }
            let status = if !required_gaps.is_empty() {
                format!("BLOCKED — missing required: {}", required_gaps.join(", "))
            } else if !optional_gaps.is_empty() || !fanouts.is_empty() {
                let mut parts = Vec::new();
                if !fanouts.is_empty() {
                    let f: Vec<String> = fanouts
                        .iter()
                        .map(|(e, n)| format!("{e}→{n} native events"))
                        .collect();
                    parts.push(format!("fan-out: {}", f.join(", ")));
                }
                if !optional_gaps.is_empty() {
                    parts.push(format!(
                        "degraded (skipped optional: {})",
                        optional_gaps.join(", ")
                    ));
                }
                format!("installable — {}", parts.join("; "))
            } else {
                "installable — clean".to_string()
            };
            let note = h
                .platform_note()
                .map(|n| format!("  [{n}]"))
                .unwrap_or_default();
            println!("  {:<12} {}{}", h.id(), status, note);
        }
        println!();
    }
}

fn cmd_matrix() {
    let events = representative_events();
    let width = 13;
    print!("{:<18}", "event \\ harness");
    for h in ALL {
        print!("{:<width$}", short(h.id()), width = width);
    }
    println!();
    println!("{}", "-".repeat(18 + width * ALL.len()));
    for ev in &events {
        print!("{:<18}", ev.id());
        for h in ALL {
            let cell = match h.support(ev) {
                Support::Native(_, _) => "native".to_string(),
                Support::Fanout(list) => format!("fanout×{}", list.len()),
                Support::Unsupported(_) => "—".to_string(),
            };
            print!("{:<width$}", cell, width = width);
        }
        println!();
    }
    println!("\nlegend: native = 1:1 · fanout×N = one normalized event registers on N native events · — = no target");
}

fn short(id: &str) -> String {
    match id {
        "claude-code" => "claude".to_string(),
        other => other.chars().take(11).collect(),
    }
}

fn representative_events() -> Vec<NormEvent> {
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
            task_kind: Some(open_harness::event::TaskKind::Start),
        },
    ]
}
