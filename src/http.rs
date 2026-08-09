//! A minimal HTTP/1.1 client — the shared transport under the streamable-HTTP
//! MCP bridge (#20) and remote capability sources (a registry index served over
//! HTTPS, see `docs/src/distribution.md`).
//!
//! Hand-rolled over `TcpStream` so the default build stays dependency-free:
//! `http://` needs nothing, `https://` uses the optional **`http-tls`** feature
//! (rustls + the Mozilla root set). Without that feature an `https` URL is
//! **honestly refused** — never silently downgraded and never a hang.
//!
//! The refusal is returned as a distinguishable [`Error::NoTls`] rather than a
//! flat string, because each caller owes the user *different* advice: the MCP
//! bridge can point at native config or a local proxy, while a registry source
//! can point at a `file://` URL or a git source. The transport states the fact;
//! the caller states the remedy.
//!
//! Requests use `Connection: close`, so a response body is delimited by EOF;
//! `Content-Length` and chunked transfer-encoding are both decoded.

use std::borrow::Cow;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Why an HTTP request failed.
#[derive(Debug)]
pub enum Error {
    /// The URL is `https://` but this build has no TLS (`http-tls` is off).
    /// Callers add their own actionable remedy.
    NoTls { host: String },
    /// Everything else: a malformed URL, DNS/connect/read/write failure, or an
    /// HTTP status >= 400.
    Message(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoTls { host } => write!(
                f,
                "this build has no TLS support: cannot reach https://{host} \
                 (rebuild with `--features http-tls`)"
            ),
            Error::Message(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

fn msg(s: impl Into<String>) -> Error {
    Error::Message(s.into())
}

/// A parsed HTTP response (headers lower-cased for case-insensitive lookup).
///
/// The body is kept as **bytes**: a capability archive is binary, and decoding it
/// as UTF-8 on the way in would corrupt it. Text callers ask for [`text`] /
/// [`into_text`] explicitly.
///
/// [`text`]: Response::text
/// [`into_text`]: Response::into_text
pub struct Response {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    /// A header's value by (case-insensitive) name.
    pub fn header(&self, name: &str) -> Option<&str> {
        let want = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == want)
            .map(|(_, v)| v.as_str())
    }

    /// The `Content-Type` header, or `""` when absent.
    pub fn content_type(&self) -> &str {
        self.header("content-type").unwrap_or("")
    }

    /// The raw response body.
    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    /// Consume the response, yielding its raw body.
    pub fn into_bytes(self) -> Vec<u8> {
        self.body
    }

    /// The body as UTF-8, replacing any invalid sequence.
    pub fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    /// Consume the response, yielding its body as UTF-8 (lossy).
    pub fn into_text(self) -> String {
        match String::from_utf8(self.body) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        }
    }
}

/// `GET url`, returning the parsed response.
pub fn get(
    url: &str,
    extra_headers: &[(String, String)],
    timeout: Duration,
) -> Result<Response, Error> {
    request("GET", url, None, extra_headers, timeout)
}

/// `POST url` with a JSON body, returning the parsed response.
pub fn post(
    url: &str,
    body: &str,
    extra_headers: &[(String, String)],
    timeout: Duration,
) -> Result<Response, Error> {
    request("POST", url, Some(body), extra_headers, timeout)
}

fn request(
    method: &str,
    url: &str,
    body: Option<&str>,
    extra_headers: &[(String, String)],
    timeout: Duration,
) -> Result<Response, Error> {
    let (scheme, host, port, path) = parse_url(url)?;
    // Everything interpolated into the raw request is checked first — see
    // `validate_header`. Refusing here, before a socket is opened, keeps a
    // malformed header from ever reaching the wire.
    for (k, v) in extra_headers {
        validate_header(k, v)?;
    }
    let raw_request = build_request(method, &host, &path, body, extra_headers);
    let raw = match scheme.as_str() {
        "http" => send_plain(&host, port, raw_request.as_bytes(), timeout)?,
        "https" => send_tls(&host, port, raw_request.as_bytes(), timeout)?,
        other => {
            return Err(msg(format!(
                "unsupported URL scheme '{other}://' in '{url}' (want http/https)"
            )))
        }
    };
    parse_response(&raw)
}

/// Reject a header that could break out of its own line.
///
/// Name and value are interpolated verbatim into the raw request, so a `\r\n` in
/// either splices in headers of someone else's choosing — and a second blank
/// line ends the request entirely, letting a whole extra one ride along the
/// connection (request smuggling). None of these strings are ours: they come
/// from a capability manifest's `tool.server.headers`, an `oh mcp --header`
/// argument, and the environment variable named by `bearer_token_env`. Writing
/// HTTP by hand means owning this check.
///
/// Names are RFC 9110 tokens; values may hold visible ASCII, space and tab, but
/// no other control characters.
fn validate_header(name: &str, value: &str) -> Result<(), Error> {
    fn is_token(c: char) -> bool {
        c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
    }
    if name.is_empty() || !name.chars().all(is_token) {
        return Err(msg(format!(
            "invalid HTTP header name {name:?}: expected a token (letters, digits, or !#$%&'*+-.^_`|~)"
        )));
    }
    if let Some(bad) = value
        .chars()
        .find(|c| (c.is_control() && *c != '\t') || !c.is_ascii())
    {
        return Err(msg(format!(
            "invalid character {bad:?} in the value of HTTP header `{name}`: a header value \
             cannot contain control characters or non-ASCII (a newline would let it forge \
             further headers)"
        )));
    }
    Ok(())
}

/// Build the raw HTTP/1.1 request bytes (shared by the plain + TLS transports).
fn build_request(
    method: &str,
    host: &str,
    path: &str,
    body: Option<&str>,
    extra_headers: &[(String, String)],
) -> String {
    let mut req = String::new();
    req.push_str(&format!("{method} {path} HTTP/1.1\r\n"));
    req.push_str(&format!("Host: {host}\r\n"));
    if let Some(b) = body {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("Connection: close\r\n");
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }
    req
}

/// Connect a `TcpStream` to `host:port` with the timeout applied to connect,
/// read, and write.
///
/// Tries **every** address the name resolves to, not just the first. A dual-stack
/// host whose AAAA record is unroutable from here is ordinary, and giving up on
/// the first candidate turns that into a hard failure with a confusing message.
fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, Error> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| msg(format!("resolve {host}:{port}: {e}")))?
        .collect();
    if addrs.is_empty() {
        return Err(msg(format!("no address for {host}:{port}")));
    }
    let mut last = None;
    for addr in &addrs {
        match TcpStream::connect_timeout(addr, timeout) {
            Ok(stream) => {
                stream.set_read_timeout(Some(timeout)).ok();
                stream.set_write_timeout(Some(timeout)).ok();
                return Ok(stream);
            }
            Err(e) => last = Some(format!("{addr}: {e}")),
        }
    }
    Err(msg(format!(
        "connect {host}:{port}: no address succeeded ({} tried, last error {})",
        addrs.len(),
        last.unwrap_or_default()
    )))
}

