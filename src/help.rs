//! `oh`'s help text, as data.
//!
//! The CLI grew to 27 commands sharing one flag namespace, and the only help
//! was a single block listing every flag at once — so a reader could not tell
//! which of ~40 flags applied to the command they were running. This module
//! holds one [`Command`] entry per subcommand (summary, synopsis, the flags
//! *that command* reads, examples), and three consumers render from it:
//! `oh help`, `oh <cmd> --help`, and `oh completions <shell>`.
//!
//! Keeping it as data rather than `println!`s inside each `cmd_*` is what makes
//! the shell completions possible at all — they need the flag set per command,
//! not a paragraph. It also lets `tests/help.rs` assert that every arm of
//! `main`'s dispatch has an entry here, so a new command cannot ship
//! undocumented.

/// One option, as it appears in a synopsis (`--into DIR`) plus what it does.
/// The flag name is the text up to the first space, which is what completions
/// need; the rest is the value placeholder.
pub struct Flag {
    pub spec: &'static str,
    pub help: &'static str,
}

impl Flag {
    /// Just the `--flag` part, for completion candidates.
    pub fn name(&self) -> &'static str {
        match self.spec.split_once(' ') {
            Some((n, _)) => n,
            None => self.spec,
        }
    }
    /// Whether the flag takes a value (so completion doesn't offer a flag next).
    pub fn takes_value(&self) -> bool {
        self.spec.contains(' ')
    }
}

pub struct Example {
    pub line: &'static str,
    pub what: &'static str,
}

pub struct Command {
    pub name: &'static str,
    /// Which section of the overview this appears under; must be in [`GROUPS`].
    pub group: &'static str,
    /// One line, shown in the overview. No trailing period.
    pub summary: &'static str,
    /// Usage lines, without the leading `oh`.
    pub synopsis: &'static [&'static str],
    /// Prose shown only in the per-command help. May be empty.
    pub details: &'static str,
    pub flags: &'static [Flag],
    pub examples: &'static [Example],
}

/// Overview section order.
pub const GROUPS: &[&str] = &[
    "authoring",
    "compose & install",
    "runtime",
    "trust",
    "publishing",
    "reporting",
];

// ---------------------------------------------------------------------------
// Flags shared by several commands. Declared once so their wording can't drift
// between the commands that accept them.
// ---------------------------------------------------------------------------

const F_CAPABILITIES: Flag = Flag {
    spec: "--capabilities DIR",
    help: "where to scan for capabilities (default: ./capabilities)",
};
const F_PROFILE: Flag = Flag {
    spec: "--profile FILE",
    help: "the profile to use (default: open-harness.yaml found here)",
};
const F_HARNESS: Flag = Flag {
    spec: "--harness H",
    help: "target one harness by id, or `all` (default: all)",
};
const F_INTO: Flag = Flag {
    spec: "--into DIR",
    help: "the project directory to act on",
};
const F_LOCKED: Flag = Flag {
    spec: "--locked",
    help: "treat open-harness.lock as a contract: verify, never rewrite (CI)",
};
const F_DRY_RUN: Flag = Flag {
    spec: "--dry-run",
    help: "report what would change; write nothing",
};
const F_CAPABILITY: Flag = Flag {
    spec: "--capability DIR",
    help: "a single capability directory",
};
const F_KEY: Flag = Flag {
    spec: "--key FILE",
    help: "an ed25519 keyfile from `oh keygen`",
};
const F_TRUST: Flag = Flag {
    spec: "--trust FILE",
    help: "the trust store to read/write (e.g. trust.yaml)",
};
const F_REQUIRE_SIGNED: Flag = Flag {
    spec: "--require-signed",
    help: "refuse anything that does not verify as Trusted",
};
const F_DENY_NETWORK: Flag = Flag {
    spec: "--deny-network",
    help: "refuse capabilities that declare network access",
};
const F_DENY_EXEC: Flag = Flag {
    spec: "--deny-exec",
    help: "refuse capabilities that declare exec access",
};
const F_DENY_WRITE: Flag = Flag {
    spec: "--deny-write",
    help: "refuse capabilities that declare filesystem writes",
};

