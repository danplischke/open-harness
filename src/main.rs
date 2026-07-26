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
use open_harness::kind::{kind_impl, Artifact, Installability};
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
    // The CLI is a thin shell over the stable library API (`open_harness::api`).
    let o = parse_opts(rest);
    let harness = o.harness.clone().unwrap_or_default();
    let event = o
        .event
        .clone()
        .unwrap_or_else(|| "pre.tool.any".to_string());
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    let caps_dir = o.capabilities.display().to_string();

    let result =
        open_harness::api::dispatch(&harness, &event, &buf, &caps_dir).unwrap_or_else(|e| {
            eprintln!("{e}");
            exit(2)
        });

    if o.explain {
        eprintln!(
            "[explain] harness={harness} event={event} ran={:?} skipped={:?} errored={:?} -> exit={}",
            result.ran, result.skipped, result.errored, result.exit_code
        );
    }
    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
    }
    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
    }
    exit(result.exit_code);
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
        for h in &harnesses {
            let plan = kind_impl(cap.manifest.kind).plan(cap, *h);
            match &plan.installability {
                Installability::Unsupported(reason) => println!(
                    "# capability `{}` [{}] on {} — UNSUPPORTED: {reason}\n",
                    cap.manifest.id,
                    cap.manifest.kind.as_str(),
                    h.id()
                ),
                _ => {
                    for art in &plan.artifacts {
                        match art {
                            Artifact::Registration { text } => println!("{text}"),
                            Artifact::File { path, contents } => {
                                println!("# file: {path}\n{contents}\n")
                            }
                        }
                    }
                }
            }
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
        println!(
            "capability: {} ({})  [kind: {}]",
            cap.manifest.id,
            cap.manifest.name,
            cap.manifest.kind.as_str()
        );
        let k = kind_impl(cap.manifest.kind);
        for h in ALL {
            let plan = k.plan(cap, h);
            let status = match &plan.installability {
                Installability::Clean => "installable — clean".to_string(),
                Installability::Degraded(d) => format!("installable — {d}"),
                Installability::Unsupported(r) => format!("BLOCKED — {r}"),
            };
            let note = if plan.notes.is_empty() {
                String::new()
            } else {
                format!("  [{}]", plan.notes.join("; "))
            };
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
