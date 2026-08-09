//! A minimal tar reader — unpacking a fetched capability archive (#21, see
//! `docs/src/distribution.md`).
//!
//! tar is one of the *simple* wire formats this crate hand-rolls (alongside
//! JSON-RPC, HTTP, and hex): fixed-offset 512-byte headers with octal numeric
//! fields. Compression is **not** simple, so it is not hand-rolled — a
//! `.tar.gz` / `.tar.zst` / `.tar.xz` / `.tar.bz2` is recognised by its magic
//! bytes and **refused honestly**, never mis-read as tar. Capability archives are
//! small text trees, and a CDN can still compress them on the wire
//! (`Content-Encoding`) without changing the bytes the digest covers.
//!
//! Extraction is the security-critical half: an archive is attacker-controlled
//! input from a registry. Even though the caller verifies the content digest
//! *before* unpacking (so a tampered download never reaches here), the publisher
//! themself is untrusted, so the extractor refuses everything that could write
//! outside the destination directory:
//!
//!   * absolute paths, drive-letter paths, and any `..` component;
//!   * symlinks and hardlinks — a link is a write-through escape, and nothing in
//!     a capability tree needs one;
//!   * device nodes, FIFOs, and other non-regular entries;
//!
//! and bounds both entry count and unpacked size, so a small download cannot
//! expand into a disk-filling bomb.

use std::path::{Path, PathBuf};

/// tar's fixed block size.
const BLOCK: usize = 512;

/// Refuse an archive with more entries than this.
const MAX_ENTRIES: usize = 20_000;

/// Refuse an archive that unpacks to more than this.
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// One accepted archive member.
struct Entry {
    /// A validated, `/`-separated path relative to the destination root.
    path: String,
    data: Vec<u8>,
    is_dir: bool,
}

