//! Dependency requirements: a **documented semver subset**, the relation types,
//! and the rules for matching one against a version.
//!
//! The resolution itself lives in [`crate::profile`]; this module is the
//! vocabulary it resolves in.
//!
//! ## The version subset
//!
//! `major[.minor[.patch]][-prerelease]`. Missing parts are zero, so `1.2` is
//! `1.2.0`. Prereleases are dot-separated identifiers ordered the semver way:
//! a release outranks its prereleases, numeric identifiers compare numerically
//! and rank below alphanumeric ones, and a shorter identifier list ranks lower
//! when everything before it is equal. Build metadata (`+…`) is accepted and
//! ignored, as semver requires.
//!
//! This is deliberately a *subset*, not a near-miss of the full grammar: a
//! subset you can state in a paragraph is honest, and the alternative — an
//! almost-complete implementation whose gaps surface as a mystery mismatch two
//! repos deep — is not.
//!
//! ## Operators
//!
//! | form | meaning |
//! |---|---|
//! | `*` or empty | any version |
//! | `1.2.3` or `=1.2.3` | exactly that version |
//! | `^1.2.3` | `>=1.2.3` and `<2.0.0` (leading-zero aware: `^0.2.3` → `<0.3.0`, `^0.0.3` → `<0.0.4`) |
//! | `~1.2.3` | `>=1.2.3` and `<1.3.0`; `~1` → `<2.0.0` |
//! | `>=` `>` `<` `<=` | the obvious bound |
//!
//! Comparators combine with `,` and **all** must hold: `>=1.2, <2.0`.
//!
//! One rule people expect and rarely get: a **prerelease only satisfies a
//! requirement in which *some* comparator names a prerelease at the same
//! `major.minor.patch`**. Without it `^1.0.0` would accept `2.0.0-beta` — a
//! prerelease of a version the range explicitly excludes. The opt-in is decided
//! per requirement, not per comparator, so `>=1.5.0-rc.1, <2.0.0` accepts
//! `1.5.0-rc.2` even though its upper bound names no prerelease. `*` is exempt:
//! it states no constraint at all, and should not quietly reject a capability
//! whose only version happens to be a prerelease.

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use std::cmp::Ordering;
use std::fmt;

// ---- version --------------------------------------------------------------

/// A concrete version in the subset above.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-separated prerelease identifiers (empty for a release).
    pub pre: Vec<String>,
}

impl Version {
    /// Parse a version, returning `None` for anything outside the subset.
    pub fn parse(text: &str) -> Option<Version> {
        let (v, _given) = parse_version_parts(text)?;
        Some(v)
    }

    /// Parse leniently, treating an unparseable string as `0.0.0`. Used for a
    /// capability's own `version`, which defaults to `0.0.0` and must never make
    /// a whole profile unresolvable.
    pub fn parse_lenient(text: &str) -> Version {
        Version::parse(text).unwrap_or_default()
    }

    fn core(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }

    fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core().cmp(&other.core()).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A release outranks any prerelease of the same core version.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => cmp_prerelease(&self.pre, &other.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Semver prerelease ordering: numeric identifiers compare numerically and rank
/// below alphanumeric ones; a shorter list ranks lower when all else is equal.
fn cmp_prerelease(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(nx), Ok(ny)) => nx.cmp(&ny),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

/// Parse `major[.minor[.patch]][-pre][+build]`, also reporting how many numeric
/// parts were written — `~1` and `~1.0.0` mean different things.
fn parse_version_parts(text: &str) -> Option<(Version, u8)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Build metadata takes no part in precedence (semver §10).
    let text = text.split('+').next().unwrap_or(text);
    // `None` (no hyphen) and `Some("")` (`1.2.3-`) are different: the first is a
    // release, the second is malformed.
    let (core, pre) = match text.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (text, None),
    };

