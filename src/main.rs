//! `oh` — the unified CLI.
//!
//! Authoring:  `init` (write a profile) · `scaffold` (a runnable capability
//!   starter) · `add`/`remove` (edit a profile's sources) · `migrate` (rewrite
//!   legacy JSON configs as YAML) · `install --runtimes` (run capabilities'
//!   declared provisioning commands, confirmed) · `doctor` (check the
//!   environment + capability health).
//! Runtime:    `run` (a harness's single native hook entrypoint) · `mcp`
//!   (list/call an MCP server's tools through the shell — the MCP→CLI bridge).
//! Delivery:   `emit` (native registration config) · `resolve` (sources → lock)
//!   · `sync` (install/converge into a project) · `check` (installability, or
//!   drift vs a synced project with `--ci`).
//! Trust:      `keygen` · `sign` · `verify` · `trust` · `revoke` · `keyring`
//!   (a root of trust: `keyring` + `trust --root`/`--keyring`).
//! Publishing: `pack` (capabilities → content-addressed archives + a registry
//!   index, ready to upload to a CDN).
//! Reporting:  `matrix` (the event × harness support grid; `--markdown`).

use open_harness::adapters::{Harness, ALL};
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
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
        _ => {
            eprintln!(
                "oh <init|migrate|install|scaffold|capture|add|remove|doctor|run|emit|keygen|sign|pack|verify|trust|revoke|keyring|mcp|resolve|sync|check|matrix|version> \\\n  [--harness H] [--event E] [--capabilities DIR] [--profile FILE] [--into DIR]\\\n  [--kind K] [--lang L] [--project] [--id ID] [--local PATH] [--git URL]\\\n  [--key FILE] [--trust FILE] [--label L] [--reason R] [--out FILE] [--require-signed] [--root] [--keyring FILE] [--deny-network] [--deny-exec]\\\n  [--command CMD] [--mcp-arg A]... [--url URL] [--base-url URL] [--header H]... [--tool NAME] [--json ARGS]\\\n  [--dry-run] [--uninstall] [--ci] [--locked] [--runtimes] [--yes] [--markdown] [--timeout-ms N] [--max-output-kb N] [--explain]\n\nauthoring: oh init · oh migrate (JSON configs → YAML) · oh install --runtimes · oh scaffold --kind hook --lang python --id my-guard · oh scaffold --project --id my-cap · oh doctor\npublish:   oh pack --capabilities capabilities --base-url https://cdn.example.com --out dist\nmcp:       oh mcp <list|call> (--id ID | --command CMD [--mcp-arg A]... | --url URL [--header 'K: V']...) [--tool NAME] [--json '<args>']"
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
    /// Treat the lockfile as a contract: never rewrite it, fail if it is stale.
    locked: bool,
    /// Confirm an action that executes code (`install --runtimes`).
    yes: bool,
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
        base_url: None,
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
            "--reason" => {
                o.reason = rest.get(i + 1).cloned();
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
            "--root" => {
                o.root = true;
                i += 1;
            }
            "--keyring" => {
                o.keyring = rest.get(i + 1).map(PathBuf::from);
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
            "--id" => {
                o.id = rest.get(i + 1).cloned();
                i += 2;
            }
            "--command" => {
                o.mcp_command = rest.get(i + 1).cloned();
                i += 2;
            }
            "--mcp-arg" => {
                if let Some(a) = rest.get(i + 1) {
                    o.mcp_args.push(a.clone());
                }
                i += 2;
            }
            "--url" => {
                o.mcp_url = rest.get(i + 1).cloned();
                i += 2;
            }
            "--base-url" => {
                o.base_url = rest.get(i + 1).cloned();
                i += 2;
            }
            "--header" => {
                // `--header "Name: value"` (repeatable), for http MCP endpoints.
                if let Some(h) = rest.get(i + 1) {
                    if let Some((k, v)) = h.split_once(':') {
                        o.mcp_headers
                            .push((k.trim().to_string(), v.trim().to_string()));
                    }
                }
                i += 2;
            }
            "--tool" => {
                o.tool = rest.get(i + 1).cloned();
                i += 2;
            }
            "--json" => {
                o.json_args = rest.get(i + 1).cloned();
                i += 2;
            }
            "--kind" => {
                o.scaffold_kind = rest.get(i + 1).cloned();
                i += 2;
            }
            "--lang" => {
                o.scaffold_lang = rest.get(i + 1).cloned();
                i += 2;
            }
            "--project" => {
                o.scaffold_project = true;
                i += 1;
            }
            "--local" => {
                o.local = rest.get(i + 1).cloned();
                i += 2;
            }
            "--git" => {
                o.git = rest.get(i + 1).cloned();
                i += 2;
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
    let Some(profile_path) = o.profile.clone() else {
        eprintln!("resolve requires --profile <file>");
        exit(2);
    };
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
        if report.has_blocked() {
            eprintln!("\nsome files were left in place: exiting non-zero");
            exit(1);
        }
        return;
    }

    // Profile mode (#15): resolve sources + harnesses from the profile; else the
    // ad-hoc `--capabilities` / `--harness` mode.
    let (caps, harnesses) = if let Some(profile_path) = o.profile.clone() {
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

    let plan = sync::plan_sync(&caps, &harnesses);
    let report = sync::apply(&into, &plan, o.dry_run).unwrap_or_else(|e| {
        eprintln!("sync failed: {e}");
        exit(1);
    });
    print_apply_report(&report, "sync", &into);
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
    if let Some(into) = o.into.clone() {
        let (caps, harnesses) = if let Some(profile_path) = o.profile.clone() {
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
    // Refuse if a profile already exists in *any* accepted spelling, so `init`
    // never quietly shadows a `open-harness.json` that is still being read.
    if let Some(existing) = open_harness::config::find(&dir.join(profile::PROFILE_STEM)) {
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
        "sources": [ { "local": { "path": "capabilities" } } ],
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
    if !rest.iter().any(|a| a == "--runtimes") {
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

fn cmd_add(rest: &[String]) {
    let o = parse_opts(rest);
    let Some(pf) = o.profile.clone() else {
        eprintln!("add requires --profile FILE");
        exit(2);
    };
    let source = if let Some(p) = &o.local {
        serde_json::json!({ "local": { "path": p } })
    } else if let Some(u) = &o.git {
        serde_json::json!({ "git": { "url": u, "rev": "HEAD" } })
    } else {
        eprintln!("add requires --local PATH or --git URL");
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
    let Some(pf) = o.profile.clone() else {
        eprintln!("remove requires --profile FILE");
        exit(2);
    };
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
    // The matrix is generated from the adapters (open_harness::matrix), the same
    // source the docs table is generated from — never hand-maintained.
    if rest.iter().any(|a| a == "--markdown") {
        print!("{}", open_harness::matrix::markdown());
        return;
    }
    let events = open_harness::matrix::representative_events();
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
            print!(
                "{:<width$}",
                open_harness::matrix::cell(h, ev),
                width = width
            );
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