/// Plain-HTTP transport: write the request, read the response to EOF.
fn send_plain(host: &str, port: u16, request: &[u8], timeout: Duration) -> Result<Vec<u8>, Error> {
    let mut stream = connect(host, port, timeout)?;
    stream
        .write_all(request)
        .and_then(|_| stream.flush())
        .map_err(|e| msg(format!("write to {host}: {e}")))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| msg(format!("read from {host}: {e}")))?;
    Ok(raw)
}

/// TLS transport (`https`), gated on `http-tls` (rustls). Trusts the Mozilla
/// root set plus, if `OPEN_HARNESS_CA_FILE` points to a PEM bundle, that CA too
/// (for a private/corporate CA or a test server).
#[cfg(feature = "http-tls")]
fn send_tls(host: &str, port: u16, request: &[u8], timeout: Duration) -> Result<Vec<u8>, Error> {
    use std::sync::Arc;

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Ok(ca_path) = std::env::var("OPEN_HARNESS_CA_FILE") {
        let pem = std::fs::read(&ca_path).map_err(|e| msg(format!("read CA {ca_path}: {e}")))?;
        for cert in pem_certs(&pem) {
            let _ = roots.add(cert);
        }
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| msg(format!("invalid TLS server name '{host}'")))?;
    let mut conn = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| msg(format!("tls setup: {e}")))?;
    let mut sock = connect(host, port, timeout)?;
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);

    tls.write_all(request)
        .and_then(|_| tls.flush())
        .map_err(|e| msg(format!("tls write to {host}: {e}")))?;
    let mut raw = Vec::new();
    match tls.read_to_end(&mut raw) {
        Ok(_) => Ok(raw),
        // A server that closes the TLS session ungracefully after the response
        // still delivered a complete body; surface a hard failure only if empty.
        Err(_) if !raw.is_empty() => Ok(raw),
        Err(e) => Err(msg(format!("tls read from {host}: {e}"))),
    }
}