    let mut nums = [0u64; 3];
    let mut given = 0u8;
    for (i, part) in core.split('.').enumerate() {
        if i >= 3 {
            return None; // more than three numeric components is out of subset
        }
        nums[i] = part.parse::<u64>().ok()?;
        given += 1;
    }
    if given == 0 {
        return None;
    }
    let pre: Vec<String> = match pre {
        None => Vec::new(),
        Some(text) => {
            // An empty identifier (`1.0.0-`, `1.0.0-a..b`) is malformed, not an
            // empty prerelease.
            let ids: Vec<String> = text.split('.').map(|s| s.to_string()).collect();
            if ids.iter().any(|s| s.is_empty()) {
                return None;
            }
            ids
        }
    };
    Some((
        Version {
            major: nums[0],
            minor: nums[1],
            patch: nums[2],
            pre,
        },
        given,
    ))
}

// ---- comparators ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Any,
    Exact,
    Caret,
    Tilde,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Comparator {
    op: Op,
    version: Version,
    /// How many numeric parts the author wrote (1–3). `~1` bounds differently
    /// from `~1.0.0`, so the distinction has to survive parsing.
    given: u8,
}

impl Comparator {
    fn parse(text: &str) -> Option<Comparator> {
        let text = text.trim();
        if text.is_empty() || text == "*" {
            return Some(Comparator {
                op: Op::Any,
                version: Version::default(),
                given: 0,
            });
        }
        // Longest operators first, so `>=` is not read as `>`.
        let (op, rest) = for_each_prefix(text);
        let (version, given) = parse_version_parts(rest)?;
        Some(Comparator { op, version, given })
    }

    /// The exclusive upper bound for `^` and `~`, if the operator has one.
    fn upper_bound(&self) -> Option<Version> {
        let v = &self.version;
        let bump = |major, minor, patch| {
            Some(Version {
                major,
                minor,
                patch,
                pre: Vec::new(),
            })
        };
        match self.op {
            // Caret allows changes that do not modify the left-most non-zero
            // component — so 0.x and 0.0.x are progressively stricter.
            Op::Caret => {
                if v.major > 0 || self.given == 1 {
                    bump(v.major + 1, 0, 0)
                } else if v.minor > 0 || self.given == 2 {
                    bump(v.major, v.minor + 1, 0)
                } else {
                    bump(v.major, v.minor, v.patch + 1)
                }
            }
            // Tilde allows patch-level changes when a minor is written, and
            // minor-level changes when only a major is.
            Op::Tilde => {
                if self.given == 1 {
                    bump(v.major + 1, 0, 0)
                } else {
                    bump(v.major, v.minor + 1, 0)
                }
            }
            _ => None,
        }
    }

    /// Does this single comparator admit `v`? The prerelease opt-in is decided
    /// by the whole [`Requirement`], not here — `>=1.5.0-rc.1, <2.0.0` must
    /// accept `1.5.0-rc.2`, and its `<2.0.0` half names no prerelease at all.
    fn matches(&self, v: &Version) -> bool {
        if self.op == Op::Any {
            return true;
        }
        let lower_ok = match self.op {
            Op::Exact => return v == &self.version,
            Op::Gt => v > &self.version,
            Op::Gte | Op::Caret | Op::Tilde => v >= &self.version,
            Op::Lt => v < &self.version,
            Op::Lte => v <= &self.version,
            Op::Any => true,
        };
        match self.upper_bound() {
            Some(upper) => lower_ok && v < &upper,
            None => lower_ok,
        }
    }
}

fn for_each_prefix(text: &str) -> (Op, &str) {
    for (prefix, op) in [
        (">=", Op::Gte),
        ("<=", Op::Lte),
        ("^", Op::Caret),
        ("~", Op::Tilde),
        (">", Op::Gt),
        ("<", Op::Lt),
        ("=", Op::Exact),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return (op, rest);
        }
    }
    (Op::Exact, text)
}

// ---- requirement ----------------------------------------------------------

/// A conjunction of comparators: every one must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    comparators: Vec<Comparator>,
    /// The text as written, for error messages and round-tripping.
    raw: String,
}