/// Every subcommand `main` dispatches, in overview order within each group.
pub const COMMANDS: &[Command] = &[
    // ---------------------------------------------------------------- authoring
    Command {
        name: "init",
        group: "authoring",
        summary: "write an open-harness.yaml profile",
        synopsis: &["init [--into DIR] [--harness H]"],
        details: "Creates the profile that lists your sources and target harnesses. \
                  Refuses to clobber a profile that already exists.",
        flags: &[
            F_INTO,
            Flag {
                spec: "--harness H",
                help: "seed the profile's harness list with this one (default: all)",
            },
        ],
        examples: &[Example {
            line: "oh init",
            what: "write ./open-harness.yaml",
        }],
    },
    Command {
        name: "import",
        group: "authoring",
        summary: "lift a project's existing native config into capabilities",
        synopsis: &["import [--harness H] [--into DIR] [--out DIR] [--dry-run]"],
        details: "Reads the harness config a project already has — skills, rules, \
                  commands, subagents, instructions files — and writes each one back \
                  out as a portable capability, so adopting open-harness does not mean \
                  re-authoring your setup first.\n\n\
                  Honest by design: everything found is either imported or reported \
                  with the reason it could not be. Native hooks are carried as \
                  `native` capabilities pinned to their harness (their commands speak \
                  that harness's schema, not `hook@1`), never silently reshaped into \
                  portable hooks.\n\n\
                  Nothing is overwritten: an id that already exists under --out is \
                  reported as a collision and skipped.",
        flags: &[
            Flag {
                spec: "--harness H",
                help: "import only this harness's config (default: every harness)",
            },
            Flag {
                spec: "--into DIR",
                help: "the project to read config from (default: .)",
            },
            Flag {
                spec: "--out DIR",
                help: "where to write capabilities (default: ./capabilities)",
            },
            F_DRY_RUN,
        ],
        examples: &[
            Example {
                line: "oh import --dry-run",
                what: "show what this project's config would become",
            },
            Example {
                line: "oh import --harness cursor --out capabilities",
                what: "import just .cursor/ into portable capabilities",
            },
        ],
    },
    Command {
        name: "scaffold",
        group: "authoring",
        summary: "generate a runnable capability starter",
        synopsis: &[
            "scaffold --kind K [--lang L] --id ID [--out DIR]",
            "scaffold --project --id ID [--out DIR]",
        ],
        details: "`--kind` is one of hook, skill, rule, command, tool, permission, \
                  agent, instructions. `--lang` (hook only) is py, node, typescript or \
                  bash. `--project` writes a typed TypeScript capability as an npm \
                  package instead of a bare directory.",
        flags: &[
            Flag {
                spec: "--kind K",
                help: "hook | skill | rule | command | tool | permission | agent | instructions",
            },
            Flag {
                spec: "--lang L",
                help: "py | node | typescript | bash (hooks only)",
            },
            Flag {
                spec: "--id ID",
                help: "the capability id (also its directory name)",
            },
            Flag {
                spec: "--project",
                help: "scaffold a typed TypeScript npm package",
            },
            Flag {
                spec: "--out DIR",
                help: "parent directory to write into (default: ./capabilities)",
            },
        ],
        examples: &[
            Example {
                line: "oh scaffold --kind hook --lang python --id my-guard",
                what: "a runnable Python hook",
            },
            Example {
                line: "oh scaffold --kind agent --id db-engineer",
                what: "a subagent capability",
            },
        ],
    },
    Command {
        name: "add",
        group: "authoring",
        summary: "add a source to the profile",
        synopsis: &[
            "add <spec>",
            "add --local PATH",
            "add --git URL",
            "add --harness H",
        ],
        details: "The source kind is inferred from the spec: a bare path is local, \
                  `git+https://…@rev` is a repo, `https://…` is an archive or a \
                  registry index. `--harness` adds a harness to the profile's target \
                  list instead of a source.",
        flags: &[
            Flag {
                spec: "--local PATH",
                help: "add a local directory as a source",
            },
            Flag {
                spec: "--git URL",
                help: "add a git source (accepts @rev and #subdirectory=)",
            },
            Flag {
                spec: "--harness H",
                help: "add a target harness rather than a source",
            },
            F_PROFILE,
        ],
        examples: &[Example {
            line: "oh add git+https://github.com/me/my-skill@v1.2.0",
            what: "pin a capability repo at a tag",
        }],
    },
    Command {
        name: "remove",
        group: "authoring",
        summary: "remove a source from the profile",
        synopsis: &["remove --local PATH", "remove --git URL"],
        details: "",
        flags: &[
            Flag {
                spec: "--local PATH",
                help: "the local source to drop",
            },
            Flag {
                spec: "--git URL",
                help: "the git source to drop",
            },
            F_PROFILE,
        ],
        examples: &[],
    },
    Command {
        name: "migrate",
        group: "authoring",
        summary: "rewrite legacy JSON configs as YAML",
        synopsis: &["migrate [--into DIR] [--dry-run]"],
        details: "Converts open-harness's own `*.json` config files (capability.json, \
                  open-harness.json, the lockfile) to their YAML spelling. It does not \
                  touch a harness's native config — see `oh import` for that.",
        flags: &[F_INTO, F_DRY_RUN],
        examples: &[],
    },
    Command {
        name: "capture",
        group: "authoring",
        summary: "record a harness's real native hook payload as a fixture",
        synopsis: &["capture --harness H --event E --out FILE"],
        details: "Wire this as the harness's hook entrypoint and trigger the event \
                  once. It writes the raw stdin it received to --out, then allows \
                  (exit 0) so nothing is disrupted. A `provenance.json` sidecar is \
                  written next to the fixture recording that it came from a live \
                  capture — that is what upgrades an adapter from doc-derived to \
                  live-verified in `oh matrix --provenance`.",
        flags: &[
            F_HARNESS,
            Flag {
                spec: "--event E",
                help: "the normalized event, e.g. pre.tool.shell",
            },
            Flag {
                spec: "--out FILE",
                help: "the fixture file to write",
            },
            Flag {
                spec: "--label L",
                help: "the harness version this was recorded from (kept in the sidecar)",
            },
        ],
        examples: &[Example {
            line: "oh capture --harness cursor --event pre.tool.shell --out fixture.json",
            what: "record what Cursor really sends",
        }],
    },
    Command {
        name: "doctor",
        group: "authoring",
        summary: "check interpreters, runtimes and capability health",
        synopsis: &["doctor [--capabilities DIR]"],
        details: "Exits non-zero when it finds an issue, so it is usable as a CI gate.",
        flags: &[F_CAPABILITIES],
        examples: &[],
    },
    // -------------------------------------------------------- compose & install
    Command {
        name: "resolve",
        group: "compose & install",
        summary: "resolve the profile's sources into a pinned lockfile",
        synopsis: &["resolve [--profile FILE] [--locked]"],
        details: "Fetches and composes every source, then writes open-harness.lock \
                  pinning each capability and each source's exact revision. With \
                  --locked it verifies the existing lockfile instead of rewriting it \
                  and fails if it is stale.",
        flags: &[
            F_PROFILE,
            F_LOCKED,
            F_TRUST,
            F_REQUIRE_SIGNED,
            F_DENY_NETWORK,
            F_DENY_EXEC,
            F_DENY_WRITE,
        ],
        examples: &[Example {
            line: "oh resolve --locked",
            what: "CI: fail if the lockfile does not match the profile",
        }],
    },
    Command {
        name: "sync",
        group: "compose & install",
        summary: "install capabilities into a project and converge them",
        synopsis: &[
            "sync --into DIR [--profile FILE] [--locked] [--dry-run]",
            "sync --into DIR --capabilities DIR [--harness H]",
            "sync --into DIR --uninstall",
        ],
        details: "Idempotent: re-running converges rather than duplicating. Files \
                  open-harness did not write are never clobbered — a target that \
                  belongs to you is reported as blocked and sync exits non-zero. \
                  --uninstall removes only what the lockfile records as ours.",
        flags: &[
            F_INTO,
            F_PROFILE,
            F_CAPABILITIES,
            F_HARNESS,
            F_LOCKED,
            F_DRY_RUN,
            Flag {
                spec: "--uninstall",
                help: "remove what open-harness installed here",
            },
            F_TRUST,
            F_REQUIRE_SIGNED,
            F_DENY_NETWORK,
            F_DENY_EXEC,
            F_DENY_WRITE,
        ],
        examples: &[
            Example {
                line: "oh sync --into ./project",
                what: "install everything the profile resolves to",
            },
            Example {
                line: "oh sync --into ./project --uninstall",
                what: "remove it again, leaving your own files alone",
            },
        ],
    },
    Command {
        name: "check",
        group: "compose & install",
        summary: "check installability, or drift against a synced project",
        synopsis: &[
            "check [--capabilities DIR] [--harness H]",
            "check --into DIR [--ci]",
        ],
        details: "Without --into, reports per-capability installability across the \
                  target harnesses (clean / degraded / unsupported). With --into, it \
                  compares the project on disk against what the profile says should be \
                  there; --ci makes drift a non-zero exit.",
        flags: &[
            F_INTO,
            F_PROFILE,
            F_CAPABILITIES,
            F_HARNESS,
            F_LOCKED,
            Flag {
                spec: "--ci",
                help: "exit non-zero on drift",
            },
        ],
        examples: &[Example {
            line: "oh check --into ./project --ci",
            what: "CI: fail if the project has drifted",
        }],
    },
    Command {
        name: "install",
        group: "compose & install",
        summary: "run capabilities' declared provisioning commands",
        synopsis: &["install --runtimes [--yes] [--profile FILE]"],
        details: "Capabilities may declare how to provision their runtime (a pip \
                  install, an npm ci). This shows those commands and, with --yes, runs \
                  them. It executes third-party code, so it never runs without \
                  confirmation.",
        flags: &[
            Flag {
                spec: "--runtimes",
                help: "provision declared runtimes (required)",
            },
            Flag {
                spec: "--yes",
                help: "actually run the commands (otherwise they are only shown)",
            },
            F_PROFILE,
            F_LOCKED,
            F_REQUIRE_SIGNED,
            F_DENY_NETWORK,
        ],
        examples: &[Example {
            line: "oh install --runtimes",
            what: "show what would be run",
        }],
    },
    // ------------------------------------------------------------------ runtime
    Command {
        name: "run",
        group: "runtime",
        summary: "a harness's single native hook entrypoint",
        synopsis: &["run --harness H --event E [--capabilities DIR]"],
        details: "Reads the harness's native payload on stdin, dispatches every bound \
                  capability concurrently, merges the decisions (deny wins) and \
                  replies in that harness's native format. This is what you register \
                  as the hook command.",
        flags: &[
            F_HARNESS,
            Flag {
                spec: "--event E",
                help: "the normalized event, e.g. pre.tool.any",
            },
            F_CAPABILITIES,
            Flag {
                spec: "--timeout-ms N",
                help: "per-capability timeout",
            },
            Flag {
                spec: "--max-output-kb N",
                help: "per-capability stdout cap",
            },
            Flag {
                spec: "--explain",
                help: "write the merge reasoning to stderr",
            },
        ],
        examples: &[Example {
            line: "oh run --harness claude-code --event pre.tool.any",
            what: "the command to register as Claude's PreToolUse hook",
        }],
    },
    Command {
        name: "emit",
        group: "runtime",
        summary: "print the native registration config for a harness",
        synopsis: &["emit [--harness H] [--capabilities DIR]"],
        details: "Shows exactly what to put in the harness's own config to register \
                  the capabilities — including a loud note wherever one normalized \
                  event has to fan out onto several native ones.",
        flags: &[F_HARNESS, F_CAPABILITIES],
        examples: &[Example {
            line: "oh emit --harness cursor",
            what: "Cursor's hooks.json, with the fan-out noted",
        }],
    },
    Command {
        name: "mcp",
        group: "runtime",
        summary: "call an MCP server's tools through the shell",
        synopsis: &[
            "mcp list (--id ID | --command CMD [--mcp-arg A]... | --url URL [--header 'K: V']...)",
            "mcp call --tool NAME [--json ARGS] (--id ID | --command CMD | --url URL)",
        ],
        details: "The MCP→CLI bridge, for orgs that disable the harness's MCP client: \
                  `oh mcp` speaks MCP to the server itself and returns the result on \
                  stdout. `https://` needs the optional `http-tls` feature.",
        flags: &[
            Flag {
                spec: "--id ID",
                help: "a tool capability that declares the server",
            },
            Flag {
                spec: "--command CMD",
                help: "spawn a stdio MCP server",
            },
            Flag {
                spec: "--mcp-arg A",
                help: "an argument for --command (repeatable)",
            },
            Flag {
                spec: "--url URL",
                help: "a streamable-HTTP MCP endpoint",
            },
            Flag {
                spec: "--header H",
                help: "\"Name: value\" for --url (repeatable)",
            },
            Flag {
                spec: "--tool NAME",
                help: "the tool to call",
            },
            Flag {
                spec: "--json ARGS",
                help: "the tool's arguments, as a JSON object",
            },
        ],
        examples: &[Example {
            line: "oh mcp list --command npx --mcp-arg -y --mcp-arg @modelcontextprotocol/server-everything",
            what: "list a stdio server's tools",
        }],
    },
    // -------------------------------------------------------------------- trust
    Command {
        name: "keygen",
        group: "trust",
        summary: "generate an ed25519 keypair",
        synopsis: &["keygen --out FILE [--label L]"],
        details: "",
        flags: &[
            Flag {
                spec: "--out FILE",
                help: "where to write the keyfile (keep it secret)",
            },
            Flag {
                spec: "--label L",
                help: "a human name recorded with the key",
            },
        ],
        examples: &[Example {
            line: "oh keygen --out author.key --label me",
            what: "",
        }],
    },
    Command {
        name: "sign",
        group: "trust",
        summary: "sign a capability's content digest",
        synopsis: &["sign --capability DIR --key FILE"],
        details: "Writes a detached signature over a sha256 digest of the capability's \
                  whole file tree, so any later edit invalidates it.",
        flags: &[F_CAPABILITY, F_KEY],
        examples: &[],
    },
    Command {
        name: "verify",
        group: "trust",
        summary: "report a capability's trust verdict",
        synopsis: &["verify --capability DIR [--trust FILE] [--require-signed]"],
        details: "One of Trusted, Untrusted, Revoked, Invalid or Unsigned. \
                  --require-signed makes anything but Trusted a non-zero exit.",
        flags: &[F_CAPABILITY, F_TRUST, F_REQUIRE_SIGNED],
        examples: &[],
    },
    Command {
        name: "trust",
        group: "trust",
        summary: "pin an author key, or a root of trust",
        synopsis: &[
            "trust --capability DIR --trust FILE",
            "trust --root --key FILE --trust FILE [--label L]",
            "trust --keyring FILE --trust FILE",
        ],
        details: "Without --root this is trust-on-first-use: it pins whatever key \
                  signed the capability. With --root it pins a root key that can \
                  vouch for many authors through a keyring.",
        flags: &[
            F_CAPABILITY,
            F_TRUST,
            Flag {
                spec: "--root",
                help: "pin a root key rather than an author key",
            },
            F_KEY,
            Flag {
                spec: "--keyring FILE",
                help: "adopt the authors a trusted root vouches for",
            },
            Flag {
                spec: "--label L",
                help: "a human name for the pinned key",
            },
        ],
        examples: &[],
    },
    Command {
        name: "revoke",
        group: "trust",
        summary: "withdraw a key so it never verifies again",
        synopsis: &["revoke --capability DIR --trust FILE --reason R"],
        details: "Revocation outranks everything: a revoked key fails even in advisory \
                  mode, and revoking a root withdraws every author it vouched for.",
        flags: &[
            F_CAPABILITY,
            F_TRUST,
            Flag {
                spec: "--reason R",
                help: "why it was withdrawn (recorded in the store)",
            },
        ],
        examples: &[],
    },
    Command {
        name: "keyring",
        group: "trust",
        summary: "sign a keyring vouching for an author",
        synopsis: &["keyring --key FILE --capability DIR --out FILE [--label L]"],
        details: "Run by the holder of a root key. Consumers then `oh trust --keyring` \
                  it, and every author in it becomes trusted without pinning each one.",
        flags: &[
            F_KEY,
            F_CAPABILITY,
            Flag {
                spec: "--out FILE",
                help: "the keyring file to write",
            },
            Flag {
                spec: "--label L",
                help: "a human name for the vouched author",
            },
        ],
        examples: &[],
    },
    // --------------------------------------------------------------- publishing
    Command {
        name: "pack",
        group: "publishing",
        summary: "build content-addressed archives and a registry index",
        synopsis: &["pack --capabilities DIR --base-url URL --out DIR"],
        details: "Produces a deterministic tarball per capability plus an index that \
                  points at them, ready to upload to a CDN. Identical content always \
                  yields an identical digest.",
        flags: &[
            F_CAPABILITIES,
            Flag {
                spec: "--base-url URL",
                help: "where the archives will be served from",
            },
            Flag {
                spec: "--out DIR",
                help: "the output directory",
            },
        ],
        examples: &[Example {
            line: "oh pack --capabilities capabilities --base-url https://cdn.example.com --out dist",
            what: "",
        }],
    },
    // ---------------------------------------------------------------- reporting
    Command {
        name: "matrix",
        group: "reporting",
        summary: "the event × harness support grid",
        synopsis: &["matrix [--markdown] [--provenance]"],
        details: "What each normalized event becomes on each harness: native, a \
                  fan-out onto N native events, or no target at all.\n\n\
                  The report always ends with adapter provenance — whether each \
                  adapter was live-captured from a real install, built into a fixture \
                  from the vendor's docs, or encoded from documentation alone. \
                  \"Supported\" and \"verified\" are different claims, and a grid that \
                  shows only the first reads as more proven than it is. \
                  --provenance narrows the output to just that section.",
        flags: &[
            Flag {
                spec: "--markdown",
                help: "emit a GitHub-flavored Markdown table",
            },
            Flag {
                spec: "--provenance",
                help: "print only the provenance report, not the support grid",
            },
        ],
        examples: &[],
    },
    Command {
        name: "completions",
        group: "reporting",
        summary: "print a shell completion script",
        synopsis: &["completions <bash|zsh|fish>"],
        details: "Generated from the same command table as this help, so completions \
                  cannot drift from the CLI.",
        flags: &[],
        examples: &[Example {
            line: "oh completions bash > /etc/bash_completion.d/oh",
            what: "",
        }],
    },
    Command {
        name: "version",
        group: "reporting",
        summary: "the binary and protocol version",
        synopsis: &["version"],
        details: "The protocol version is what a consumer negotiates against; it moves \
                  independently of the binary's.",
        flags: &[],
        examples: &[],
    },
    Command {
        name: "help",
        group: "reporting",
        summary: "this overview, or help for one command",
        synopsis: &["help [command]"],
        details: "",
        flags: &[],
        examples: &[Example {
            line: "oh help sync",
            what: "",
        }],
    },
];

