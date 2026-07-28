//! `oh-dispatch` — the CLI face of the spike.
//!
//! Subcommands:
//!   run     Act as a harness's single native hook entrypoint (reads native
//!           JSON on stdin, runs capabilities, writes the native response).
//!   emit    Print the native registration config for a harness (or `all`).
//!   sync    Install/converge the composed capability set into a project tree
//!           (`--into DIR`), with `--dry-run` and `--uninstall`.
//!   check   Per-capability installability, or drift vs a synced project
//!           (`--into DIR [--ci]`).
//!   matrix  The raw (event × harness) support grid — the feasibility finding.

use open_harness::adapters::{Harness, Support, ALL};
use open_harness::event::{Boundary, NormEvent, Phase, SubjectKind, ToolClass};
use open_harness::kind::{kind_impl, Artifact, Installability};
use open_harness::manifest::{discover, LoadedCapability, PermissionPolicy};
use open_harness::profile::{self, Lock, Profile, Resolved};
use open_harness::sync::{self, ApplyReport, ChangeAction, Deferred, DriftKind, DriftReport};
use open_harness::trust::{self, TrustStore};
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
        "keygen" => cmd_keygen(&rest),
        "sign" => cmd_sign(&rest),
        "verify" => cmd_verify(&rest),
        "trust" => cmd_trust(&rest),
        "resolve" => cmd_resolve(&rest),
        "sync" => cmd_sync(&rest),
        "check" => cmd_check(&rest),
        "matrix" => cmd_matrix(),
        _ => {
            eprintln!(
                "oh-dispatch <run|emit|keygen|sign|verify|trust|resolve|sync|check|matrix> \\\n  [--harness H] [--event E] [--capabilities DIR] [--profile FILE] [--into DIR]\\\n  [--key FILE] [--trust FILE] [--label L] [--out FILE] [--require-signed] [--deny-network] [--deny-exec]\\\n  [--dry-run] [--uninstall] [--ci] [--timeout-ms N] [--max-output-kb N] [--explain]"
            );
            exit(2);
        }
    }
}

struct Opts {
    harness: Option<String>,
    event: Option<String>,
    capabilities: PathBuf,
    profile: Option<PathBuf>,
    into: Option<PathBuf>,
    dry_run: bool,
    uninstall: bool,
    ci: bool,
    explain: bool,
    timeout_ms: u64,
    max_output_bytes: u64,
    // trust & signing (#17)
    capability_one: Option<PathBuf>,
    key: Option<PathBuf>,
    trust: Option<PathBuf>,
    label: Option<String>,
    out: Option<PathBuf>,
    require_signed: bool,
    deny_network: bool,
    deny_exec: bool,
}