impl Default for Requirement {
    fn default() -> Self {
        Requirement::any()
    }
}

impl Requirement {
    /// The requirement that accepts anything (`*`) — what a bare dependency
    /// name means.
    pub fn any() -> Requirement {
        Requirement {
            comparators: vec![Comparator {
                op: Op::Any,
                version: Version::default(),
                given: 0,
            }],
            raw: "*".to_string(),
        }
    }

    /// Parse `">=1.2, <2.0"`. An unparseable comparator is an error rather than
    /// a silently-ignored one — a requirement that quietly means "any" is how
    /// you ship the wrong version.
    pub fn parse(text: &str) -> Result<Requirement, String> {
        let raw = text.trim().to_string();
        if raw.is_empty() {
            return Ok(Requirement::any());
        }
        let mut comparators = Vec::new();
        for part in raw.split(',') {
            let c = Comparator::parse(part)
                .ok_or_else(|| format!("unparseable version requirement '{}'", part.trim()))?;
            comparators.push(c);
        }
        Ok(Requirement { comparators, raw })
    }

    /// Does `v` satisfy every comparator?
    ///
    /// A prerelease additionally has to be *opted into*: some comparator must
    /// name a prerelease at the same `major.minor.patch`. Without that,
    /// `^1.0.0` would accept `2.0.0-beta` — a prerelease of a version the range
    /// excludes. `*` is exempt, because it states no constraint at all and
    /// should not quietly reject a capability whose only version is a
    /// prerelease.
    pub fn matches(&self, v: &Version) -> bool {
        if self.is_any() {
            return true;
        }
        if v.is_prerelease() && !self.admits_prerelease(v) {
            return false;
        }
        self.comparators.iter().all(|c| c.matches(v))
    }

    fn admits_prerelease(&self, v: &Version) -> bool {
        self.comparators
            .iter()
            .any(|c| c.version.is_prerelease() && c.version.core() == v.core())
    }

    pub fn is_any(&self) -> bool {
        self.comparators.iter().all(|c| c.op == Op::Any)
    }

    /// Conjunction: a version satisfies the result exactly when it satisfies
    /// both. This is how several dependents' requirements on one capability are
    /// combined.
    pub fn intersect(&self, other: &Requirement) -> Requirement {
        if self.is_any() {
            return other.clone();
        }
        if other.is_any() {
            return self.clone();
        }
        let mut comparators = self.comparators.clone();
        comparators.extend(other.comparators.clone());
        Requirement {
            comparators,
            raw: format!("{}, {}", self.raw, other.raw),
        }
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Serialize for Requirement {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for Requirement {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Requirement, D::Error> {
        let text = String::deserialize(d)?;
        Requirement::parse(&text).map_err(de::Error::custom)
    }
}

// ---- relations ------------------------------------------------------------

/// How one capability relates to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Relation {
    /// Must be present and satisfy the requirement.
    #[default]
    Requires,
    /// Better together; absence is a note, not a problem.
    Suggests,
    /// Must **not** be composed alongside. Useful because merging is
    /// deny-wins: two policies that contradict each other silently produce the
    /// strictest union, which may be neither author's intent.
    Conflicts,
    /// Supersedes the named capability — a fork or a vendored copy standing in
    /// for the original, so a dependency on the original is satisfied by this.
    Replaces,
}

impl Relation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Relation::Requires => "requires",
            Relation::Suggests => "suggests",
            Relation::Conflicts => "conflicts",
            Relation::Replaces => "replaces",
        }
    }
}

/// One declared edge: a target name, a version requirement, and a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// The target's qualified name, or a bare id resolved against the
    /// declaring capability's own namespace first.
    pub name: String,
    pub requirement: Requirement,
    pub relation: Relation,
    /// Where this dependency may be fetched from, for opt-in transitive
    /// acquisition. Ignored unless the profile enables it.
    pub source: Option<serde_json::Value>,
}