/// Unpack an uncompressed tar archive into `dest`, returning the number of files
/// written. `dest` is created if absent.
pub fn unpack_tar(bytes: &[u8], dest: &Path) -> Result<usize, String> {
    if let Some(kind) = compression_kind(bytes) {
        return Err(format!(
            "archive is {kind}-compressed, but open-harness reads an uncompressed tar \
             (repack with `tar -cf capability.tar …`)"
        ));
    }
    let entries = read_entries(bytes)?;
    std::fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;

    let mut files = 0usize;
    for entry in entries {
        let target = join_relative(dest, &entry.path);
        if entry.is_dir {
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("mkdir {}: {e}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&target, &entry.data)
            .map_err(|e| format!("write {}: {e}", target.display()))?;
        files += 1;
    }
    Ok(files)
}

/// Join a validated relative path component-by-component, so a `/`-separated
/// archive path lands correctly on every platform's separator.
fn join_relative(base: &Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for part in rel.split('/') {
        out.push(part);
    }
    out
}

/// The compression format `bytes` starts with, if it is a compressed stream.
fn compression_kind(bytes: &[u8]) -> Option<&'static str> {
    const MAGIC: &[(&[u8], &str)] = &[
        (&[0x1f, 0x8b], "gzip"),
        (&[0x28, 0xb5, 0x2f, 0xfd], "zstd"),
        (&[0xfd, b'7', b'z', b'X', b'Z', 0x00], "xz"),
        (b"BZh", "bzip2"),
    ];
    MAGIC
        .iter()
        .find(|(m, _)| bytes.starts_with(m))
        .map(|(_, name)| *name)
}

/// Walk the archive's header blocks, validating and collecting its members.
fn read_entries(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let mut out: Vec<Entry> = Vec::new();
    let mut pos = 0usize;
    let mut total: u64 = 0;
    let mut zero_blocks = 0u8;
    // A GNU `L` record carries the next entry's (over-long) name.
    let mut long_name: Option<String> = None;

    while pos + BLOCK <= bytes.len() {
        let header = &bytes[pos..pos + BLOCK];
        pos += BLOCK;

        if header.iter().all(|&b| b == 0) {
            // Two consecutive zero blocks terminate the archive.
            zero_blocks += 1;
            if zero_blocks == 2 {
                break;
            }
            continue;
        }
        zero_blocks = 0;
        verify_checksum(header)?;

        let size = octal(header, 124, 12)?;
        let data_start = pos;
        let data_end = data_start
            .checked_add(size as usize)
            .ok_or("tar: entry size overflows")?;
        if data_end > bytes.len() {
            return Err("truncated tar archive (entry data runs past the end)".to_string());
        }
        let data = &bytes[data_start..data_end];
        // Data is padded out to a whole number of blocks.
        pos = data_start + (size as usize).div_ceil(BLOCK) * BLOCK;

        match header[156] {
            // GNU long name / long link name: the name is this entry's payload.
            b'L' => {
                long_name = Some(cstr(data));
                continue;
            }
            b'K' => continue,
            // PAX extended headers: skipped — the following ustar header still
            // carries a usable name.
            b'x' | b'g' => {
                long_name = None;
                continue;
            }
            _ => {}
        }

        let raw_name = long_name.take().unwrap_or_else(|| ustar_name(header));
        let typeflag = header[156];
        let is_dir = typeflag == b'5' || (raw_name.ends_with('/') && matches!(typeflag, b'0' | 0));

        if !is_dir {
            match typeflag {
                b'0' | 0 => {}
                b'1' | b'2' => {
                    return Err(format!(
                        "tar entry '{raw_name}' is a link — refused (a link in a capability \
                         archive can redirect a later write outside the destination)"
                    ))
                }
                other => {
                    return Err(format!(
                        "tar entry '{raw_name}' has unsupported type '{}' — only regular files \
                         and directories are unpacked",
                        printable(other)
                    ))
                }
            }
        }

        let path = safe_relative_path(&raw_name)?;

        total = total
            .checked_add(size)
            .filter(|t| *t <= MAX_TOTAL_BYTES)
            .ok_or_else(|| {
                format!("tar archive unpacks to more than {MAX_TOTAL_BYTES} bytes — refused")
            })?;
        if out.len() >= MAX_ENTRIES {
            return Err(format!(
                "tar archive has more than {MAX_ENTRIES} entries — refused"
            ));
        }

        out.push(Entry {
            path,
            data: if is_dir { Vec::new() } else { data.to_vec() },
            is_dir,
        });
    }
    Ok(out)
}

/// A tar header's checksum must match the sum of its bytes with the checksum
/// field itself read as spaces. Both the unsigned and the (historical) signed
/// interpretation are accepted.
fn verify_checksum(h: &[u8]) -> Result<(), String> {
    let want = octal(h, 148, 8)?;
    let mut unsigned: u64 = 0;
    let mut signed: i64 = 0;
    for (i, &b) in h.iter().enumerate() {
        let v = if (148..156).contains(&i) { b' ' } else { b };
        unsigned += v as u64;
        signed += v as i8 as i64;
    }
    if unsigned == want || signed == want as i64 {
        Ok(())
    } else {
        Err("tar: header checksum mismatch (corrupt archive)".to_string())
    }
}

/// Read an octal numeric header field.
fn octal(h: &[u8], off: usize, len: usize) -> Result<u64, String> {
    let field = &h[off..off + len];
    // GNU encodes very large values in base-256 (high bit set). Capability
    // archives never need it, so it is refused rather than mis-read.
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return Err("tar: base-256 numeric fields are not supported".to_string());
    }
    let cleaned: Vec<u8> = field.iter().copied().filter(|&b| b != 0).collect();
    let text = String::from_utf8_lossy(&cleaned);
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|_| format!("tar: bad octal field '{text}'"))
}