fn parse_opts(rest: &[String]) -> Opts {
    let mut o = Opts {
        harness: None,
        event: None,
        capabilities: PathBuf::from("capabilities"),
        profile: None,
        into: None,
        dry_run: false,
        uninstall: false,
        ci: false,
        explain: false,
        timeout_ms: 0,
        max_output_bytes: 0,
        capability_one: None,
        key: None,
        trust: None,
        label: None,
        out: None,
        require_signed: false,
        deny_network: false,
        deny_exec: false,
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
            "--profile" => {
                o.profile = rest.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--capability" => {
                o.capability_one = rest.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--key" => {
                o.key = rest.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--trust" => {
                o.trust = rest.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--label" => {
                o.label = rest.get(i + 1).cloned();
                i += 2;
            }
            "--out" => {
                o.out = rest.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--require-signed" => {
                o.require_signed = true;
                i += 1;
            }
            "--deny-network" => {
                o.deny_network = true;
                i += 1;
            }
            "--deny-exec" => {
                o.deny_exec = true;
                i += 1;
            }
            "--timeout-ms" => {
                o.timeout_ms = rest.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
                i += 2;
            }
            "--max-output-kb" => {
                o.max_output_bytes = rest
                    .get(i + 1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|kb| kb * 1024)
                    .unwrap_or(0);
                i += 2;
            }
            "--into" => {
                o.into = rest.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--dry-run" => {
                o.dry_run = true;
                i += 1;
            }
            "--uninstall" => {
                o.uninstall = true;
                i += 1;
            }
            "--ci" => {
                o.ci = true;
                i += 1;
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

/// The target harness set for the generative commands (`emit`/`sync`/`check`):
/// a single `--harness`, or every harness (`all` / unset).
fn select_harnesses(o: &Opts) -> Vec<Harness> {
    match o.harness.as_deref() {
        None | Some("all") => ALL.to_vec(),
        Some(id) => vec![Harness::from_id(id).unwrap_or_else(|| {
            eprintln!("unknown harness {id}");
            exit(2)
        })],
    }
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

    let result = open_harness::api::dispatch_with_limits(
        &harness,
        &event,
        &buf,
        &caps_dir,
        o.timeout_ms,
        o.max_output_bytes,
    )
    .unwrap_or_else(|e| {
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
    let harnesses = select_harnesses(&o);
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

// ---- trust & signing (#17) -------------------------------------------------

fn cmd_keygen(rest: &[String]) {
    let o = parse_opts(rest);
    let out = o
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("openharness.key"));
    let label = o.label.clone().unwrap_or_default();
    let key = trust::generate_keyfile(&label).unwrap_or_else(|e| {
        eprintln!("keygen failed: {e}");
        exit(1);
    });
    key.write(&out).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1);
    });
    println!("wrote {} (ed25519)", out.display());
    println!("  public key : {}", key.public_key);
    println!("  fingerprint: {}", trust::fingerprint(&key.public_key));
    eprintln!(
        "keep the secret key private — do not commit {}",
        out.display()
    );
}

fn cmd_sign(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(dir) = o.capability_one.clone() else {
        eprintln!("sign requires --capability DIR");
        exit(2);
    };
    let Some(keyf) = o.key.clone() else {
        eprintln!("sign requires --key FILE");
        exit(2);
    };
    let key = trust::Keyfile::load(&keyf).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1);
    });
    let sig = trust::sign(&dir, &key).unwrap_or_else(|e| {
        eprintln!("sign failed: {e}");
        exit(1);
    });
    sig.write(&dir).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1);
    });
    println!("signed {}", dir.display());
    println!("  digest: {}", sig.digest);
    println!("  signer: {}", trust::fingerprint(&key.public_key));
}

fn cmd_verify(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(dir) = o.capability_one.clone() else {
        eprintln!("verify requires --capability DIR");
        exit(2);
    };
    let store = load_trust(&o);
    let v = trust::verify(&dir, &store);
    println!("{}: {}", dir.display(), v.status());
    if let Ok(cap) = LoadedCapability::load(&dir.join("capability.json")) {
        if !cap.manifest.permissions.is_empty() {
            println!("  permissions: {}", cap.manifest.permissions.summary());
        }
    }
    if !v.passes(o.require_signed) {
        exit(1);
    }
}

fn cmd_trust(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(dir) = o.capability_one.clone() else {
        eprintln!("trust requires --capability DIR (whose signer to trust)");
        exit(2);
    };
    let Some(sig) = trust::Signature::load(&dir) else {
        eprintln!("no {} in {}", trust::SIG_NAME, dir.display());
        exit(1);
    };
    let trust_path = o
        .trust
        .clone()
        .unwrap_or_else(|| PathBuf::from("trust.json"));
    let mut store = TrustStore::load(&trust_path).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1);
    });
    let label = o.label.clone().unwrap_or_default();
    let fp = trust::fingerprint(&sig.public_key);
    if store.add(&sig.public_key, &label) {
        store.write(&trust_path).unwrap_or_else(|e| {
            eprintln!("{e}");
            exit(1);
        });
        println!("trusted {fp} → {}", trust_path.display());
    } else {
        println!("already trusted: {fp}");
    }
}

fn load_trust(o: &Opts) -> TrustStore {
    match &o.trust {
        Some(p) => TrustStore::load(p).unwrap_or_else(|e| {
            eprintln!("{e}");
            exit(1);
        }),
        None => TrustStore::default(),
    }
}

/// Whether the user opted into trust/permission checking on this invocation.
fn trust_requested(o: &Opts) -> bool {
    o.trust.is_some() || o.require_signed || o.deny_network || o.deny_exec
}

