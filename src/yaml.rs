//! YAML frontmatter: emit (hand-rolled) and read (via `yaml-rust2`).
//!
//! The generative kinds *emit* YAML frontmatter (`---\nkey: value\n---`). Emission
//! is hand-rolled: it quotes exactly the values that need it and renders arbitrary
//! JSON values (from per-harness `overrides`) as YAML nodes — a small, fully
//! controlled output surface.
//!
//! *Reading* single-file frontmatter is the inverse, and YAML input is a genuinely
//! complex spec (single/double quotes and their escapes, block vs flow nesting,
//! anchors, multi-line scalars). We hand-roll the *simple* wire formats (JSON-RPC,
//! HTTP, hex) but not this — the reader delegates to `yaml-rust2`, a pure-Rust
//! YAML 1.2 parser, and converts its output to `serde_json::Value`.

use serde_json::{Map, Value};

/// Emit `s` as a YAML scalar safe to place after `key: `. Plain when unambiguous,
/// double-quoted (with escapes) when a bare scalar would be misparsed.
pub fn scalar(s: &str) -> String {
    if needs_quoting(s) {
        quote(s)
    } else {
        s.to_string()
    }
}

fn needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    // Leading/trailing whitespace is lost on a bare scalar.
    if s != s.trim() {
        return true;
    }
    // A reserved indicator in the first position changes the node's meaning.
    let first = s.chars().next().unwrap();
    if "!&*?|>%@`\"'#,[]{}-:".contains(first) {
        return true;
    }
    // `: ` opens a mapping; a trailing `:` is a key; ` #` opens a comment.
    if s.contains(": ") || s.ends_with(':') || s.contains(" #") {
        return true;
    }
    // Bool / null / number lookalikes must be quoted to stay strings.
    let low = s.to_ascii_lowercase();
    if matches!(
        low.as_str(),
        "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
    ) {
        return true;
    }
    if s.parse::<f64>().is_ok() {
        return true;
    }
    // Control characters (newlines, tabs) can't sit in a bare scalar.
    s.chars().any(|c| c.is_control())
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render any JSON value as a YAML node (scalar, flow list, or flow map). Used to
/// emit per-harness `overrides` whose values are arbitrary JSON.
pub fn render_value(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => scalar(s),
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(render_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(o) => {
            let items: Vec<String> = o
                .iter()
                .map(|(k, val)| format!("{k}: {}", render_value(val)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// A flow sequence `[a, b, c]` with each item scalar-quoted as needed.
pub fn flow_list(items: &[String]) -> String {
    let rendered: Vec<String> = items.iter().map(|s| scalar(s)).collect();
    format!("[{}]", rendered.join(", "))
}

/// Assemble a frontmatter block from ordered, pre-rendered `key: value` pairs.
pub fn frontmatter(pairs: &[(String, String)]) -> String {
    let mut s = String::from("---\n");
    for (k, v) in pairs {
        s.push_str(k);
        s.push_str(": ");
        s.push_str(v);
        s.push('\n');
    }
    s.push_str("---\n");
    s
}

/// Merge per-harness `overrides` frontmatter into an ordered pair list: an
/// existing key is replaced in place (order preserved); a new key is appended.
pub fn apply_overrides(pairs: &mut Vec<(String, String)>, extra: &Map<String, Value>) {
    for (k, v) in extra {
        let rendered = render_value(v);
        match pairs.iter_mut().find(|(key, _)| key == k) {
            Some(slot) => slot.1 = rendered,
            None => pairs.push((k.clone(), rendered)),
        }
    }
}

/// Render a JSON value as a **block-style YAML document** — the profile format
/// (`oh.yaml`). Emission stays hand-rolled for the same reason frontmatter does:
/// the shape is small and fully under our control (maps, sequences, scalars),
/// while *reading* arbitrary YAML is delegated to `yaml-rust2`.
pub fn to_yaml_document(v: &Value) -> String {
    let mut out = String::new();
    write_block(&mut out, v, 0, false);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Write `v` at `indent`. `inline` means the caller already emitted a prefix on
/// this line (a `key:` or a `- `), so a container starts on the next line.
fn write_block(out: &mut String, v: &Value, indent: usize, inline: bool) {
    let pad = "  ".repeat(indent);
    match v {
        Value::Object(m) if !m.is_empty() => {
            if inline {
                out.push('\n');
            }
            for (k, val) in m {
                write_entry(out, k, val, indent);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            if inline {
                out.push('\n');
            }
            for item in a {
                out.push_str(&pad);
                out.push_str("- ");
                if is_container(item) {
                    // Start the nested container on the following line, indented
                    // past the dash.
                    let mut nested = String::new();
                    write_block(&mut nested, item, indent + 1, false);
                    out.push_str(nested.trim_start());
                } else {
                    out.push_str(&render_value(item));
                    out.push('\n');
                }
            }
        }
        // Empty containers and scalars render inline.
        other => {
            if inline {
                out.push(' ');
            } else {
                out.push_str(&pad);
            }
            out.push_str(&render_value(other));
            out.push('\n');
        }
    }
}

/// One `key: value` line (with its nested block, if the value is a container).
fn write_entry(out: &mut String, key: &str, val: &Value, indent: usize) {
    out.push_str(&"  ".repeat(indent));
    out.push_str(&scalar(key));
    out.push(':');
    if is_container(val) {
        write_block(out, val, indent + 1, true);
    } else {
        out.push(' ');
        out.push_str(&render_value(val));
        out.push('\n');
    }
}

/// As [`to_yaml_document`], but the top-level mapping emits `first` keys in the
/// order given, then everything else. `serde_json`'s map is sorted, so without
/// this a profile would read `harnesses, name, sources`.
pub fn to_yaml_document_ordered(v: &Value, first: &[&str]) -> String {
    let Value::Object(m) = v else {
        return to_yaml_document(v);
    };
    let mut out = String::new();
    for key in first {
        if let Some(val) = m.get(*key) {
            write_entry(&mut out, key, val, 0);
        }
    }
    for (k, val) in m {
        if !first.contains(&k.as_str()) {
            write_entry(&mut out, k, val, 0);
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn is_container(v: &Value) -> bool {
    matches!(v, Value::Object(m) if !m.is_empty()) || matches!(v, Value::Array(a) if !a.is_empty())
}

// ---- reading (frontmatter → JSON) -----------------------------------------
//
// The frontmatter block is real YAML, so it is parsed by `yaml-rust2` and
// converted to `serde_json::Value`. Splitting the `---` fences off the Markdown
// body is plain string handling and stays here.

use yaml_rust2::{Yaml, YamlLoader};

/// Split a `---\nfrontmatter\n---\n\nbody` document into its parsed frontmatter
/// map and its body. A document without a leading `---` yields an empty map and
/// the whole text as the body.
pub fn parse_document(text: &str) -> Result<(Map<String, Value>, String), String> {
    let trimmed = text.trim_start();
    let Some(after_open) = trimmed.strip_prefix("---") else {
        return Ok((Map::new(), text.to_string()));
    };
    if !after_open.starts_with('\n') && !after_open.starts_with("\r\n") {
        // `---` not on its own line — treat as ordinary body.
        return Ok((Map::new(), text.to_string()));
    }
    let close = after_open
        .find("\n---")
        .ok_or("missing closing '---' for the frontmatter block")?;
    let fm_text = &after_open[..close];
    // The body starts after the rest of the closing `---` line.
    let after_marker = &after_open[close + 1..];
    let body = match after_marker.find('\n') {
        Some(nl) => after_marker[nl + 1..].to_string(),
        None => String::new(),
    };
    let fm = parse_frontmatter(fm_text)?;
    Ok((fm, body.trim_start_matches(['\n', '\r']).to_string()))
}

/// Parse a YAML frontmatter block into a JSON object.
pub fn parse_frontmatter(text: &str) -> Result<Map<String, Value>, String> {
    let docs =
        YamlLoader::load_from_str(text).map_err(|e| format!("invalid YAML frontmatter: {e}"))?;
    match docs.into_iter().next() {
        None | Some(Yaml::Null) | Some(Yaml::BadValue) => Ok(Map::new()),
        Some(doc) => match yaml_to_value(doc)? {
            Value::Object(m) => Ok(m),
            Value::Null => Ok(Map::new()),
            other => Err(format!("frontmatter must be a mapping, got {other}")),
        },
    }
}

/// Convert a `yaml-rust2` node into a `serde_json::Value`.
fn yaml_to_value(y: Yaml) -> Result<Value, String> {
    Ok(match y {
        Yaml::Null | Yaml::BadValue => Value::Null,
        Yaml::Boolean(b) => Value::Bool(b),
        Yaml::Integer(i) => Value::Number(i.into()),
        Yaml::Real(s) => s
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::String(s)),
        Yaml::String(s) => Value::String(s),
        Yaml::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(yaml_to_value(it)?);
            }
            Value::Array(out)
        }
        Yaml::Hash(h) => {
            // `yaml-rust2`'s Hash preserves insertion order, so frontmatter key
            // order is stable (deterministic re-emit).
            let mut out = Map::new();
            for (k, v) in h {
                out.insert(yaml_key(k)?, yaml_to_value(v)?);
            }
            Value::Object(out)
        }
        // Anchors/aliases aren't used in capability frontmatter.
        Yaml::Alias(_) => Value::Null,
    })
}

/// A mapping key must be a scalar (capability frontmatter keys always are).
fn yaml_key(k: Yaml) -> Result<String, String> {
    match k {
        Yaml::String(s) | Yaml::Real(s) => Ok(s),
        Yaml::Boolean(b) => Ok(b.to_string()),
        Yaml::Integer(i) => Ok(i.to_string()),
        other => Err(format!("unsupported non-scalar mapping key: {other:?}")),
    }
}
