//! `oh` — the unified CLI.
//!
//! Authoring:  `init` (write a profile) · `scaffold` (a runnable capability
//!   starter) · `add`/`remove` (edit a profile's sources) · `migrate` (rewrite
//!   legacy JSON configs as YAML) · `install --runtimes` (run capabilities'
//!   declared provisioning commands, confirmed) · `doctor` (check the
//!   environment + capability health).
//! Runtime:    `run` (a harness's single native hook entrypoint) · `mcp`
//!   (list/call an MCP server's tools through the shell — the MCP→CLI bridge).
//! Delivery:   `emit` (native registration config) · `try` (what a source would
//!   install, writing nothing) · `resolve` (sources → lock) · `sync`
//!   (install/converge into a project) · `check` (installability, or drift vs a
//!   synced project with `--ci`).
//! Trust:      `keygen` · `sign` · `verify` · `trust` · `revoke` · `keyring`
//!   (a root of trust: `keyring` + `trust --root`/`--keyring`).
//! Publishing: `pack` (capabilities → content-addressed archives + a registry
//!   index, ready to upload to a CDN).
//! Reporting:  `matrix` (the event × harness support grid; `--markdown`).

use open_harness::adapters::{Harness, ALL};
use open_harness::help;
use open_harness::importer;
use open_harness::kind::{kind_impl, Artifact, Installability, KindId};
use open_harness::manifest::{discover, LoadedCapability, PermissionPolicy};
use open_harness::mcp::{self, HttpServerSpec, McpServer, ServerSpec};
use open_harness::profile::{self, Lock, Profile, Resolved};
use open_harness::publish;
use open_harness::scaffold::{self, Lang};
use open_harness::sync::{self, ApplyReport, ChangeAction, Deferred, DriftKind, DriftReport};
use open_harness::trust::{self, TrustStore};
use std::io::Read;
use std::path::PathBuf;
use std::process::exit;

/// Restore the default `SIGPIPE` disposition.
///
/// Rust sets `SIGPIPE` to `SIG_IGN` before `main`, so a closed downstream pipe
/// surfaces as an `EPIPE` write error — and `println!` panics on those. That
/// turns the most ordinary shell idioms this tool invites (`oh check | head`,
/// `oh matrix | less` and quitting, `oh emit | grep -m1`) into a Rust backtrace
/// and exit 101. Restoring `SIG_DFL` makes the process die quietly on the
/// signal, which is what every other CLI in the pipeline does.
///
/// Declared directly rather than pulling in `libc` for one call: `SIGPIPE` is
/// 13 and `SIG_DFL` is 0 on every Unix this builds for, and both are fixed by
/// POSIX. Windows has no signals and needs nothing.
#[cfg(unix)]
fn restore_sigpipe() {
    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    // Safety: `signal` with SIG_DFL is async-signal-safe and this runs before
    // any thread is spawned.
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_sigpipe() {}

fn main() {
    restore_sigpipe();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();

    // Asking for help is not an error: it prints to stdout and exits 0. `oh`
    // with no arguments lands here too — a bare invocation should teach, not
    // scold.
    if matches!(cmd, "help" | "--help" | "-h") {
        match rest.first() {
            None => print!("{}", help::overview()),
            Some(name) => match help::command(name) {
                Some(c) => print!("{}", help::command_help(c)),
                None => unknown_command(name),
            },
        }
        return;
    }
    // The `oh <cmd> --help` form, handled once here instead of in all 27
    // subcommands. It must win over the flag parser, which would otherwise
    // reject `--help` as an unknown option.
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        if let Some(c) = help::command(cmd) {
            print!("{}", help::command_help(c));
            return;
        }
    }

    match cmd {
        "run" => cmd_run(&rest),
        "emit" => cmd_emit(&rest),
        "keygen" => cmd_keygen(&rest),
        "sign" => cmd_sign(&rest),
        "pack" => cmd_pack(&rest),
        "verify" => cmd_verify(&rest),
        "trust" => cmd_trust(&rest),
        "revoke" => cmd_revoke(&rest),
        "keyring" => cmd_keyring(&rest),
        "mcp" => cmd_mcp(&rest),
        "resolve" => cmd_resolve(&rest),
        "try" => cmd_try(&rest),
        "sync" => cmd_sync(&rest),
        "check" => cmd_check(&rest),
        "matrix" => cmd_matrix(&rest),
        "version" | "--version" | "-V" => cmd_version(),
        "scaffold" => cmd_scaffold(&rest),
        "capture" => cmd_capture(&rest),
        "init" => cmd_init(&rest),
        "migrate" => cmd_migrate(&rest),
        "install" => cmd_install(&rest),
        "doctor" => cmd_doctor(&rest),
        "add" => cmd_add(&rest),
        "remove" => cmd_remove(&rest),
        "import" => cmd_import(&rest),
        "completions" => cmd_completions(&rest),
        other => unknown_command(other),
    }
}

/// A mistyped command should point at the one meant. Exits 2 — an unknown
/// command is a usage error, unlike an explicit `--help`.
fn unknown_command(name: &str) -> ! {
    eprintln!("unknown command `{name}`");
    if let Some(s) = help::suggest(name) {
        eprintln!("did you mean `oh {s}`?");
    }
    eprintln!("\nrun `oh help` for the command list");
    exit(2)
}