/// Verify a composed set, surface permission manifests, and gate. Only called
/// when the user opts in (a trust store, `--require-signed`, or a deny flag).
fn enforce_trust(caps: &[LoadedCapability], o: &Opts) {
    let store = load_trust(o);
    let policy = PermissionPolicy {
        allow_network: !o.deny_network,
        allow_exec: !o.deny_exec,
    };
    println!(
        "trust check ({}):",
        if o.require_signed {
            "require-signed"
        } else {
            "advisory"
        }
    );
    let mut failed = false;
    for cap in caps {
        let v = trust::verify(&cap.dir, &store);
        let perms = &cap.manifest.permissions;
        let extra = if perms.is_empty() {
            String::new()
        } else {
            format!("  [perms: {}]", perms.summary())
        };
        println!("  {:<16} {}{extra}", cap.manifest.id, v.status());
        if !v.passes(o.require_signed) {
            eprintln!("    ✗ rejected ({})", v.status());
            failed = true;
        }
        for viol in trust::permission_violations(perms, &policy) {
            if o.require_signed {
                eprintln!("    ✗ permission: {viol}");
                failed = true;
            } else {
                eprintln!("    ⚠ permission: {viol}");
            }
        }
    }
    if failed {
        eprintln!("\ntrust/permission gate failed");
        exit(1);
    }
}

/// Resolve a profile file: load it (and any lockfile next to it), resolve every
/// source, rewrite the lockfile, and print the composed set + warnings. The
/// profile file's directory is the workdir (roots relative paths + the git cache).
fn resolve_profile(path: &std::path::Path) -> Resolved {
    let profile = Profile::load(path).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(2);
    });
    let workdir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let lock_path = workdir.join(profile::LOCK_NAME);
    let existing = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|t| Lock::from_json(&t).ok());

    let resolved = profile::resolve(&profile, workdir, existing.as_ref()).unwrap_or_else(|e| {
        eprintln!("resolve failed: {e}");
        exit(1);
    });
    if let Err(e) = resolved.lock.write(&lock_path) {
        eprintln!("could not write {}: {e}", lock_path.display());
    }
    for w in &resolved.warnings {
        eprintln!("warning: {w}");
    }
    resolved
}

fn cmd_resolve(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(profile_path) = o.profile.clone() else {
        eprintln!("resolve requires --profile <file>");
        exit(2);
    };
    let resolved = resolve_profile(&profile_path);
    let workdir = profile_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    println!(
        "profile '{}' → {} capabilities across {} harness(es)",
        resolved.profile,
        resolved.capabilities.len(),
        resolved.harnesses.len()
    );
    for s in &resolved.lock.sources {
        let at = s
            .rev
            .as_deref()
            .map(|r| &r[..r.len().min(12)])
            .unwrap_or("-");
        println!(
            "  [{}] {} @ {}  ({} capabilities)",
            s.kind,
            s.origin,
            at,
            s.capabilities.len()
        );
        for c in &s.capabilities {
            println!("      · {} {} [{}]", c.id, c.version, c.kind);
        }
    }
    println!("\nlockfile: {}", workdir.join(profile::LOCK_NAME).display());

    if trust_requested(&o) {
        println!();
        enforce_trust(&resolved.capabilities, &o);
    }
}

fn cmd_sync(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(into) = o.into.clone() else {
        eprintln!("sync requires --into <project-dir>");
        exit(2);
    };

    // Uninstall short-circuits: it reads the lockfile, not the capabilities.
    if o.uninstall {
        let report = sync::uninstall(&into, o.dry_run).unwrap_or_else(|e| {
            eprintln!("uninstall failed: {e}");
            exit(1);
        });
        print_apply_report(&report, "uninstall", &into);
        return;
    }

    // Profile mode (#15): resolve sources + harnesses from the profile; else the
    // ad-hoc `--capabilities` / `--harness` mode.
    let (caps, harnesses) = if let Some(profile_path) = o.profile.clone() {
        let resolved = resolve_profile(&profile_path);
        if resolved.harnesses.is_empty() {
            eprintln!("profile '{}' targets no harnesses", resolved.profile);
            exit(1);
        }
        (resolved.capabilities, resolved.harnesses)
    } else {
        let caps = load_caps(&o);
        if caps.is_empty() {
            eprintln!("no capabilities found under {}", o.capabilities.display());
            exit(1);
        }
        (caps, select_harnesses(&o))
    };

    // Trust/permission gate (#17) — opt-in; a tampered/rejected capability here
    // aborts the install before anything is written.
    if trust_requested(&o) {
        enforce_trust(&caps, &o);
        println!();
    }

    let plan = sync::plan_sync(&caps, &harnesses);
    let report = sync::apply(&into, &plan, o.dry_run).unwrap_or_else(|e| {
        eprintln!("sync failed: {e}");
        exit(1);
    });
    print_apply_report(&report, "sync", &into);
}