#[cfg(not(feature = "http-tls"))]
fn send_tls(host: &str, _port: u16, _request: &[u8], _timeout: Duration) -> Result<Vec<u8>, Error> {
    Err(Error::NoTls {
        host: host.to_string(),
    })
}

/// Minimal PEM certificate extractor (avoids a `rustls-pemfile` dependency):
/// pull each `-----BEGIN CERTIFICATE-----` block and base64-decode it.
#[cfg(feature = "http-tls")]
fn pem_certs(pem: &[u8]) -> Vec<rustls::pki_types::CertificateDer<'static>> {
    let text = String::from_utf8_lossy(pem);
    let mut out = Vec::new();
    let mut in_cert = false;
    let mut b64 = String::new();
    for line in text.lines() {
        if line.contains("BEGIN CERTIFICATE") {
            in_cert = true;
            b64.clear();
        } else if line.contains("END CERTIFICATE") {
            if let Some(der) = base64_decode(&b64) {
                out.push(rustls::pki_types::CertificateDer::from(der));
            }
            in_cert = false;
        } else if in_cert {
            b64.push_str(line.trim());
        }
    }
    out
}

/// Standard base64 decode (no external crate), for PEM certificate bodies.
#[cfg(feature = "http-tls")]
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Split a `scheme://host[:port][/path]` URL. Path defaults to `/`.
fn parse_url(url: &str) -> Result<(String, String, u16, String), Error> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| msg(format!("malformed URL '{url}' (want scheme://host…)")))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(msg(format!("malformed URL '{url}' (no host)")));
    }
    // Checked before the port split so the diagnosis is the real problem, not a
    // downstream symptom: the authority and path are interpolated into the
    // request line and the `Host` header, where a newline forges headers exactly
    // as one in a header value would.
    for (part, label) in [(authority, "host"), (path, "path")] {
        if let Some(bad) = part
            .chars()
            .find(|c| c.is_control() || *c == ' ' || !c.is_ascii())
        {
            return Err(msg(format!(
                "invalid character {bad:?} in the {label} of '{url}'"
            )));
        }
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| msg(format!("bad port in '{url}'")))?,
        ),
        None => (authority.to_string(), default_port),
    };
    Ok((scheme.to_string(), host, port, path.to_string()))
}

/// Parse raw response bytes into headers + a decoded body. A status >= 400 is an
/// error carrying a snippet of the body.
fn parse_response(raw: &[u8]) -> Result<Response, Error> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| msg("truncated HTTP response (no header terminator)"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body_bytes = &raw[split + 4..];

    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| msg(format!("bad HTTP status line '{status_line}'")))?;

    let mut headers = Vec::new();
    let mut chunked = false;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key == "transfer-encoding" && val.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
            headers.push((key, val));
        }
    }

    let body = if chunked {
        dechunk(body_bytes)
    } else {
        body_bytes.to_vec()
    };

    if status >= 400 {
        let snippet: String = String::from_utf8_lossy(&body).chars().take(200).collect();
        return Err(msg(format!("HTTP {status}: {snippet}")));
    }
    Ok(Response { headers, body })
}

/// Decode HTTP/1.1 chunked transfer-encoding.
fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        // A chunk-size line may carry a `;ext` suffix; take the hex prefix only.
        let line = String::from_utf8_lossy(&b[..nl]);
        let hex = line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(hex, 16) else {
            break;
        };
        if size == 0 {
            break; // last chunk
        }
        let (start, end) = (nl + 2, nl + 2 + size);
        if end > b.len() {
            out.extend_from_slice(&b[start.min(b.len())..]); // truncated — salvage
            break;
        }
        out.extend_from_slice(&b[start..end]);
        b = &b[(end + 2).min(b.len())..]; // skip data + trailing CRLF
    }
    out
}