fn cmd_completions(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(shell) = o.positional.first() else {
        eprintln!("completions wants a shell: bash, zsh or fish");
        exit(2);
    };
    match help::Shell::parse(shell) {
        Some(s) => print!("{}", help::completions(s)),
        None => {
            eprintln!("unknown shell `{shell}` — expected bash, zsh or fish");
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
    /// Treat the lockfile as a contract: never rewrite it, fail if it is stale.
    locked: bool,
    /// Confirm an action that executes code (`install --runtimes`).
    yes: bool,
    /// `sync --wire`: write hook registrations into the harness's own config
    /// instead of reporting them as a manual step.
    wire: bool,
    /// `sync`/`check` at **user** scope: `~/.claude/skills/…` instead of the
    /// project's, so a capability applies to every project on this machine.
    global: bool,
    /// `install --runtimes`: provision capabilities' declared dependencies.
    runtimes: bool,
    /// `matrix --markdown`: emit the support grid as a Markdown table.
    markdown: bool,
    /// `matrix --provenance`: how each adapter was established, not just what
    /// it supports.
    provenance: bool,
    explain: bool,
    timeout_ms: u64,
    max_output_bytes: u64,
    // trust & signing (#17)
    capability_one: Option<PathBuf>,
    key: Option<PathBuf>,
    trust: Option<PathBuf>,
    label: Option<String>,
    reason: Option<String>,
    out: Option<PathBuf>,
    require_signed: bool,
    // root of trust (#21)
    root: bool,
    keyring: Option<PathBuf>,
    deny_network: bool,
    deny_exec: bool,
    deny_write: bool,
    // MCP→CLI bridge (#19, #20)
    id: Option<String>,
    mcp_command: Option<String>,
    mcp_args: Vec<String>,
    mcp_url: Option<String>,
    mcp_headers: Vec<(String, String)>,
    tool: Option<String>,
    json_args: Option<String>,
    // authoring (#18)
    scaffold_kind: Option<String>,
    scaffold_lang: Option<String>,
    scaffold_project: bool,
    local: Option<String>,
    git: Option<String>,
    /// Bare arguments (a source spec for `add`, a subcommand for `mcp`).
    positional: Vec<String>,
    // publishing (#21)
    base_url: Option<String>,
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
        locked: false,
        yes: false,
        global: false,
        wire: false,
        runtimes: false,
        markdown: false,
        provenance: false,
        explain: false,
        timeout_ms: 0,
        max_output_bytes: 0,
        capability_one: None,
        key: None,
        trust: None,
        label: None,
        reason: None,
        out: None,
        require_signed: false,
        root: false,
        keyring: None,
        deny_network: false,
        deny_exec: false,
        deny_write: false,
        id: None,
        mcp_command: None,
        mcp_args: Vec::new(),
        mcp_url: None,
        mcp_headers: Vec::new(),
        tool: None,
        json_args: None,
        scaffold_kind: None,
        scaffold_lang: None,
        scaffold_project: false,
        local: None,
        git: None,
        positional: Vec::new(),
        base_url: None,
    };
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--harness" => {
                o.harness = Some(value_for(rest, i));
                i += 2;
            }
            "--event" => {
                o.event = Some(value_for(rest, i));
                i += 2;
            }
            "--capabilities" => {
                o.capabilities = PathBuf::from(value_for(rest, i));
                i += 2;
            }
            "--profile" => {
                o.profile = Some(PathBuf::from(value_for(rest, i)));
                i += 2;
            }
            "--capability" => {
                o.capability_one = Some(PathBuf::from(value_for(rest, i)));
                i += 2;
            }
            "--key" => {
                o.key = Some(PathBuf::from(value_for(rest, i)));
                i += 2;
            }
            "--trust" => {
                o.trust = Some(PathBuf::from(value_for(rest, i)));
                i += 2;
            }
            "--label" => {
                o.label = Some(value_for(rest, i));
                i += 2;
            }
            "--reason" => {
                o.reason = Some(value_for(rest, i));
                i += 2;
            }
            "--out" => {
                o.out = Some(PathBuf::from(value_for(rest, i)));
                i += 2;
            }
            "--require-signed" => {
                o.require_signed = true;
                i += 1;
            }
            "--root" => {
                o.root = true;
                i += 1;
            }
            "--keyring" => {
                o.keyring = Some(PathBuf::from(value_for(rest, i)));
                i += 2;
            }
            "--deny-network" => {
                o.deny_network = true;
                i += 1;
            }
            "--deny-exec" => {
                o.deny_exec = true;
                i += 1;
            }
            "--deny-write" => {
                o.deny_write = true;
                i += 1;
            }
            "--id" => {
                o.id = Some(value_for(rest, i));
                i += 2;
            }
            "--command" => {
                o.mcp_command = Some(value_for(rest, i));
                i += 2;
            }
            "--mcp-arg" => {
                o.mcp_args.push(value_for(rest, i));
                i += 2;
            }
            "--url" => {
                o.mcp_url = Some(value_for(rest, i));
                i += 2;
            }
            "--base-url" => {
                o.base_url = Some(value_for(rest, i));
                i += 2;
            }
            "--header" => {
                // `--header "Name: value"` (repeatable), for http MCP endpoints.
                let h = value_for(rest, i);
                match h.split_once(':') {
                    Some((k, v)) => o
                        .mcp_headers
                        .push((k.trim().to_string(), v.trim().to_string())),
                    // Silently dropping it would send the request without the
                    // header the user asked for — an auth header, usually.
                    None => {
                        eprintln!("`--header` wants \"Name: value\", got `{h}`");
                        exit(2);
                    }
                }
                i += 2;
            }
            "--tool" => {
                o.tool = Some(value_for(rest, i));
                i += 2;
            }
            "--json" => {
                o.json_args = Some(value_for(rest, i));
                i += 2;
            }
            "--kind" => {
                o.scaffold_kind = Some(value_for(rest, i));
                i += 2;
            }
            "--lang" => {
                o.scaffold_lang = Some(value_for(rest, i));
                i += 2;
            }
            "--project" => {
                o.scaffold_project = true;
                i += 1;
            }
            "--local" => {
                o.local = Some(value_for(rest, i));
                i += 2;
            }
            "--git" => {
                o.git = Some(value_for(rest, i));
                i += 2;
            }
            "--timeout-ms" => {
                o.timeout_ms = numeric_value(rest, i);
                i += 2;
            }
            "--max-output-kb" => {
                o.max_output_bytes = numeric_value(rest, i) * 1024;
                i += 2;
            }
            "--into" => {
                o.into = Some(PathBuf::from(value_for(rest, i)));
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
            "--locked" => {
                o.locked = true;
                i += 1;
            }
            "--yes" | "-y" => {
                o.yes = true;
                i += 1;
            }
            "--ci" => {
                o.ci = true;
                i += 1;
            }
            "--global" => {
                o.global = true;
                i += 1;
            }
            "--wire" => {
                o.wire = true;
                i += 1;
            }
            "--runtimes" => {
                o.runtimes = true;
                i += 1;
            }
            "--markdown" => {
                o.markdown = true;
                i += 1;
            }
            "--provenance" => {
                o.provenance = true;
                i += 1;
            }
            "--explain" => {
                o.explain = true;
                i += 1;
            }
            // A flag we do not know is a typo, and absorbing it silently means
            // running with the opposite of what was asked — `--require-signd`
            // installs unsigned code, `--dry-run` misspelt writes the project.
            other if other.starts_with('-') => {
                eprintln!("unknown option `{other}`");
                exit(2);
            }
            // Anything else is a positional argument (`oh mcp list`, `oh add
            // <source>`), which subcommands read from `o.positional`.
            other => {
                o.positional.push(other.to_string());
                i += 1;
            }
        }
    }
    o
}

/// A numeric flag value, or exit. Parsing failure used to fall back to `0`,
/// which means "use the default" — so `--timeout-ms 5o00` silently ran with the
/// default timeout instead of the one asked for.
fn numeric_value(rest: &[String], i: usize) -> u64 {
    let raw = value_for(rest, i);
    raw.parse::<u64>().unwrap_or_else(|_| {
        eprintln!("option `{}` wants a number, got `{raw}`", rest[i]);
        exit(2);
    })
}