fn cmd_check(rest: &[String]) {
    let o = parse_opts(rest);

    // With `--into`, `check` is drift detection against a synced project.
    if let Some(into) = o.into.clone() {
        let caps = load_caps(&o);
        if caps.is_empty() {
            eprintln!("no capabilities found under {}", o.capabilities.display());
            exit(1);
        }
        let harnesses = select_harnesses(&o);
        let plan = sync::plan_sync(&caps, &harnesses);
        let report = sync::check(&into, &plan).unwrap_or_else(|e| {
            eprintln!("check failed: {e}");
            exit(1);
        });
        print_drift_report(&report, &into);
        if o.ci && report.has_drift() {
            eprintln!("\ndrift detected (--ci): exiting non-zero");
            exit(1);
        }
        return;
    }

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

// ---- sync / check reporting ----------------------------------------------

fn print_apply_report(report: &ApplyReport, verb: &str, into: &std::path::Path) {
    let tag = if report.dry_run { " (dry-run)" } else { "" };
    println!("{verb}{tag} → {}", into.display());
    for c in &report.changes {
        let mark = match c.action {
            ChangeAction::Create => "＋ create   ",
            ChangeAction::Update => "~ update   ",
            ChangeAction::Unchanged => "· unchanged",
            ChangeAction::Prune => "－ prune    ",
        };
        println!(
            "  {mark} {}{}",
            c.path,
            provenance(&c.harnesses, &c.sources)
        );
    }
    if report.changes.is_empty() {
        println!("  (no managed files)");
    }
    println!(
        "\n  {} created, {} updated, {} unchanged, {} pruned",
        report.count(ChangeAction::Create),
        report.count(ChangeAction::Update),
        report.count(ChangeAction::Unchanged),
        report.count(ChangeAction::Prune),
    );
    if !report.conflicts.is_empty() {
        println!("\n  composition conflicts resolved:");
        for (_path, msgs) in &report.conflicts {
            for m in msgs {
                println!("    ⚠ {m}");
            }
        }
    }
    print_deferred(&report.deferred);
}

fn print_drift_report(report: &DriftReport, into: &std::path::Path) {
    println!("drift check → {}", into.display());
    for i in &report.items {
        let mark = match i.kind {
            DriftKind::Ok => "· ok      ",
            DriftKind::Missing => "✗ missing ",
            DriftKind::Modified => "✗ modified",
            DriftKind::Stale => "✗ stale   ",
        };
        println!(
            "  {mark} {}{}",
            i.path,
            provenance(&i.harnesses, &i.sources)
        );
    }
    if report.items.is_empty() {
        println!("  (nothing planned or managed)");
    }
    println!(
        "\n  {} ok, {} missing, {} modified, {} stale",
        report.count(DriftKind::Ok),
        report.count(DriftKind::Missing),
        report.count(DriftKind::Modified),
        report.count(DriftKind::Stale),
    );
    print_deferred(&report.deferred);
    println!(
        "\n  {}",
        if report.has_drift() {
            "DRIFT"
        } else {
            "in sync"
        }
    );
}

/// The deferred/blocked artifacts, grouped by reason — always shown so nothing
/// looks silently dropped.
fn print_deferred(deferred: &[Deferred]) {
    if deferred.is_empty() {
        return;
    }
    let mut registration = Vec::new();
    let mut global = Vec::new();
    let mut blocked = Vec::new();
    for d in deferred {
        match d.reason {
            sync::DeferReason::Registration => registration.push(d),
            sync::DeferReason::GlobalPath => global.push(d),
            sync::DeferReason::Unsupported => blocked.push(d),
        }
    }
    if !registration.is_empty() {
        println!(
            "\n  manual: {} hook registration(s) to wire into a native entrypoint (see `emit`)",
            registration.len()
        );
        for d in &registration {
            println!("    → {} on {} [{}]", d.source, d.harness, d.kind);
        }
    }
    if !global.is_empty() {
        println!(
            "\n  manual: {} global-path target(s) outside the project",
            global.len()
        );
        for d in &global {
            println!("    → {} on {}: {}", d.source, d.harness, d.detail);
        }
    }
    if !blocked.is_empty() {
        println!("\n  blocked: {} unsupported target(s)", blocked.len());
        for d in &blocked {
            println!("    → {} on {}: {}", d.source, d.harness, d.detail);
        }
    }
}

fn provenance(harnesses: &[String], sources: &[String]) -> String {
    if harnesses.is_empty() && sources.is_empty() {
        return String::new();
    }
    format!("  [{} · {}]", harnesses.join(","), sources.join("+"))
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