/// Look up a command by name. Also accepts the aliases `main` dispatches.
pub fn command(name: &str) -> Option<&'static Command> {
    let canonical = match name {
        "--version" | "-V" => "version",
        "--help" | "-h" => "help",
        other => other,
    };
    COMMANDS.iter().find(|c| c.name == canonical)
}

/// The overview: every command, grouped, one line each.
pub fn overview() -> String {
    let mut out = String::new();
    out.push_str("oh — author an AI-coding-agent capability once, run it on every harness.\n\n");
    out.push_str("usage:\n");
    out.push_str("  oh <command> [options]\n");
    out.push_str("  oh <command> --help      what that command does, and its options\n");
    out.push_str("  oh help                  this overview\n");

    let width = COMMANDS.iter().map(|c| c.name.len()).max().unwrap_or(8);
    for group in GROUPS {
        let mut any = false;
        for c in COMMANDS.iter().filter(|c| c.group == *group) {
            if !any {
                out.push_str(&format!("\n{group}\n"));
                any = true;
            }
            out.push_str(&format!("  {:width$}  {}\n", c.name, c.summary));
        }
    }
    out.push_str("\nEvery command takes --help. Start with `oh init`, then `oh sync --into .`.\n");
    out
}

/// The full help for one command.
pub fn command_help(c: &Command) -> String {
    let mut out = format!("oh {} — {}\n\nusage:\n", c.name, c.summary);
    for line in c.synopsis {
        out.push_str(&format!("  oh {line}\n"));
    }
    if !c.details.is_empty() {
        out.push('\n');
        out.push_str(&wrap(c.details, 78));
    }
    if !c.flags.is_empty() {
        out.push_str("\noptions:\n");
        let width = c.flags.iter().map(|f| f.spec.len()).max().unwrap_or(0);
        for f in c.flags {
            out.push_str(&format!("  {:width$}  {}\n", f.spec, f.help));
        }
    }
    if !c.examples.is_empty() {
        out.push_str("\nexamples:\n");
        for e in c.examples {
            out.push_str(&format!("  {}\n", e.line));
            if !e.what.is_empty() {
                out.push_str(&format!("      {}\n", e.what));
            }
        }
    }
    out
}