/// The value following a flag, or exit saying which flag is missing one.
///
/// Reading a missing value as `None` and moving on silently drops the flag —
/// `oh run --harness` would dispatch against no harness at all — and swallows
/// the next argument along with it.
fn value_for(rest: &[String], i: usize) -> String {
    match rest.get(i + 1) {
        Some(v) if !v.starts_with("--") => v.clone(),
        _ => {
            eprintln!("option `{}` needs a value", rest[i]);
            exit(2);
        }
    }
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
        // `discover` promises that a malformed capability is a hard error and
        // never a silent skip. Returning an empty set here undid that: the
        // caller could not tell "nothing to load" from "could not read any of
        // it", and reported the former.
        Err(e) => {
            eprintln!(
                "could not load capabilities from {}: {e}",
                o.capabilities.display()
            );
            exit(1);
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
            // Show the dispatched event, and the registered one alongside it when
            // the tool's class narrowed it — that is what decided who ran.
            "[explain] harness={harness} event={}{} ran={:?} skipped={:?} errored={:?} -> exit={}",
            result.event,
            if result.event == event {
                String::new()
            } else {
                format!(" (registered {event})")
            },
            result.ran,
            result.skipped,
            result.errored,
            result.exit_code
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

/// Name the target harnesses whose adapter has no recorded payload behind it.
///
/// Installability and support answer "does this map onto a native surface";
/// they say nothing about whether that mapping was ever exercised against the
/// real harness. Reporting the first without the second is the same silent
/// overstatement the honest-degradation rule exists to prevent, so every
/// command that hands you something to run says which targets are unproven.
/// It goes to **stderr**: stdout is the artifact. `oh emit > hooks.json` has to
/// produce the config and nothing else, or every consumer has to learn to strip
/// our commentary — and someone will paste it into a real file first.
fn print_provenance_note(harnesses: &[Harness]) {
    let unverified = open_harness::matrix::unverified(harnesses);
    if unverified.is_empty() {
        return;
    }
    eprintln!(
        "note: {} adapter(s) are encoded from documentation with no recorded payload: {}.",
        unverified.len(),
        unverified.join(", ")
    );
    eprintln!("      They are unverified against a live install — see `oh matrix --provenance`.");
}

fn cmd_emit(rest: &[String]) {
    let o = parse_opts(rest);
    let caps = load_caps(&o);
    if caps.is_empty() {
        eprintln!("no capabilities found under {}", o.capabilities.display());
        exit(1);
    }
    let harnesses = select_harnesses(&o);
    // This output gets pasted into a real harness's config, which is exactly
    // where it matters whether the mapping was verified against that harness or
    // read off its documentation.
    print_provenance_note(&harnesses);
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

// ---- MCP→CLI bridge (#19) --------------------------------------------------

fn cmd_mcp(rest: &[String]) {
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("");
    let o = parse_opts(&rest[rest.len().min(1)..]);

    let server = mcp_server(&o).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(2);
    });
    let mut client = mcp::Client::connect(&server).unwrap_or_else(|e| {
        eprintln!("mcp: {e}");
        exit(1);
    });

    match sub {
        "list" => {
            let tools = client.list_tools().unwrap_or_else(|e| {
                eprintln!("mcp list: {e}");
                exit(1);
            });
            for t in &tools {
                println!("{}\t{}", t.name, t.description);
            }
        }
        "call" => {
            let Some(tool) = o.tool.clone() else {
                eprintln!("mcp call requires --tool NAME");
                exit(2);
            };
            let args: serde_json::Value = match &o.json_args {
                Some(s) if !s.trim().is_empty() => serde_json::from_str(s).unwrap_or_else(|e| {
                    eprintln!("invalid --json args: {e}");
                    exit(2);
                }),
                _ => serde_json::json!({}),
            };
            let result = client.call_tool(&tool, args).unwrap_or_else(|e| {
                eprintln!("mcp call: {e}");
                exit(1);
            });
            // Print the text content if present, else the raw result JSON.
            println!("{}", mcp::result_text(&result));
        }
        _ => {
            eprintln!("mcp: expected `list` or `call`");
            exit(2);
        }
    }
}

/// Resolve which MCP server to reach: a streamable-HTTP `--url`, a stdio
/// `--command`/`--mcp-arg`, or a capability's `tool.server` (`--id` under
/// `--capabilities`, which may itself be stdio or http).
fn mcp_server(o: &Opts) -> Result<McpServer, String> {
    if let Some(url) = &o.mcp_url {
        return Ok(McpServer::Http(HttpServerSpec {
            url: url.clone(),
            headers: o.mcp_headers.clone(),
        }));
    }
    if let Some(command) = &o.mcp_command {
        return Ok(McpServer::Stdio(ServerSpec {
            command: command.clone(),
            args: o.mcp_args.clone(),
            env: Vec::new(),
            cwd: None,
        }));
    }
    if let Some(id) = &o.id {
        let caps = discover(&o.capabilities)?;
        let cap = caps
            .into_iter()
            .find(|c| &c.manifest.id == id)
            .ok_or_else(|| format!("no capability '{id}' under {}", o.capabilities.display()))?;
        return mcp::server_from_capability(&cap);
    }
    Err("mcp requires --url URL, --command CMD, or --id CAPABILITY".to_string())
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

/// `oh pack` — turn a capability directory into the publishable CDN tree
/// (content-addressed archives + a registry index). See
/// `docs/src/distribution.md`.
fn cmd_pack(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(base_url) = o.base_url.clone() else {
        eprintln!(
            "pack requires --base-url URL (where the published tree will be served, \
             e.g. https://cdn.example.com)"
        );
        exit(2);
    };
    let out = o.out.clone().unwrap_or_else(|| PathBuf::from("dist"));

    let caps = discover(&o.capabilities).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1);
    });
    if caps.is_empty() {
        eprintln!("no capabilities found in {}", o.capabilities.display());
        exit(1);
    }
    let packed = publish::pack_capabilities(&caps).unwrap_or_else(|e| {
        eprintln!("pack failed: {e}");
        exit(1);
    });
    let report = publish::write_tree(&packed, &base_url, &out).unwrap_or_else(|e| {
        eprintln!("pack failed: {e}");
        exit(1);
    });

    println!("packed {} capabilities → {}", report.blobs, out.display());
    for p in &packed {
        println!(
            "  {} {} [{}]  sha256:{}  ({} bytes){}",
            p.id,
            p.version,
            p.kind,
            p.digest,
            p.bytes.len(),
            if p.signed { "" } else { "  ⚠ unsigned" }
        );
    }
    println!();
    println!("  index:    {}", report.index_path.display());
    println!("  snapshot: {}", report.snapshot_path.display());
    if !report.unsigned.is_empty() {
        // Honest by design: a catalog that ships unvouched-for code says so.
        println!();
        println!(
            "note: {} of {} capabilities are unsigned ({}). Consumers cannot verify \
             their author — run `oh sign --capability DIR --key FILE` before packing.",
            report.unsigned.len(),
            packed.len(),
            report.unsigned.join(", ")
        );
    }
    println!();
    println!("publish by uploading blobs/ and index/ first, then registry.yaml last —");
    println!("so the catalog never names an artifact that has not landed:");
    println!("  aws s3 sync {}/blobs s3://BUCKET/blobs", out.display());
    println!("  aws s3 sync {}/index s3://BUCKET/index", out.display());
    println!(
        "  aws s3 cp   {}/registry.yaml s3://BUCKET/registry.yaml",
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
    if let Ok(cap) = load_manifest_in(&dir) {
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
    let trust_path = o
        .trust
        .clone()
        .unwrap_or_else(|| default_store_path("trust"));
    let mut store = TrustStore::load(&trust_path).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1);
    });

    // Attach a keyring: its authors become trusted once their root is trusted.
    if let Some(kr_path) = o.keyring.clone() {
        let keyring = trust::Keyring::load(&kr_path).unwrap_or_else(|e| {
            eprintln!("{e}");
            exit(1);
        });
        if !keyring.verify() {
            eprintln!(
                "refusing to attach {}: its root signature is invalid",
                kr_path.display()
            );
            exit(1);
        }
        let root_fp = trust::fingerprint(&keyring.root_public_key);
        let n = keyring.keys.len();
        store.attach_keyring(keyring);
        store.write(&trust_path).unwrap_or_else(|e| {
            eprintln!("{e}");
            exit(1);
        });
        println!(
            "attached keyring vouching for {n} author(s) under root {root_fp} → {}",
            trust_path.display()
        );
        eprintln!(
            "trust that root with: oh trust --root --key <root.key> --trust {}",
            trust_path.display()
        );
        return;
    }

    // Trust a root key (delegation anchor): from a keyfile (--key) or a capability
    // signer (--capability).
    if o.root {
        let (public_key, label) = if let Some(keyf) = o.key.clone() {
            let key = trust::Keyfile::load(&keyf).unwrap_or_else(|e| {
                eprintln!("{e}");
                exit(1);
            });
            (key.public_key, o.label.clone().unwrap_or(key.label))
        } else if let Some(dir) = o.capability_one.clone() {
            let sig = require_signature(&dir);
            (sig.public_key, o.label.clone().unwrap_or_default())
        } else {
            eprintln!("trust --root requires --key ROOT_KEYFILE or --capability DIR");
            exit(2);
        };
        let fp = trust::fingerprint(&public_key);
        if store.add_root(&public_key, &label) {
            store.write(&trust_path).unwrap_or_else(|e| {
                eprintln!("{e}");
                exit(1);
            });
            println!("trusted root {fp} → {}", trust_path.display());
        } else {
            println!("already a trusted root: {fp}");
        }
        return;
    }

    // Default: trust a capability's signer directly (trust-on-first-use).
    let Some(dir) = o.capability_one.clone() else {
        eprintln!("trust requires --capability DIR (whose signer to trust)");
        exit(2);
    };
    let sig = require_signature(&dir);
    let label = o.label.clone().unwrap_or_default();
    let fp = trust::fingerprint(&sig.public_key);
    if store.add(&sig.public_key, &label) {
        store.write(&trust_path).unwrap_or_else(|e| {
            eprintln!("{e}");
            exit(1);
        });
        println!("trusted {fp} → {}", trust_path.display());
    } else if store.is_revoked(&sig.public_key) {
        eprintln!("refusing to trust {fp}: it is revoked (un-revoke it first)");
        exit(1);
    } else {
        println!("already trusted: {fp}");
    }
}

/// The signature beside a capability, or exit explaining why there isn't one.
/// Keeps the two failures distinct: a capability that was never signed, and one
/// whose signature file is there but broken.
fn require_signature(dir: &std::path::Path) -> trust::Signature {
    match trust::Signature::load(dir) {
        Ok(Some(sig)) => sig,
        Ok(None) => {
            eprintln!(
                "no {} in {} (sign it first)",
                trust::SIG_NAME,
                dir.display()
            );
            exit(1);
        }
        Err(e) => {
            eprintln!("{}: {e}", dir.display());
            exit(1);
        }
    }
}

/// `oh keyring --key ROOT_KEYFILE --capability AUTHOR_DIR --out FILE` — the root
/// vouches for the author who signed AUTHOR_DIR, producing a signed keyring.
fn cmd_keyring(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(keyf) = o.key.clone() else {
        eprintln!("keyring requires --key ROOT_KEYFILE (the vouching root)");
        exit(2);
    };
    let Some(dir) = o.capability_one.clone() else {
        eprintln!("keyring requires --capability DIR (the author to vouch for)");
        exit(2);
    };
    let root = trust::Keyfile::load(&keyf).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1);
    });
    let sig = require_signature(&dir);
    let author = trust::TrustedKey {
        public_key: sig.public_key,
        label: o.label.clone().unwrap_or_default(),
    };
    let keyring = trust::sign_keyring(vec![author], &root).unwrap_or_else(|e| {
        eprintln!("keyring signing failed: {e}");
        exit(1);
    });
    let out = o
        .out
        .clone()
        .unwrap_or_else(|| default_store_path("keyring"));
    keyring.write(&out).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1);
    });
    println!(
        "signed keyring ({} author) by root {} → {}",
        keyring.keys.len(),
        trust::fingerprint(&keyring.root_public_key),
        out.display()
    );
}

fn cmd_revoke(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(dir) = o.capability_one.clone() else {
        eprintln!("revoke requires --capability DIR (whose signer to revoke)");
        exit(2);
    };
    let sig = require_signature(&dir);
    let trust_path = o
        .trust
        .clone()
        .unwrap_or_else(|| default_store_path("trust"));
    let mut store = TrustStore::load(&trust_path).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1);
    });
    let reason = o.reason.clone().unwrap_or_default();
    let fp = trust::fingerprint(&sig.public_key);
    if store.revoke(&sig.public_key, &reason) {
        store.write(&trust_path).unwrap_or_else(|e| {
            eprintln!("{e}");
            exit(1);
        });
        println!("revoked {fp} → {}", trust_path.display());
        eprintln!("any capability signed by {fp} will now fail verification");
    } else {
        println!("already revoked: {fp}");
    }
}