impl Dependency {
    pub fn new(name: impl Into<String>) -> Dependency {
        Dependency {
            name: name.into(),
            requirement: Requirement::any(),
            relation: Relation::Requires,
            source: None,
        }
    }
}

/// A capability's declared dependencies.
///
/// Three spellings deserialize, so the simple case stays simple and the old
/// list form keeps working:
///
/// ```yaml
/// dependencies: [shared-patterns]                 # bare names, any version
/// dependencies:
///   acme/shared-patterns: "^1.2"                  # name → requirement
///   acme/legacy:
///     version: ">=2, <4"                          # the long form
///     relation: suggests
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dependencies(pub Vec<Dependency>);

impl Dependencies {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Dependency> {
        self.0.iter()
    }

    /// Just the edges with the given relation.
    pub fn with_relation(&self, relation: Relation) -> impl Iterator<Item = &Dependency> {
        self.0.iter().filter(move |d| d.relation == relation)
    }
}

impl<'a> IntoIterator for &'a Dependencies {
    type Item = &'a Dependency;
    type IntoIter = std::slice::Iter<'a, Dependency>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl Serialize for Dependencies {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Always the map form: it round-trips every field the long form
        // carries. An order-preserving map, so the author's ordering survives
        // (`serde_json/preserve_order`) rather than being alphabetized.
        let mut map = serde_json::Map::new();
        for d in &self.0 {
            let mut entry = serde_json::Map::new();
            entry.insert(
                "version".into(),
                serde_json::Value::String(d.requirement.to_string()),
            );
            if d.relation != Relation::Requires {
                entry.insert(
                    "relation".into(),
                    serde_json::Value::String(d.relation.as_str().to_string()),
                );
            }
            if let Some(src) = &d.source {
                entry.insert("source".into(), src.clone());
            }
            map.insert(d.name.clone(), serde_json::Value::Object(entry));
        }
        map.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Dependencies {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Dependencies, D::Error> {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = Dependencies;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a list of dependency names or a map of name → requirement")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Dependencies, A::Error> {
                let mut out = Vec::new();
                while let Some(name) = seq.next_element::<String>()? {
                    out.push(Dependency::new(name));
                }
                Ok(Dependencies(out))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Dependencies, A::Error> {
                let mut out = Vec::new();
                while let Some((name, value)) = map.next_entry::<String, serde_json::Value>()? {
                    out.push(dependency_from_value(name, value).map_err(de::Error::custom)?);
                }
                Ok(Dependencies(out))
            }
        }

        d.deserialize_any(V)
    }
}

/// One map entry: either a bare requirement string or the long form.
fn dependency_from_value(name: String, value: serde_json::Value) -> Result<Dependency, String> {
    match value {
        serde_json::Value::String(req) => {
            let requirement = Requirement::parse(&req).map_err(|e| format!("{name}: {e}"))?;
            Ok(Dependency {
                name,
                requirement,
                relation: Relation::Requires,
                source: None,
            })
        }
        serde_json::Value::Null => Ok(Dependency::new(name)),
        serde_json::Value::Object(obj) => {
            let requirement = match obj.get("version").and_then(|v| v.as_str()) {
                Some(req) => Requirement::parse(req).map_err(|e| format!("{name}: {e}"))?,
                None => Requirement::any(),
            };
            let relation = match obj.get("relation").and_then(|v| v.as_str()) {
                None => Relation::Requires,
                Some("requires") => Relation::Requires,
                Some("suggests") => Relation::Suggests,
                Some("conflicts") => Relation::Conflicts,
                Some("replaces") => Relation::Replaces,
                Some(other) => {
                    return Err(format!(
                        "{name}: unknown relation '{other}' \
                         (requires | suggests | conflicts | replaces)"
                    ))
                }
            };
            Ok(Dependency {
                name,
                requirement,
                relation,
                source: obj.get("source").cloned(),
            })
        }
        other => Err(format!(
            "{name}: a dependency must be a requirement string or a mapping, got {other}"
        )),
    }
}