/// Wrap prose to `cols`, preserving blank-line paragraph breaks.
fn wrap(text: &str, cols: usize) -> String {
    let mut out = String::new();
    for (n, para) in text.split("\n\n").enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.len() + 1 + word.len() > cols {
                out.push_str(&line);
                out.push('\n');
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// The closest command name to `unknown`, if one is close enough to be worth
/// suggesting. A typo should point at the command meant, not just fail.
pub fn suggest(unknown: &str) -> Option<&'static str> {
    // Distance 1 for very short names, else 2 — otherwise `add` "suggests" `pack`.
    let budget = if unknown.len() <= 4 { 1 } else { 2 };
    COMMANDS
        .iter()
        .map(|c| (c.name, distance(unknown, c.name)))
        .filter(|(_, d)| *d <= budget)
        .min_by_key(|(_, d)| *d)
        .map(|(n, _)| n)
}

/// Optimal string alignment distance — Levenshtein plus a **transposition** of
/// two adjacent characters at cost 1. Plain Levenshtein scores `snyc` → `sync`
/// as 2 and so would not suggest it, which misses the single most common way a
/// command name gets mistyped.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // Full matrix: a transposition needs the row two back, not just one.
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// The shells `oh completions` can emit for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    pub fn parse(s: &str) -> Option<Shell> {
        Some(match s {
            "bash" => Shell::Bash,
            "zsh" => Shell::Zsh,
            "fish" => Shell::Fish,
            _ => return None,
        })
    }
}

