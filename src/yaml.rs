//! Minimal, correct YAML frontmatter emission.
//!
//! The generative kinds emit YAML frontmatter (`---\nkey: value\n---`). The spike
//! interpolated values raw, which silently produces invalid YAML the moment a
//! value contains a `:` — and skill/agent descriptions routinely do, so the file
//! fails to load with no error (gap 2d). This module quotes exactly the values
//! that need it and renders arbitrary JSON values (from per-harness `overrides`)
//! as YAML nodes. It is deliberately not a general YAML library: frontmatter
//! values are scalars, flow lists, and flow maps, which is all this covers.

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