/// The ustar name: `prefix` + `/` + `name` when a prefix is present.
fn ustar_name(h: &[u8]) -> String {
    let name = cstr(&h[0..100]);
    let prefix = cstr(&h[345..500]);
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

/// A NUL-terminated (or NUL-padded) header string.
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn printable(b: u8) -> String {
    if b.is_ascii_graphic() {
        (b as char).to_string()
    } else {
        format!("0x{b:02x}")
    }
}

/// Validate an archive path and return it normalized, or explain the refusal.
///
/// This is the containment boundary: everything that could escape `dest` is
/// rejected here rather than filtered silently.
fn safe_relative_path(name: &str) -> Result<String, String> {
    if name.is_empty() {
        return Err("tar entry with an empty name".to_string());
    }
    if name.contains('\0') {
        return Err("tar entry name contains a NUL byte".to_string());
    }
    // tar paths use `/`; a `\` is treated as a separator too, so a Windows-style
    // name cannot smuggle a `..` component past the check on any platform.
    let normalized = name.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(format!("tar entry '{name}' has an absolute path — refused"));
    }
    let b = normalized.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Err(format!(
            "tar entry '{name}' has a drive-letter path — refused"
        ));
    }
    let mut parts: Vec<&str> = Vec::new();
    for comp in normalized.split('/') {
        match comp {
            "" | "." => continue,
            ".." => {
                return Err(format!(
                    "tar entry '{name}' escapes the archive root with '..' — refused"
                ))
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Err(format!("tar entry '{name}' names no path"));
    }
    Ok(parts.join("/"))
}

// ---- writing --------------------------------------------------------------
//
// Publishing is the inverse of the above, with one extra requirement: the output
// must be **deterministic**. A capability's archive is named by its own sha256,
// so two publishers packing identical bytes have to produce an identical
// archive — otherwise the content address changes for reasons that have nothing
// to do with the content. Every field that would otherwise vary (mtime, uid/gid,
// owner names, directory iteration order) is therefore fixed.

/// Ordinary file mode written for every member — see the determinism note above.
const PACK_MODE: u32 = 0o644;

/// Build a deterministic uncompressed tar from `(path, contents)` members.
///
/// Paths are validated with the same rules the reader enforces, and sorted, so
/// the archive is byte-identical for the same input regardless of how the caller
/// discovered its files. Directory entries are not emitted: the reader (and
/// `tar -x`) creates parent directories as needed.
pub fn write_tar(members: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut sorted: Vec<(String, &Vec<u8>)> = Vec::with_capacity(members.len());
    for (path, data) in members {
        sorted.push((safe_relative_path(path)?, data));
    }
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted.dedup_by(|a, b| a.0 == b.0);

    let mut out = Vec::new();
    for (path, data) in sorted {
        out.extend_from_slice(&header(&path, data.len())?);
        out.extend_from_slice(data);
        out.resize(out.len() + (BLOCK - data.len() % BLOCK) % BLOCK, 0);
    }
    // Two zero blocks terminate the archive.
    out.resize(out.len() + 2 * BLOCK, 0);
    Ok(out)
}

/// A ustar header for one regular file.
fn header(path: &str, size: usize) -> Result<[u8; BLOCK], String> {
    let (name, prefix) = split_name(path)?;
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name.as_bytes());
    write_field(&mut h, 100, &format!("{PACK_MODE:07o}\0"));
    write_field(&mut h, 108, "0000000\0"); // uid
    write_field(&mut h, 116, "0000000\0"); // gid
    write_field(&mut h, 124, &format!("{size:011o}\0"));
    write_field(&mut h, 136, "00000000000\0"); // mtime — fixed, for reproducibility
    h[156] = b'0'; // regular file
    write_field(&mut h, 257, "ustar\0");
    write_field(&mut h, 263, "00");
    // uname/gname stay empty: an owner name would leak the publisher's account
    // into the content address.
    write_field(&mut h, 345, prefix);

    // The checksum is computed with its own field read as spaces.
    for b in h.iter_mut().skip(148).take(8) {
        *b = b' ';
    }
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    write_field(&mut h, 148, &format!("{sum:06o}\0 "));
    Ok(h)
}

fn write_field(h: &mut [u8; BLOCK], off: usize, s: &str) {
    h[off..off + s.len()].copy_from_slice(s.as_bytes());
}

