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

// ---- reading (frontmatter → JSON) -----------------------------------------
//
// The inverse of the emit side, for single-file capabilities whose frontmatter
// *is* the manifest. A deliberately small YAML subset — the shapes real
// frontmatter uses: scalars (quoted/plain, bool/int/float/null inference), flow
// lists `[a, b]` and maps `{k: v}` (nested), and top-level block lists (`- x`)
// and block maps. Not a general YAML parser (no anchors, multi-line scalars, or
// deep block indentation); deeply-nested config belongs in `capability.json`.

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

/// Parse a YAML frontmatter block (the text between the `---` fences) into a map.
pub fn parse_frontmatter(text: &str) -> Result<Map<String, Value>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut map = Map::new();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        let content = strip_comment(raw);
        if content.trim().is_empty() {
            i += 1;
            continue;
        }
        if raw.starts_with([' ', '\t']) {
            return Err(format!("unexpected indentation in frontmatter: {raw:?}"));
        }
        let (key, rest) = content
            .split_once(':')
            .ok_or_else(|| format!("frontmatter line without ':': {content:?}"))?;
        let key = key.trim().to_string();
        let rest = rest.trim();
        if rest.is_empty() {
            // A block list or block map on the following indented lines.
            let mut block: Vec<&str> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let l = lines[j];
                if l.trim().is_empty() {
                    j += 1;
                    continue;
                }
                if !l.starts_with([' ', '\t']) {
                    break;
                }
                block.push(l);
                j += 1;
            }
            map.insert(key, parse_block(&block)?);
            i = j;
        } else {
            map.insert(key, parse_flow(rest)?);
            i += 1;
        }
    }
    Ok(map)
}

/// A top-level block: `- item` lines → array; `k: v` lines → map.
fn parse_block(lines: &[&str]) -> Result<Value, String> {
    let mut cleaned: Vec<String> = Vec::new();
    for l in lines {
        let c = strip_comment(l);
        if !c.trim().is_empty() {
            cleaned.push(c.trim().to_string());
        }
    }
    if cleaned.is_empty() {
        return Ok(Value::Null);
    }
    if cleaned.iter().all(|l| l == "-" || l.starts_with("- ")) {
        let mut arr = Vec::new();
        for l in &cleaned {
            arr.push(parse_flow(l[1..].trim())?);
        }
        Ok(Value::Array(arr))
    } else {
        let mut obj = Map::new();
        for l in &cleaned {
            let (k, v) = l
                .split_once(':')
                .ok_or_else(|| format!("block-map line without ':': {l:?}"))?;
            obj.insert(k.trim().to_string(), parse_flow(v.trim())?);
        }
        Ok(Value::Object(obj))
    }
}

/// Parse a single flow value: a scalar, a `[…]` list, or a `{…}` map.
fn parse_flow(s: &str) -> Result<Value, String> {
    let s = s.trim();
    if s.starts_with('[') || s.starts_with('{') {
        let (v, rest) = parse_flow_collection(s)?;
        if !rest.trim().is_empty() {
            return Err(format!("trailing data after a flow value: {rest:?}"));
        }
        Ok(v)
    } else {
        Ok(parse_scalar(s))
    }
}

/// Parse a `[…]` or `{…}` collection (nesting + quotes respected), returning the
/// value and the unparsed remainder.
fn parse_flow_collection(s: &str) -> Result<(Value, &str), String> {
    if let Some(inner) = s.strip_prefix('[') {
        let (body, rest) = split_matching(inner, '[', ']')?;
        let mut arr = Vec::new();
        for item in split_top_commas(body) {
            if !item.trim().is_empty() {
                arr.push(parse_flow(item.trim())?);
            }
        }
        Ok((Value::Array(arr), rest))
    } else if let Some(inner) = s.strip_prefix('{') {
        let (body, rest) = split_matching(inner, '{', '}')?;
        let mut obj = Map::new();
        for item in split_top_commas(body) {
            if item.trim().is_empty() {
                continue;
            }
            let (k, v) = item
                .split_once(':')
                .ok_or_else(|| format!("flow-map entry without ':': {item:?}"))?;
            obj.insert(unquote(k.trim()), parse_flow(v.trim())?);
        }
        Ok((Value::Object(obj), rest))
    } else {
        Err(format!("expected a flow collection: {s:?}"))
    }
}

/// Return the substring up to the `close` that matches the already-consumed
/// `open`, plus the remainder after it. Depth- and quote-aware.
fn split_matching(inner: &str, open: char, close: char) -> Result<(&str, &str), String> {
    let mut depth = 1;
    let mut in_str = false;
    for (i, c) in inner.char_indices() {
        match c {
            '"' => in_str = !in_str,
            _ if in_str => {}
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&inner[..i], &inner[i + c.len_utf8()..]));
                }
            }
            _ => {}
        }
    }
    Err(format!("unterminated '{open}…{close}' flow collection"))
}

/// Split on top-level commas (ignoring commas inside nested `[]`/`{}` or quotes).
fn split_top_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut in_str = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_str = !in_str,
            _ if in_str => {}
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Parse a scalar: quoted string, bool, null, integer, float, else a plain string.
fn parse_scalar(s: &str) -> Value {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return Value::String(unquote(s));
    }
    match s {
        "true" | "True" => return Value::Bool(true),
        "false" | "False" => return Value::Bool(false),
        "null" | "~" | "" => return Value::Null,
        _ => {}
    }
    if let Ok(i) = s.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = s.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(s.to_string())
}

/// Unwrap a double-quoted scalar (basic escapes); pass a plain scalar through.
fn unquote(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => out.push(other),
                    None => {}
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        s.to_string()
    }
}

/// Strip a trailing `# comment` that sits outside a quoted string.
fn strip_comment(line: &str) -> String {
    let mut in_str = false;
    let mut prev_ws = true; // start-of-line counts as preceding whitespace
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str && prev_ws => return line[..i].to_string(),
            _ => {}
        }
        prev_ws = c.is_whitespace();
    }
    line.to_string()
}