/// The default path for a trust store / keyring the user didn't name: the YAML
/// spelling, unless a legacy `<stem>.json` is already sitting there — writing a
/// fresh `trust.yaml` next to a `trust.json` full of pinned keys would silently
/// drop every one of them.
fn default_store_path(stem: &str) -> PathBuf {
    open_harness::config::find(std::path::Path::new(stem))
        .unwrap_or_else(|| PathBuf::from(format!("{stem}.yaml")))
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
    o.trust.is_some() || o.require_signed || o.deny_network || o.deny_exec || o.deny_write
}

/// Verify a composed set, surface permission manifests, and gate. Only called
/// when the user opts in (a trust store, `--require-signed`, or a deny flag).
fn enforce_trust(caps: &[LoadedCapability], o: &Opts) {
    let store = load_trust(o);
    let policy = PermissionPolicy {
        allow_network: !o.deny_network,
        allow_exec: !o.deny_exec,
        allow_write: !o.deny_write,
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
///
/// Under `--locked` the lockfile becomes a contract instead of an output: it
/// must exist, the resolution must match it exactly, and nothing is written.
/// That is what makes a lock worth having in CI — a resolve that quietly
/// rewrites the lock it was supposed to honor verifies nothing.
fn resolve_profile_opts(path: &std::path::Path, locked: bool) -> Resolved {
    let profile = Profile::load(path).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(2);
    });
    let workdir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let lock_path = workdir.join(profile::LOCK_NAME);
    let existing = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|t| Lock::from_text(&t).ok());

    if locked && existing.is_none() {
        eprintln!(
            "--locked requires an existing {}; run `oh resolve --profile {}` first",
            lock_path.display(),
            path.display()
        );
        exit(1);
    }

    let resolved = profile::resolve(&profile, workdir, existing.as_ref()).unwrap_or_else(|e| {
        eprintln!("resolve failed: {e}");
        exit(1);
    });

    if locked {
        let recorded = existing.expect("checked above");
        if resolved.lock != recorded {
            eprintln!(
                "lockfile is stale — {} does not match the profile as resolved:",
                lock_path.display()
            );
            for line in lock_drift(&recorded, &resolved.lock) {
                eprintln!("  {line}");
            }
            eprintln!(
                "\nre-run `oh resolve --profile {}` and commit the result",
                path.display()
            );
            exit(1);
        }
        for w in &resolved.warnings {
            eprintln!("warning: {w}");
        }
        return resolved;
    }

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
    let profile_path = resolve_profile_path(&o);
    let resolved = resolve_profile_opts(&profile_path, o.locked);
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

// ---- try ------------------------------------------------------------------

/// Where `try` resolves. Only the fetch cache is written, and only under
/// `<workdir>/.open-harness/cache/` — so rooting it at `$HOME` puts it in
/// `~/.open-harness/cache/`, beside the user scope's own, and a second `try` of
/// the same repo is a fetch rather than a clone.
///
/// The project is never the workdir: `try` must not leave a `.open-harness/`
/// behind in a repository you were only evaluating something for.
fn try_workdir() -> PathBuf {
    home_dir().unwrap_or_else(std::env::temp_dir)
}

/// Re-root a local spec on the caller's cwd.
///
/// A `local` source path is resolved against the *profile's* workdir, and
/// `try`'s workdir is `$HOME` rather than the directory you are standing in. So
/// `oh try ./capabilities` would ask `$HOME` for `./capabilities` — a path
/// nobody meant. Every other source kind is absolute already (a URL, a repo).
fn absolutise_local(source: profile::Source) -> profile::Source {
    match source {
        profile::Source::Local(mut l) => {
            let p = PathBuf::from(&l.path);
            if p.is_relative() {
                if let Ok(mut abs) = std::env::current_dir() {
                    // `./` components are dropped rather than joined: the result
                    // is echoed back as the source's origin and as the `oh add`
                    // line, and `/home/me/proj/./caps` reads like a bug.
                    for c in p.components() {
                        match c {
                            std::path::Component::CurDir => {}
                            other => abs.push(other.as_os_str()),
                        }
                    }
                    l.path = abs.to_string_lossy().into_owned();
                }
            }
            profile::Source::Local(l)
        }
        other => other,
    }
}

/// The harnesses `try` previews against, and a word on where that set came from.
///
/// Defaulting to all eleven would answer a question nobody asked: what matters
/// is whether this capability works on *your* harnesses. So the existing profile
/// decides, and `--harness` overrides it. Which set was used is printed, because
/// a preview that quietly narrowed itself is a preview you cannot trust.
fn try_harnesses(o: &Opts) -> (Vec<Harness>, String) {
    if o.harness.is_some() {
        return (select_harnesses(o), "--harness".to_string());
    }
    let found = if o.global {
        user_profile_path(o)
    } else {
        o.profile
            .clone()
            .or_else(|| profile::find_profile(std::path::Path::new(".")))
    };
    if let Some(path) = found {
        if let Ok(p) = Profile::load(&path) {
            if let Ok(hs) = p.harness_set() {
                if !hs.is_empty() {
                    return (hs, format!("from {}", path.display()));
                }
            }
        }
    }
    (ALL.to_vec(), "no profile found — all harnesses".to_string())
}

/// `oh try <spec>` — resolve one source and report what it *would* install.
///
/// The gap this fills: every other command answers for capabilities you have
/// already committed to. `add` edits your profile, `resolve` pins it, `sync`
/// writes it. To judge a stranger's capability you had to adopt it first and
/// read the diff afterwards — which is exactly backwards for a tool whose claim
/// is that you can see what lands on your machine before it lands.
///
/// So: nothing is written outside the fetch cache. Not the profile, not the
/// lockfile, not one byte of harness config. What comes back is the resolved
/// revision (so `oh add` gets the same bits), what it would write where, what it
/// degrades to per harness, what it asks for — network, exec, filesystem writes,
/// a runtime you may not have — and whether anyone signed it.
///
/// Resolution stays `flat` regardless of what the capability declares: a preview
/// that cloned a dependency graph would be doing the thing you ran `try` to
/// avoid committing to.
fn cmd_try(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(spec) = o.positional.first().cloned() else {
        eprintln!("try requires a source: `oh try <spec>` (a git URL, an archive URL, or a path)");
        exit(2);
    };
    let source = profile::Source::parse_spec(&spec).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(2);
    });
    let (harnesses, target_origin) = try_harnesses(&o);
    let profile = Profile {
        name: format!("try:{spec}"),
        harnesses: harnesses.iter().map(|h| h.id().to_string()).collect(),
        sources: vec![absolutise_local(source)],
        resolution: profile::Resolution::Flat,
        transitive_trust: profile::TransitiveTrust::default(),
        trust: None,
    };
    let workdir = try_workdir();
    let resolved = profile::resolve(&profile, &workdir, None).unwrap_or_else(|e| {
        eprintln!("resolve failed: {e}");
        exit(1);
    });
    for w in &resolved.warnings {
        eprintln!("warning: {w}");
    }

    for s in &resolved.lock.sources {
        let at = s
            .rev
            .as_deref()
            .map(|r| &r[..r.len().min(12)])
            .unwrap_or("-");
        println!("source   [{}] {} @ {}", s.kind, s.origin, at);
    }
    let ids: Vec<&str> = harnesses.iter().map(|h| h.id()).collect();
    println!("targets  {} ({target_origin})", ids.join(" "));
    println!(
        "scope    {}",
        match scope_of(&o) {
            sync::Scope::User => "user (--global)",
            sync::Scope::Project => "project",
        }
    );

    if resolved.capabilities.is_empty() {
        eprintln!("\n{spec} resolved, but provides no capabilities");
        exit(1);
    }

    let store = load_trust(&o);
    let plan = sync::plan_with(
        &resolved.capabilities,
        &harnesses,
        scope_of(&o),
        wire_of(&o),
    );

    for cap in &resolved.capabilities {
        let m = &cap.manifest;
        println!(
            "\ncapability  {}  ({})  {}  [{}]",
            cap.qualified_name(),
            m.name,
            m.version,
            m.kind.as_str()
        );
        println!("  signature {}", trust::verify(&cap.dir, &store).status());
        // Always printed, including the all-empty case. "asks for nothing" is
        // the single most useful thing a preview can tell you, and it only
        // means anything if its absence would have been shown too.
        let p = &m.permissions;
        println!(
            "  asks      network {} · exec {} · writes {}",
            p.network_summary(),
            list_or_none(&p.exec),
            list_or_none(&p.write),
        );
        if !m.runtime.requires.is_empty() {
            let state = match open_harness::runtime::missing_requirements(&m.runtime) {
                None => "ready".to_string(),
                Some(gap) => format!("MISSING — {gap}"),
            };
            println!("  runtime   {} — {state}", m.runtime.requires.join(", "));
        }

        let mine: Vec<&sync::DesiredFile> = plan
            .files
            .iter()
            .filter(|f| f.sources.iter().any(|s| s == &m.id))
            .collect();
        if mine.is_empty() {
            println!("  writes    (nothing on these harnesses)");
        } else {
            println!("  writes");
            for f in mine {
                let deg = if f.degraded.is_empty() {
                    String::new()
                } else {
                    format!("   degraded: {}", f.degraded.join("; "))
                };
                println!("    {}  [{}]{deg}", f.path, f.harnesses.join(" "));
            }
        }
    }

    // Everything that could not be placed, in the same shape `sync` reports it —
    // a preview that hid the unsupported half would be the friendlier lie.
    print_deferred(&plan.deferred);

    println!("\nnothing was written. to keep it:");
    println!("  oh add {spec}");
    println!(
        "  oh sync {}",
        if o.global {
            "--global".to_string()
        } else {
            "--into .".to_string()
        }
    );
    print_provenance_note(&harnesses);
}