/// Split a path into ustar's `name` (≤100) and `prefix` (≤155) fields, which
/// together carry up to 255 bytes. Anything longer is refused rather than
/// silently truncated.
fn split_name(path: &str) -> Result<(&str, &str), String> {
    if path.len() <= 100 {
        return Ok((path, ""));
    }
    // Find the split point that leaves a ≤100-byte name and a ≤155-byte prefix.
    for (i, _) in path.match_indices('/') {
        let (prefix, rest) = (&path[..i], &path[i + 1..]);
        if rest.len() <= 100 && prefix.len() <= 155 {
            return Ok((rest, prefix));
        }
    }
    Err(format!(
        "path '{path}' is too long for a tar header (max 100 bytes after the last \
         directory split, 155 before it)"
    ))
}

/// Read `dir` into deterministic tar members, each prefixed with `root_name` so
/// the archive unpacks to `root_name/…` — the layout `discover` expects.
///
/// The managed cache tree is skipped; a `capability.sig` is **kept**, so a
/// published archive can carry its own signature.
pub fn read_dir_members(dir: &Path, root_name: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    collect_files(dir, &PathBuf::from(root_name), &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn collect_files(dir: &Path, rel: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("scan {}: {e}", dir.display()))?;
    // Sort locally: directory iteration order is filesystem-dependent, and the
    // archive's digest must not be.
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let child = rel.join(&name);
        if path.is_dir() {
            if name == ".open-harness" {
                continue;
            }
            collect_files(&path, &child, out)?;
        } else if path.is_file() {
            let data = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
            out.push((child.to_string_lossy().replace('\\', "/"), data));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_archives_round_trip_and_are_deterministic() {
        let members = vec![
            ("pack/b.md".to_string(), b"second".to_vec()),
            ("pack/a/capability.json".to_string(), b"{}".to_vec()),
        ];
        let once = write_tar(&members).unwrap();
        // Reversing the input must not change the archive: the content address
        // depends on content, not on discovery order.
        let mut reversed = members.clone();
        reversed.reverse();
        assert_eq!(
            once,
            write_tar(&reversed).unwrap(),
            "packing is deterministic"
        );

        let dest = std::env::temp_dir().join(format!("oh-archive-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        assert_eq!(unpack_tar(&once, &dest).unwrap(), 2);
        assert_eq!(
            std::fs::read_to_string(dest.join("pack/a/capability.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("pack/b.md")).unwrap(),
            "second"
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn long_paths_use_the_ustar_prefix_and_over_long_ones_are_refused() {
        let deep = format!("{}/{}", "d".repeat(120), "f".repeat(40));
        let archive = write_tar(&[(deep.clone(), b"x".to_vec())]).unwrap();
        let dest = std::env::temp_dir().join(format!("oh-archive-long-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        unpack_tar(&archive, &dest).unwrap();
        assert!(
            dest.join(&deep).is_file(),
            "a prefix-split path round-trips"
        );
        let _ = std::fs::remove_dir_all(&dest);

        let unsplittable = "x".repeat(120);
        assert!(
            write_tar(&[(unsplittable, b"x".to_vec())]).is_err(),
            "a name too long to split is refused, not truncated"
        );
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        for bad in [
            "../evil",
            "a/../../evil",
            "/etc/passwd",
            "C:/windows/system32",
            r"..\evil",
        ] {
            assert!(
                safe_relative_path(bad).is_err(),
                "'{bad}' must be refused, not normalized away"
            );
        }
    }

    #[test]
    fn accepts_and_normalizes_ordinary_paths() {
        assert_eq!(safe_relative_path("a/b.md").unwrap(), "a/b.md");
        assert_eq!(safe_relative_path("./a//b.md").unwrap(), "a/b.md");
        assert_eq!(safe_relative_path("pack/").unwrap(), "pack");
    }

    #[test]
    fn detects_compressed_streams() {
        assert_eq!(compression_kind(&[0x1f, 0x8b, 0x08]), Some("gzip"));
        assert_eq!(compression_kind(&[0x28, 0xb5, 0x2f, 0xfd]), Some("zstd"));
        assert_eq!(compression_kind(b"BZh9"), Some("bzip2"));
        assert_eq!(compression_kind(b"capability.json"), None);
    }
}