/// A completion script, generated from [`COMMANDS`].
pub fn completions(shell: Shell) -> String {
    let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
    match shell {
        Shell::Bash => bash(&names),
        Shell::Zsh => zsh(&names),
        Shell::Fish => fish(),
    }
}

fn bash(names: &[&str]) -> String {
    let mut s = String::from(
        "# oh completions (bash) — generated by `oh completions bash`.\n_oh() {\n  local cur prev cmd\n  cur=\"${COMP_WORDS[COMP_CWORD]}\"\n  cmd=\"${COMP_WORDS[1]}\"\n  if [ \"$COMP_CWORD\" -eq 1 ]; then\n",
    );
    s.push_str(&format!(
        "    COMPREPLY=($(compgen -W \"{}\" -- \"$cur\"))\n    return\n  fi\n  case \"$cmd\" in\n",
        names.join(" ")
    ));
    for c in COMMANDS {
        let flags: Vec<&str> = c.flags.iter().map(|f| f.name()).collect();
        s.push_str(&format!(
            "    {})\n      COMPREPLY=($(compgen -W \"{} --help\" -- \"$cur\"))\n      ;;\n",
            c.name,
            flags.join(" ")
        ));
    }
    s.push_str("  esac\n}\ncomplete -F _oh oh\n");
    s
}