fn list_or_none(v: &[String]) -> String {
    if v.is_empty() {
        "none".to_string()
    } else {
        v.join(",")
    }
}

/// The scope a `sync`/`check` acts on.
fn wire_of(o: &Opts) -> sync::Wire {
    if o.wire {
        sync::Wire::Write
    } else {
        sync::Wire::Report
    }
}

fn scope_of(o: &Opts) -> sync::Scope {
    if o.global {
        sync::Scope::User
    } else {
        sync::Scope::Project
    }
}

/// The root to write into, and the profile to read, for this scope.
///
/// `--global` is a flag rather than a profile field on purpose: a profile is
/// data that travels — cloned, vendored, resolved from a git source — and a
/// `scope: user` key in one would let a repository you cloned write into your
/// home directory. A flag cannot be set by anything but the person at the
/// keyboard.
fn scope_root(o: &Opts) -> PathBuf {
    if !o.global {
        return o.into.clone().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(into) = &o.into {
        // Honoured so the whole user scope is testable without touching a real
        // home directory.
        return into.clone();
    }
    home_dir().unwrap_or_else(|| {
        eprintln!("--global needs a home directory: set HOME (or USERPROFILE on Windows)");
        exit(2);
    })
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// The user-scope profile: `~/.open-harness/open-harness.yaml`.
///
/// Deliberately *not* merged with the project profile. The harnesses already
/// read both levels and resolve the precedence themselves, so the project
/// profile lists only what is extra — resolving the two together would vendor
/// your global capabilities into every repo, which is churn in `git status` and
/// a second copy to keep in step.
fn user_profile_path(o: &Opts) -> Option<PathBuf> {
    if let Some(p) = &o.profile {
        return Some(p.clone());
    }
    let p = scope_root(o)
        .join(".open-harness")
        .join(profile::PROFILE_NAME);
    p.is_file().then_some(p)
}

fn cmd_sync(rest: &[String]) {
    let o = parse_opts(rest);
    let scope = scope_of(&o);
    let into = if o.global {
        scope_root(&o)
    } else {
        match o.into.clone() {
            Some(p) => p,
            None => {
                eprintln!("sync requires --into <project-dir> (or --global for user scope)");
                exit(2);
            }
        }
    };

    // Uninstall short-circuits: it reads the lockfile, not the capabilities.
    if o.uninstall {
        let report = sync::uninstall(&into, o.dry_run).unwrap_or_else(|e| {
            eprintln!("uninstall failed: {e}");
            exit(1);
        });
        print_apply_report(&report, "uninstall", &into);
        if report.has_blocked() {
            eprintln!("\nsome files were left in place: exiting non-zero");
            exit(1);
        }
        return;
    }

    // Profile mode (#15): resolve sources + harnesses from the profile; else the
    // ad-hoc `--capabilities` / `--harness` mode.
    let discovered = if o.global {
        user_profile_path(&o)
    } else {
        o.profile
            .clone()
            .or_else(|| profile::find_profile(std::path::Path::new(".")))
    };
    let (caps, harnesses) = if let Some(profile_path) = discovered {
        let resolved = resolve_profile_opts(&profile_path, o.locked);
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

    let plan = sync::plan_with(&caps, &harnesses, scope, wire_of(&o));
    let report = sync::apply(&into, &plan, o.dry_run).unwrap_or_else(|e| {
        eprintln!("sync failed: {e}");
        exit(1);
    });
    print_apply_report(&report, &format!("sync ({} scope)", scope.as_str()), &into);
    // A blocked target means the requested state is not on disk. Reporting it
    // and still exiting 0 would be the silent-success lie this project forbids.
    if report.has_blocked() {
        eprintln!("\nsome targets were not installed: exiting non-zero");
        exit(1);
    }
}

fn cmd_check(rest: &[String]) {
    let o = parse_opts(rest);

    // With `--into`, `check` is drift detection against a synced project. Sources
    // resolve the same way as `sync`: from a `--profile` (its sources + harnesses)
    // or the ad-hoc `--capabilities` / `--harness` mode — so you can drift-check
    // exactly what you profile-synced.
    let scope = scope_of(&o);
    let checked_root = if o.global {
        Some(scope_root(&o))
    } else {
        o.into.clone()
    };
    if let Some(into) = checked_root {
        let discovered = if o.global {
            user_profile_path(&o)
        } else {
            o.profile
                .clone()
                .or_else(|| profile::find_profile(std::path::Path::new(".")))
        };
        let (caps, harnesses) = if let Some(profile_path) = discovered {
            let resolved = resolve_profile_opts(&profile_path, o.locked);
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
        let plan = sync::plan_with(&caps, &harnesses, scope, wire_of(&o));
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
        // Installability is a per-harness question ("can this kind be written
        // there"); the runtime is a per-*host* one ("will it actually run").
        // Both have to be answered, or a capability that denies every tool call
        // reads as "installable — clean" on all eleven harnesses.
        if !cap.manifest.runtime.requires.is_empty() {
            match open_harness::runtime::missing_requirements(&cap.manifest.runtime) {
                None => println!(
                    "  {:<12} ready — {}",
                    "runtime",
                    cap.manifest.runtime.requires.join(", ")
                ),
                Some(gap) => println!("  {:<12} MISSING — {gap}", "runtime"),
            }
        }
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
    // "installable — clean" is a claim about the *adapter*, so it inherits that
    // adapter's provenance. Saying so here is the difference between "this will
    // work" and "this is what we believe the harness reads".
    print_provenance_note(&ALL);
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
            ChangeAction::Reduce => "－ reduce   ",
        };
        // A shared file is one open-harness merged into rather than owns, so say
        // so on every line — it changes what a later uninstall will do to it.
        let shared = if c.shared {
            "  (shared — merged)"
        } else {
            ""
        };
        println!(
            "  {mark} {}{}{shared}",
            c.path,
            provenance(&c.harnesses, &c.sources)
        );
    }
    if report.changes.is_empty() {
        println!("  (no managed files)");
    }
    println!(
        "\n  {} created, {} updated, {} unchanged, {} pruned, {} reduced",
        report.count(ChangeAction::Create),
        report.count(ChangeAction::Update),
        report.count(ChangeAction::Unchanged),
        report.count(ChangeAction::Prune),
        report.count(ChangeAction::Reduce),
    );
    if !report.conflicts.is_empty() {
        println!("\n  composition conflicts resolved:");
        for (_path, msgs) in &report.conflicts {
            for m in msgs {
                println!("    ⚠ {m}");
            }
        }
    }
    print_blocked(&report.blocked);
    print_deferred(&report.deferred);
}

/// Files open-harness refused to touch. Always shown, and the reason with them —
/// a refusal that is not reported is indistinguishable from a silent drop.
fn print_blocked(blocked: &[sync::Blocked]) {
    if blocked.is_empty() {
        return;
    }
    println!(
        "\n  blocked: {} file(s) open-harness will not write over",
        blocked.len()
    );
    for b in blocked {
        println!("    ✗ {}: {}", b.path, b.reason);
    }
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
    print_blocked(&report.blocked);
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
    let mut no_user = Vec::new();
    let mut blocked = Vec::new();
    for d in deferred {
        match d.reason {
            sync::DeferReason::Registration => registration.push(d),
            sync::DeferReason::GlobalPath => global.push(d),
            sync::DeferReason::NoUserLocation => no_user.push(d),
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
    if !no_user.is_empty() {
        // Not a limitation of open-harness so much as of what is knowable: the
        // harness documents no user-level home for this artifact, and writing a
        // guess into someone's home directory is worse than saying so.
        //
        // Summarized by harness rather than listed: at user scope this is most
        // of the matrix, and eighty lines of detail buries the handful of things
        // that did install.
        let mut by_harness: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
        for d in &no_user {
            by_harness
                .entry(d.harness.as_str())
                .or_default()
                .push(d.source.as_str());
        }
        println!(
            "\n  manual: {} target(s) across {} harness(es) have no documented user-level location",
            no_user.len(),
            by_harness.len()
        );
        for (harness, sources) in &by_harness {
            let mut ids = sources.to_vec();
            ids.sort_unstable();
            ids.dedup();
            println!("    → {harness}: {}", ids.join(", "));
        }
        println!("      install these into a project, or see `oh help sync` on scopes");
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

// ---- authoring / DevEx (#18) ----------------------------------------------

fn cmd_scaffold(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(id) = o.id.clone() else {
        eprintln!("scaffold requires --id <capability-id>");
        exit(2);
    };
    let into = o
        .into
        .clone()
        .unwrap_or_else(|| PathBuf::from("capabilities"));

    // `--project`: a TypeScript capability as a full npm package (uses
    // @open-harness/node in-process for typed authoring + a dev-time test).
    if o.scaffold_project {
        let files = scaffold::scaffold_ts_project(&id, &into).unwrap_or_else(|e| {
            eprintln!("scaffold failed: {e}");
            exit(1);
        });
        println!("scaffolded TypeScript capability package '{id}':");
        for f in &files {
            println!("  + {f}");
        }
        println!("\ndevelop it:");
        println!("  cd {}", into.join(&id).display());
        println!("  npm install                 # typescript + @types/node");
        println!("  npm link @open-harness/node # types + in-process test (see README)");
        println!("  npm run build && npm test");
        return;
    }

    let kind_str = o.scaffold_kind.as_deref().unwrap_or("hook");
    let Some(kind) = KindId::parse(kind_str) else {
        eprintln!(
            "unknown --kind '{kind_str}' (hook|skill|rule|command|tool|permission|agent|instructions)"
        );
        exit(2);
    };
    let lang_str = o.scaffold_lang.as_deref().unwrap_or("python");
    let Some(lang) = Lang::parse(lang_str) else {
        eprintln!("unknown --lang '{lang_str}' (python|node|typescript|bash)");
        exit(2);
    };

    let files = scaffold::scaffold(kind, lang, &id, &into).unwrap_or_else(|e| {
        eprintln!("scaffold failed: {e}");
        exit(1);
    });
    println!("scaffolded {} capability '{id}':", kind.as_str());
    for f in &files {
        println!("  + {f}");
    }
    if kind == KindId::Hook {
        println!(
            "\nrun it:  echo '{{}}' | oh run --harness claude-code --capabilities {} --event pre.tool.any",
            into.display()
        );
    } else {
        println!("\ncheck it: oh check --capabilities {}", into.display());
    }
}

/// `oh capture` — record the raw native payload a harness sends on stdin into a
/// fixture, then allow (exit 0) so the harness is never disrupted. Wire it as a
/// harness's hook entrypoint to build the fixture corpus from live runs (#24).
fn cmd_capture(rest: &[String]) {
    let o = parse_opts(rest);
    let out = o
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("captured.stdin.json"));
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    match open_harness::capture::record_fixture(&buf, &out) {
        Ok(()) => {
            let ctx = match (&o.harness, &o.event) {
                (Some(h), Some(e)) => format!(" ({h} {e})"),
                (Some(h), None) => format!(" ({h})"),
                _ => String::new(),
            };
            eprintln!("captured native payload{ctx} → {}", out.display());
            // The sidecar is what distinguishes this from a payload transcribed
            // out of the vendor's docs, so it is written with the fixture rather
            // than being something to remember afterwards.
            let p = open_harness::capture::Provenance {
                harness: o.harness.as_deref(),
                event: o.event.as_deref(),
                label: o.label.as_deref(),
            };
            match open_harness::capture::record_provenance(&out, &p) {
                Ok(path) => eprintln!("recorded provenance → {}", path.display()),
                Err(e) => eprintln!("capture: {e}"),
            }
        }
        // Even on a capture error, allow: a capture hook must not block the agent.
        Err(e) => eprintln!("capture: {e}"),
    }
    exit(0);
}

/// `oh version` — the binary + frozen wire-protocol versions, for a consumer to
/// check compatibility before depending on this build (version negotiation).
fn cmd_version() {
    println!(
        "open-harness {} (protocol {})",
        open_harness::api::version(),
        open_harness::api::protocol_version()
    );
}

fn cmd_init(rest: &[String]) {
    let o = parse_opts(rest);
    let dir = o.into.clone().unwrap_or_else(|| PathBuf::from("."));
    let path = dir.join(profile::PROFILE_NAME);
    // Refuse if a profile already exists in *any* accepted spelling or name, so
    // `init` never quietly shadows an `open-harness.json` — or an `oh.yaml` —
    // that is still being read.
    if let Some(existing) = profile::all_profiles(&dir).first() {
        eprintln!("{} already exists", existing.display());
        exit(1);
    }
    let harnesses: Vec<String> = match o.harness.as_deref() {
        None | Some("all") => vec!["claude-code".into(), "cursor".into()],
        Some(id) => vec![id.to_string()],
    };
    let profile = serde_json::json!({
        "name": "default",
        "harnesses": harnesses,
        "sources": [ "capabilities" ],
    });
    if let Err(e) = open_harness::config::write(&path, &profile) {
        eprintln!("{e}");
        exit(1);
    }
    println!("wrote {}", path.display());
    println!("next: oh scaffold --kind hook --lang python --id my-guard");
    println!("      oh sync --profile {} --into .", path.display());
}

/// Config filenames `oh migrate` rewrites from JSON to YAML. Everything else is
/// left alone — a harness's own `.json` config is that harness's format, not
/// ours, and rewriting it would produce a file the harness cannot read.
const MIGRATABLE: &[&str] = &[
    "capability.json",
    "open-harness.json",
    "open-harness.lock",
    "sync.lock.json",
    "trust.json",
    "keyring.json",
    "capability.sig",
];

fn cmd_migrate(rest: &[String]) {
    let o = parse_opts(rest);
    let root = o.into.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut found = Vec::new();
    collect_migratable(&root, &mut found);
    found.sort();

    if found.is_empty() {
        println!("no JSON config files to migrate under {}", root.display());
        return;
    }
    let mut migrated = 0usize;
    for path in &found {
        if o.dry_run {
            println!(
                "  would migrate  {} → {}",
                path.display(),
                open_harness::config::migrated_path(path).display()
            );
            migrated += 1;
            continue;
        }
        match open_harness::config::migrate_file(path) {
            // `None` means it parsed as something other than JSON — already
            // migrated, so a re-run is a no-op rather than a churned file.
            Ok(None) => {}
            Ok(Some(target)) => {
                println!("  migrated  {} → {}", path.display(), target.display());
                migrated += 1;
            }
            Err(e) => {
                eprintln!("  failed    {}: {e}", path.display());
                exit(1);
            }
        }
    }
    println!(
        "\n{migrated} file(s) {}",
        if o.dry_run {
            "would migrate"
        } else {
            "migrated"
        }
    );
    if migrated > 0 && !o.dry_run {
        println!(
            "note: a capability's content digest changes with its manifest filename, \
             so any existing `capability.sig` must be re-signed (`oh sign`)."
        );
    }
}

/// Walk `dir` collecting migratable config files. Hidden directories are skipped
/// except `.open-harness`, which holds the sync lockfile; `.git` and friends are
/// none of our business.
fn collect_migratable(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            if !name.starts_with('.') || name == ".open-harness" {
                collect_migratable(&entry.path(), out);
            }
        } else if ft.is_file() && MIGRATABLE.contains(&name.as_str()) {
            out.push(entry.path());
        }
    }
}

/// `oh install --runtimes` — run capabilities' declared provisioning commands.
///
/// The only place in open-harness that executes something a capability author
/// chose but the wire protocol did not define. Three gates stand in front of it,
/// all deliberate:
///
///  1. **It is a separate verb.** `sync` writes config and never provisions;
///     executing a package manager is a different act.
///  2. **It shows before it runs.** Without `--yes` it prints the exact commands
///     and stops. `pip`/`npm` execute arbitrary code at install time, so the
///     blast radius should be visible before it happens, not after.
///  3. **`--deny-network` refuses outright**, because provisioning is
///     network-touching by nature and the flag would otherwise be a lie.
///
/// `--require-signed` additionally restricts it to capabilities whose signature
/// verifies against the trust store — the same gate transitive acquisition uses,
/// for the same reason.
fn cmd_install(rest: &[String]) {
    let o = parse_opts(rest);
    if !o.runtimes {
        eprintln!("oh install --runtimes [--capabilities DIR | --profile FILE] [--yes]");
        exit(2);
    }
    if o.deny_network {
        eprintln!(
            "--deny-network refuses runtime provisioning: fetching dependencies needs the \
             network, so running it under this flag would be dishonest"
        );
        exit(1);
    }

    let caps = match o.profile.clone() {
        Some(p) => resolve_profile_opts(&p, o.locked).capabilities,
        None => load_caps(&o),
    };
    let store = load_trust(&o);

    let mut queued = Vec::new();
    for cap in &caps {
        let Some(spec) = cap.manifest.runtime.provision.as_ref() else {
            continue;
        };
        if o.require_signed {
            let v = trust::verify(&cap.dir, &store);
            if !v.passes(true) {
                eprintln!(
                    "  skipped   {} — {} (--require-signed)",
                    cap.manifest.id,
                    v.status()
                );
                continue;
            }
        }
        queued.push((cap, spec));
    }

    if queued.is_empty() {
        println!("no capabilities declare `runtime.provision`.");
        return;
    }

    println!(
        "{} capability(ies) declare a provisioning command:\n",
        queued.len()
    );
    for (cap, spec) in &queued {
        let state = match open_harness::runtime::missing_requirements(&cap.manifest.runtime) {
            None => "ready".to_string(),
            Some(gap) => format!("missing {gap}"),
        };
        println!("  {:<20} {}", cap.manifest.id, spec.display());
        println!("  {:<20}   in {} · {state}", "", cap.dir.display());
    }

    if !o.yes {
        println!(
            "\nThese commands run with your privileges and may execute code from the \
             package registries they contact.\nRe-run with --yes to proceed."
        );
        return;
    }

    println!("\nprovisioning:");
    let mut failed = 0;
    for (cap, _) in &queued {
        let Some(outcome) = open_harness::runtime::provision(cap) else {
            continue;
        };
        let mark = if outcome.success && outcome.requirements_met {
            "✓"
        } else {
            failed += 1;
            "✗"
        };
        println!("  {mark} {:<20} {}", outcome.capability, outcome.command);
        // A provisioner that exits 0 without satisfying the declared
        // requirement has not done its job, and saying "✓" there would be the
        // same silent lie this project refuses everywhere else.
        if outcome.success && !outcome.requirements_met {
            println!(
                "      exited 0 but `runtime.requires` is still unsatisfied — \
                 the command ran, the dependency is not there"
            );
        }
        if !outcome.output.is_empty() && !outcome.success {
            for line in outcome.output.lines().take(5) {
                println!("      {line}");
            }
        }
    }
    if failed > 0 {
        eprintln!("\n{failed} provisioning command(s) did not succeed");
        exit(1);
    }
    println!("\nall provisioned.");
}

fn cmd_doctor(rest: &[String]) {
    let o = parse_opts(rest);
    println!("open-harness doctor");
    println!("\ninterpreters / tools on PATH:");
    let mut missing = 0;
    for (name, why) in [
        ("python3", "Python capabilities"),
        ("node", "TypeScript/Node capabilities"),
        ("sh", "bash capabilities + MCP bridge wrappers"),
        ("git", "git sources (personal-repo sync)"),
    ] {
        let found = open_harness::runtime::find_executable(name);
        println!(
            "  {:<8} {}",
            name,
            match &found {
                Some(p) => format!("✓ {p}"),
                None => {
                    missing += 1;
                    let hint = open_harness::runtime::install_hint(name)
                        .map(|h| format!(" · install: {h}"))
                        .unwrap_or_default();
                    format!("✗ not found — needed for {why}{hint}")
                }
            }
        );
    }

    println!("\ncapabilities under {}:", o.capabilities.display());
    match discover(&o.capabilities) {
        Ok(caps) if caps.is_empty() => println!("  (none found)"),
        Ok(caps) => {
            for cap in &caps {
                // A capability is "healthy" if it plans on at least one harness
                // *and* the runtime its entrypoint needs is actually present.
                // Reporting only the first would call a capability that denies
                // every tool call "✓".
                let installable = ALL.iter().any(|&h| {
                    matches!(
                        kind_impl(cap.manifest.kind).plan(cap, h).installability,
                        Installability::Clean | Installability::Degraded(_)
                    )
                });
                let runtime_gap =
                    open_harness::runtime::missing_requirements(&cap.manifest.runtime);
                println!(
                    "  {} {:<16} [{}]",
                    if installable && runtime_gap.is_none() {
                        "✓"
                    } else {
                        "✗"
                    },
                    cap.manifest.id,
                    cap.manifest.kind.as_str()
                );
                if let Some(gap) = runtime_gap {
                    println!("      runtime missing: {gap}");
                    missing += 1;
                }
            }
        }
        Err(e) => {
            println!("  could not scan: {e}");
            missing += 1;
        }
    }

    if missing > 0 {
        println!("\n{missing} issue(s) found.");
        exit(1);
    }
    println!("\nall good.");
}

/// The profile to act on: `--profile` when given, else the one this project
/// keeps. Exits with the fix in the message when there is none, or when several
/// would be ambiguous.
fn resolve_profile_path(o: &Opts) -> PathBuf {
    if let Some(p) = &o.profile {
        return p.clone();
    }
    let found = profile::all_profiles(std::path::Path::new("."));
    match found.len() {
        1 => found.into_iter().next().unwrap(),
        0 => {
            eprintln!(
                "no profile here — expected {} (or oh.yaml / harness.yaml) in the current \
                 directory.\nRun `oh init` to create one, or pass --profile FILE.",
                profile::PROFILE_NAME
            );
            exit(2);
        }
        _ => {
            let names: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
            eprintln!(
                "several profiles here ({}) — keep one, or pick with --profile FILE.",
                names.join(", ")
            );
            exit(2);
        }
    }
}

/// A git source as a mapping, for the explicit `--git` form.
fn git_value(g: &profile::GitSource) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("url".into(), serde_json::json!(g.url));
    m.insert("rev".into(), serde_json::json!(g.rev));
    if let Some(sub) = &g.subdir {
        m.insert("subdir".into(), serde_json::json!(sub));
    }
    serde_json::Value::Object(m)
}

fn cmd_add(rest: &[String]) {
    let o = parse_opts(rest);
    let pf = resolve_profile_path(&o);
    // `oh add <spec>` infers the kind from the spec itself; `--local` / `--git`
    // stay as explicit overrides for what inference cannot settle.
    let source = if let Some(p) = &o.local {
        serde_json::json!({ "local": { "path": p } })
    } else if let Some(u) = &o.git {
        let g = profile::GitSource::parse_spec(u).unwrap_or_else(|e| {
            eprintln!("{e}");
            exit(2);
        });
        serde_json::json!({ "git": git_value(&g) })
    } else if let Some(spec) = o.positional.first() {
        // Validated now so a typo fails here rather than at the next resolve,
        // but stored as written: a profile that says `git+https://…@v1` keeps
        // saying that, rather than being expanded into a mapping nobody wrote.
        profile::Source::parse_spec(spec).unwrap_or_else(|e| {
            eprintln!("{e}");
            exit(2);
        });
        serde_json::Value::String(spec.clone())
    } else {
        eprintln!(
            "add requires a source: `oh add <spec>` (a git URL, an archive URL, \
             or a path), or --local PATH / --git SPEC to be explicit"
        );
        exit(2);
    };
    let mut v = read_profile_value(&pf);
    let sources = v.as_object_mut().and_then(|m| {
        m.entry("sources")
            .or_insert(serde_json::json!([]))
            .as_array_mut()
    });
    match sources {
        Some(arr) => arr.push(source),
        None => {
            eprintln!("profile `sources` is not an array");
            exit(1);
        }
    }
    if let Some(h) = &o.harness {
        add_harness(&mut v, h);
    }
    write_profile_value(&pf, &v);
    println!("added source to {}", pf.display());
}

fn cmd_remove(rest: &[String]) {
    let o = parse_opts(rest);
    let pf = resolve_profile_path(&o);
    let (kind, target) = match (&o.local, &o.git) {
        (Some(p), _) => ("local", p.clone()),
        (_, Some(u)) => ("git", u.clone()),
        _ => {
            eprintln!("remove requires --local PATH or --git URL");
            exit(2);
        }
    };
    let mut v = read_profile_value(&pf);
    if let Some(arr) = v.get_mut("sources").and_then(|s| s.as_array_mut()) {
        let before = arr.len();
        arr.retain(|s| {
            let hit = match kind {
                "local" => s.pointer("/local/path").and_then(|p| p.as_str()) == Some(&target),
                _ => s.pointer("/git/url").and_then(|u| u.as_str()) == Some(&target),
            };
            !hit
        });
        let removed = before - arr.len();
        write_profile_value(&pf, &v);
        println!("removed {removed} source(s) from {}", pf.display());
    }
}

/// A human-readable summary of how a recorded lock differs from a fresh
/// resolution — which capabilities appeared, vanished, or moved. Enough to see
/// *what* went stale without diffing the file by hand.
fn lock_drift(recorded: &Lock, fresh: &Lock) -> Vec<String> {
    use std::collections::BTreeMap;

    fn index(lock: &Lock) -> BTreeMap<String, (String, String)> {
        lock.sources
            .iter()
            .flat_map(|s| &s.capabilities)
            .map(|c| (c.name.clone(), (c.version.clone(), c.digest.clone())))
            .collect()
    }
    let (old, new) = (index(recorded), index(fresh));
    let mut out = Vec::new();

    for (name, (version, digest)) in &new {
        match old.get(name) {
            None => out.push(format!("+ {name} {version} (not in the lock)")),
            Some((old_version, _)) if old_version != version => {
                out.push(format!("~ {name} {old_version} → {version}"))
            }
            Some((_, old_digest)) if old_digest != digest => {
                out.push(format!("~ {name} {version} — content changed"))
            }
            Some(_) => {}
        }
    }
    for name in old.keys().filter(|n| !new.contains_key(*n)) {
        out.push(format!("- {name} (locked, but no source provides it)"));
    }

    // Sources move independently of the capabilities they carry.
    for (a, b) in recorded.sources.iter().zip(&fresh.sources) {
        if a.origin == b.origin && a.rev != b.rev {
            out.push(format!(
                "~ {} @ {} → {}",
                a.origin,
                a.rev.as_deref().unwrap_or("-"),
                b.rev.as_deref().unwrap_or("-")
            ));
        }
    }
    if out.is_empty() {
        out.push("(source list or ordering changed)".to_string());
    }
    out
}

/// Load the capability manifest in `dir`, whichever spelling it uses.
fn load_manifest_in(dir: &std::path::Path) -> Result<LoadedCapability, String> {
    let path = open_harness::config::find(&dir.join(open_harness::manifest::MANIFEST_STEM))
        .ok_or_else(|| format!("no capability manifest in {}", dir.display()))?;
    LoadedCapability::load(&path)
}

fn read_profile_value(path: &std::path::Path) -> serde_json::Value {
    match std::fs::read_to_string(path) {
        Ok(t) => open_harness::config::parse(&t, open_harness::config::Format::of(path))
            .unwrap_or_else(|e| {
                eprintln!("invalid profile {}: {e}", path.display());
                exit(1);
            }),
        Err(_) => serde_json::json!({ "name": "default", "harnesses": [], "sources": [] }),
    }
}

fn write_profile_value(path: &std::path::Path, v: &serde_json::Value) {
    if let Err(e) = open_harness::config::write(path, v) {
        eprintln!("{e}");
        exit(1);
    }
}

fn add_harness(v: &mut serde_json::Value, harness: &str) {
    if let Some(arr) = v.as_object_mut().and_then(|m| {
        m.entry("harnesses")
            .or_insert(serde_json::json!([]))
            .as_array_mut()
    }) {
        if !arr.iter().any(|h| h.as_str() == Some(harness)) {
            arr.push(serde_json::json!(harness));
        }
    }
}

fn cmd_matrix(rest: &[String]) {
    let o = parse_opts(rest);
    // `--provenance` narrows the report to how the adapters were established.
    // It is not what *enables* that section: printing support without it is the
    // conflation this report exists to undo, so the full report always carries
    // both.
    if o.provenance {
        print!(
            "{}",
            if o.markdown {
                open_harness::matrix::provenance_markdown()
            } else {
                open_harness::matrix::provenance_text()
            }
        );
        return;
    }
    // The matrix is generated from the adapters (open_harness::matrix), the same
    // source the docs table is generated from — never hand-maintained.
    if o.markdown {
        print!("{}", open_harness::matrix::markdown());
        return;
    }
    let events = open_harness::matrix::representative_events();
    let width = 13;
    // The rows are derived from the event space now, so the widest id is not
    // known in advance — `pre.tool.file_write` already overflows any guess.
    let label = "event \\ harness";
    let first = events
        .iter()
        .map(|e| e.id().len())
        .chain(std::iter::once(label.len()))
        .max()
        .unwrap_or(18)
        + 2;
    print!("{:<first$}", label);
    for h in ALL {
        print!("{:<width$}", short(h.id()), width = width);
    }
    println!();
    println!("{}", "-".repeat(first + width * ALL.len()));
    for ev in &events {
        print!("{:<first$}", ev.id());
        for h in ALL {
            print!(
                "{:<width$}",
                open_harness::matrix::cell(h, ev),
                width = width
            );
        }
        println!();
    }
    println!("\nlegend: native = 1:1 · fanout×N = one normalized event registers on N native events · — = no target");
    println!("rows are every event at least one harness can host, derived from the adapters.");
    println!("session/subagent events are keyed by their boundary, so both phases resolve to the");
    println!(
        "same native event — `pre.session.end` and `post.session.end` are one target, not two."
    );
    println!();
    print!("{}", open_harness::matrix::provenance_text());
}

fn short(id: &str) -> String {
    match id {
        "claude-code" => "claude".to_string(),
        other => other.chars().take(11).collect(),
    }
}

/// `oh import` — a project's existing native config → portable capabilities.
///
/// The adoption ramp. Everything else here assumes a greenfield capability;
/// this reads the `.claude/`, `.cursor/`, `AGENTS.md` a project already has.
/// Read-only until it writes, and it never overwrites: an id that already
/// exists under `--out` is reported as a collision and left alone.
fn cmd_import(rest: &[String]) {
    let o = parse_opts(rest);
    let from = o.into.clone().unwrap_or_else(|| PathBuf::from("."));
    let out = o
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("capabilities"));
    let only = o.harness.as_deref().and_then(|h| match h {
        "all" => None,
        id => Some(Harness::from_id(id).unwrap_or_else(|| {
            eprintln!("unknown harness {id}");
            exit(2)
        })),
    });

    let imp = importer::scan(&from, only).unwrap_or_else(|e| {
        eprintln!("import failed: {e}");
        exit(1);
    });

    let tag = if o.dry_run { " (dry-run)" } else { "" };
    println!("import{tag} ← {}", from.display());
    for note in &imp.notes {
        println!("  {note}");
    }

    let width = imp
        .items
        .iter()
        .map(|i| i.id.len())
        .max()
        .unwrap_or(4)
        .max(4);
    for item in &imp.items {
        let mark = match &item.status {
            importer::Status::Portable => "＋",
            importer::Status::Native(_) => "◆",
            importer::Status::Skipped(_) => "－",
        };
        println!(
            "  {mark} {:<12} {:<width$}  ← {}",
            item.kind.as_str(),
            item.id,
            item.from
        );
        if item.harnesses.len() > 1 {
            println!("      read by: {}", item.harnesses.join(", "));
        }
        match &item.status {
            // "Not portable" is the whole point of saying it here: this one will
            // be Unsupported on every harness but the one it came from.
            importer::Status::Native(why) => println!("      NOT PORTABLE: {why}"),
            importer::Status::Skipped(why) => println!("      SKIPPED: {why}"),
            importer::Status::Portable => {}
        }
        for n in &item.notes {
            println!("      · {n}");
        }
    }

    let report = importer::write(&out, &imp, o.dry_run).unwrap_or_else(|e| {
        eprintln!("import failed: {e}");
        exit(1);
    });

    let skipped = imp
        .items
        .iter()
        .filter(|i| matches!(i.status, importer::Status::Skipped(_)))
        .count();
    let native = imp
        .items
        .iter()
        .filter(|i| matches!(i.status, importer::Status::Native(_)))
        .count();
    println!(
        "\n{} capabilit{} {} {}{}{}",
        report.created.len(),
        if report.created.len() == 1 {
            "y"
        } else {
            "ies"
        },
        if o.dry_run { "would go to" } else { "→" },
        out.display(),
        if native > 0 {
            format!("  ({native} not portable)")
        } else {
            String::new()
        },
        if skipped > 0 {
            format!("  ({skipped} skipped)")
        } else {
            String::new()
        }
    );
    if !report.collisions.is_empty() {
        println!(
            "\nleft alone — a capability with that id already exists under {}:",
            out.display()
        );
        for id in &report.collisions {
            println!("  · {id}");
        }
    }
    if !o.dry_run && !report.created.is_empty() {
        println!(
            "\nnext: `oh check --capabilities {}` to see how each one lands on every harness",
            out.display()
        );
    }
    // A skip is a thing the project has that open-harness now does not. Exiting
    // 0 would report a partial import as a complete one.
    if skipped > 0 {
        exit(1);
    }
}
