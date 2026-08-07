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
// *is* the manifest. A focused YAML subset covering what real frontmatter uses:
// scalars (quoted/plain, bool/int/float/null inference), flow lists `[a, b]` and
// maps `{k: v}`, and **block** mappings and sequences nested to arbitrary depth
// (sequences of scalars, sequences of maps, maps of lists) — including PyYAML's
// default layout where a sequence sits at its key's indent. Not a general YAML
// parser: no anchors/aliases, tags, or multi-line (`|` / `>` / folded) scalars.

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
///
/// Indentation carries structure, so this is a recursive descent over the block:
/// nested mappings, sequences of scalars, sequences of maps, and arbitrary depth
/// all parse — including PyYAML's default layout where a sequence value sits at
/// the *same* indent as its key (`key:` then `- item`).
pub fn parse_frontmatter(text: &str) -> Result<Map<String, Value>, String> {
    let lines = logical_lines(text);
    let mut pos = 0;
    let value = parse_map(&lines, &mut pos, 0)?;
    if pos < lines.len() {
        return Err(format!(
            "frontmatter must be a mapping; unexpected line: {:?}",
            lines[pos].1
        ));
    }
    match value {
        Value::Object(m) => Ok(m),
        _ => Ok(Map::new()),
    }
}

/// Preprocess the block into `(indent, content)` logical lines: drop blank and
/// comment lines, and split each `- item` into a bare `-` marker at the dash's
/// column plus the item content at the column where it starts. That reduction
/// lets the same map/seq recursion handle sequences of scalars, sequences of
/// maps, and nesting without special cases.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for raw in text.lines() {
        let stripped = strip_comment(raw);
        if stripped.trim().is_empty() {
            continue;
        }
        let mut indent = stripped.len() - stripped.trim_start().len();
        let mut content = stripped.trim().to_string();
        loop {
            if content == "-" {
                out.push((indent, "-".to_string()));
                break;
            } else if let Some(rest) = content.strip_prefix("- ") {
                out.push((indent, "-".to_string()));
                let trimmed = rest.trim_start();
                indent += 2 + (rest.len() - trimmed.len());
                content = trimmed.to_string();
            } else {
                out.push((indent, content));
                break;
            }
        }
    }
    out
}

fn is_seq_marker(content: &str) -> bool {
    content == "-"
}

/// The index of the `:` that separates a mapping key from its value: the first
/// top-level colon (outside quotes/flow) followed by a space or end-of-line.
fn find_key_colon(content: &str) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut in_str = false;
    let mut depth = 0i32;
    for (i, c) in content.char_indices() {
        match c {
            '"' => in_str = !in_str,
            _ if in_str => {}
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            ':' if depth == 0 => {
                let next = bytes.get(i + 1);
                if next.is_none() || next == Some(&b' ') {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a block mapping whose keys sit at `indent`.
fn parse_map(lines: &[(usize, String)], pos: &mut usize, indent: usize) -> Result<Value, String> {
    let mut map = Map::new();
    while let Some((ind, content)) = lines.get(*pos) {
        if *ind < indent {
            break;
        }
        if *ind > indent {
            return Err(format!("unexpected indentation: {content:?}"));
        }
        if is_seq_marker(content) {
            break; // a sequence at map indent is not a mapping entry
        }
        let ci = find_key_colon(content)
            .ok_or_else(|| format!("mapping line without a key ':': {content:?}"))?;
        let key = unquote(content[..ci].trim());
        let rest = content[ci + 1..].trim();
        *pos += 1;
        let value = if !rest.is_empty() {
            parse_flow(rest)?
        } else {
            parse_value_after_key(lines, pos, indent)?
        };
        map.insert(key, value);
    }
    Ok(Value::Object(map))
}

/// The value that follows a `key:` with an empty inline part: a sequence (whose
/// items may sit at the key's indent — PyYAML style — or deeper), a nested
/// mapping (strictly deeper), or null.
fn parse_value_after_key(
    lines: &[(usize, String)],
    pos: &mut usize,
    key_indent: usize,
) -> Result<Value, String> {
    match lines.get(*pos) {
        Some((ind, content)) if is_seq_marker(content) && *ind >= key_indent => {
            parse_seq(lines, pos, *ind)
        }
        Some((ind, _)) if *ind > key_indent => parse_node(lines, pos, *ind),
        _ => Ok(Value::Null),
    }
}

/// Parse a block sequence whose `-` markers sit at `indent`. Each item's value is
/// the nested block indented deeper than the marker (a scalar, flow value, map,
/// or nested sequence), or null for a bare `-`.
fn parse_seq(lines: &[(usize, String)], pos: &mut usize, indent: usize) -> Result<Value, String> {
    let mut arr = Vec::new();
    while let Some((ind, content)) = lines.get(*pos) {
        if *ind < indent {
            break;
        }
        if *ind > indent {
            return Err(format!("unexpected indentation in sequence: {content:?}"));
        }
        if !is_seq_marker(content) {
            break;
        }
        *pos += 1;
        let value = match lines.get(*pos) {
            Some((ind2, _)) if *ind2 > indent => parse_node(lines, pos, *ind2)?,
            _ => Value::Null,
        };
        arr.push(value);
    }
    Ok(Value::Array(arr))
}

/// Parse the node beginning at the current line (indent already established by the
/// caller): a sequence, a lone flow value, a mapping, or a lone scalar.
fn parse_node(lines: &[(usize, String)], pos: &mut usize, indent: usize) -> Result<Value, String> {
    let Some((_, content)) = lines.get(*pos) else {
        return Ok(Value::Null);
    };
    if is_seq_marker(content) {
        parse_seq(lines, pos, indent)
    } else if content.starts_with('[') || content.starts_with('{') {
        let v = parse_flow(content)?;
        *pos += 1;
        Ok(v)
    } else if find_key_colon(content).is_some() {
        parse_map(lines, pos, indent)
    } else {
        let v = parse_scalar(content);
        *pos += 1;
        Ok(v)
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