fn zsh(names: &[&str]) -> String {
    let mut s = String::from(
        "#compdef oh\n# oh completions (zsh) — generated by `oh completions zsh`.\n_oh() {\n  local -a cmds\n  cmds=(\n",
    );
    for c in COMMANDS {
        s.push_str(&format!(
            "    '{}:{}'\n",
            c.name,
            c.summary.replace('\'', "")
        ));
    }
    s.push_str("  )\n  if (( CURRENT == 2 )); then\n    _describe -t commands 'oh command' cmds\n    return\n  fi\n  case \"${words[2]}\" in\n");
    for c in COMMANDS {
        let flags: Vec<String> = c
            .flags
            .iter()
            .map(|f| format!("'{}[{}]'", f.name(), f.help.replace(['\'', '[', ']'], "")))
            .collect();
        s.push_str(&format!(
            "    {})\n      _arguments {} '--help[show help for this command]'\n      ;;\n",
            c.name,
            if flags.is_empty() {
                String::new()
            } else {
                flags.join(" ")
            }
        ));
    }
    s.push_str("  esac\n}\n_oh \"$@\"\n");
    let _ = names;
    s
}

fn fish() -> String {
    let mut s = String::from(
        "# oh completions (fish) — generated by `oh completions fish`.\ncomplete -c oh -f\n",
    );
    for c in COMMANDS {
        s.push_str(&format!(
            "complete -c oh -n '__fish_use_subcommand' -a {} -d '{}'\n",
            c.name,
            c.summary.replace('\'', "")
        ));
    }
    for c in COMMANDS {
        for f in c.flags {
            s.push_str(&format!(
                "complete -c oh -n '__fish_seen_subcommand_from {}' -l {} -d '{}'{}\n",
                c.name,
                f.name().trim_start_matches("--"),
                f.help.replace('\'', ""),
                if f.takes_value() { " -r" } else { "" }
            ));
        }
    }
    s
}
