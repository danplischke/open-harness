//! The dependency vocabulary: version parsing/ordering, the operator subset,
//! and how the three `dependencies` spellings deserialize.
//!
//! Resolution against a real profile lives in `tests/profile.rs`; this file
//! pins the semantics the resolver relies on.

use open_harness::deps::{Dependencies, Relation, Requirement, Version};
use serde_json::json;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap_or_else(|| panic!("{s} should parse"))
}

fn req(s: &str) -> Requirement {
    Requirement::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

/// `matches` for every listed version, `!matches` for every excluded one.
fn check(requirement: &str, accepts: &[&str], rejects: &[&str]) {
    let r = req(requirement);
    for a in accepts {
        assert!(r.matches(&v(a)), "{requirement} should accept {a}");
    }
    for x in rejects {
        assert!(!r.matches(&v(x)), "{requirement} should reject {x}");
    }
}

// ---- versions -------------------------------------------------------------

#[test]
fn missing_components_default_to_zero() {
    assert_eq!(v("1"), v("1.0.0"));
    assert_eq!(v("1.2"), v("1.2.0"));
    assert_eq!(v("1.2.3").to_string(), "1.2.3");
}

#[test]
fn build_metadata_is_accepted_and_ignored() {
    // Semver §10: build metadata takes no part in precedence.
    assert_eq!(v("1.2.3+build.7"), v("1.2.3"));
}

#[test]
fn versions_order_numerically_not_lexically() {
    assert!(
        v("0.10.0") > v("0.9.0"),
        "10 > 9 even though \"10\" < \"9\""
    );
    assert!(v("2.0.0") > v("1.99.99"));
}

#[test]
fn a_release_outranks_its_prereleases() {
    assert!(v("1.0.0") > v("1.0.0-rc.1"));
    assert!(v("1.0.0-rc.1") > v("1.0.0-beta.11"));
    // Numeric identifiers compare numerically, so 11 > 2 (lexically it is not).
    assert!(v("1.0.0-beta.11") > v("1.0.0-beta.2"));
    // Numeric identifiers rank below alphanumeric ones.
    assert!(v("1.0.0-alpha.beta") > v("1.0.0-alpha.1"));
    // A shorter identifier list ranks lower when everything before is equal.
    assert!(v("1.0.0-alpha.1") > v("1.0.0-alpha"));
}

#[test]
fn out_of_subset_versions_are_refused() {
    for bad in ["", "x", "1.2.3.4", "1.-2.3", "1.2.3-", "v1.2.3"] {
        assert!(Version::parse(bad).is_none(), "{bad} is outside the subset");
    }
}

#[test]
fn a_capabilitys_own_version_parses_leniently() {
    // A malformed `version:` must not make a whole profile unresolvable.
    assert_eq!(Version::parse_lenient("not-a-version"), v("0.0.0"));
    assert_eq!(Version::parse_lenient("2.1"), v("2.1.0"));
}

// ---- operators ------------------------------------------------------------

#[test]
fn star_and_empty_accept_anything() {
    check("*", &["0.0.0", "1.2.3", "99.0.0"], &[]);
    check("", &["0.0.0", "1.2.3"], &[]);
    assert!(req("*").is_any());
}

#[test]
fn a_bare_version_is_exact() {
    check("1.2.3", &["1.2.3"], &["1.2.4", "1.2.2", "2.0.0"]);
    check("=1.2.3", &["1.2.3"], &["1.2.4"]);
}

#[test]
fn caret_is_leading_zero_aware() {
    check("^1.2.3", &["1.2.3", "1.9.0"], &["1.2.2", "2.0.0"]);
    // Below 1.0 the minor is the breaking component…
    check("^0.2.3", &["0.2.3", "0.2.9"], &["0.3.0", "0.2.2"]);
    // …and below 0.1 the patch is.
    check("^0.0.3", &["0.0.3"], &["0.0.4", "0.1.0"]);
}

#[test]
fn tilde_bounds_by_how_much_was_written() {
    check("~1.2.3", &["1.2.3", "1.2.9"], &["1.3.0", "1.2.2"]);
    check("~1.2", &["1.2.0", "1.2.9"], &["1.3.0"]);
    check("~1", &["1.0.0", "1.9.9"], &["2.0.0"]);
}

#[test]
fn bounds_do_what_they_say() {
    check(">=1.2.3", &["1.2.3", "9.0.0"], &["1.2.2"]);
    check(">1.2.3", &["1.2.4"], &["1.2.3"]);
    check("<2.0.0", &["1.9.9"], &["2.0.0"]);
    check("<=2.0.0", &["2.0.0"], &["2.0.1"]);
}

#[test]
fn comparators_combine_as_a_conjunction() {
    check(">=1.2, <2.0", &["1.2.0", "1.9.9"], &["1.1.0", "2.0.0"]);
}

#[test]
fn a_prerelease_only_satisfies_a_range_that_names_one() {
    // Otherwise `^1.0.0` would accept `2.0.0-beta`: a prerelease of a version
    // the range explicitly excludes.
    check("^1.0.0", &["1.5.0"], &["2.0.0-beta", "1.5.0-rc.1"]);
    // Naming a prerelease at the same core version opts into that series.
    check(">=1.5.0-rc.1, <2.0.0", &["1.5.0-rc.2"], &["1.6.0-rc.1"]);
}

#[test]
fn an_unparseable_requirement_is_an_error_not_silently_any() {
    // A requirement that quietly means "any" is how you ship the wrong version.
    assert!(Requirement::parse("^not-a-version").is_err());
    assert!(Requirement::parse(">=1.0, garbage").is_err());
}

#[test]
fn intersect_is_conjunction() {
    let combined = req(">=1.2").intersect(&req("<2.0"));
    assert!(combined.matches(&v("1.5.0")));
    assert!(!combined.matches(&v("2.0.0")));
    // `any` is the identity, so it never widens or narrows the other side.
    assert_eq!(req("^1.2").intersect(&Requirement::any()), req("^1.2"));
    assert_eq!(Requirement::any().intersect(&req("^1.2")), req("^1.2"));
}

// ---- the three spellings --------------------------------------------------

fn parse_deps(v: serde_json::Value) -> Dependencies {
    serde_json::from_value(v).expect("valid dependencies")
}

#[test]
fn a_bare_list_still_works() {
    // Back-compat: the pre-requirement spelling means "any version, requires".
    let d = parse_deps(json!(["shared", "other"]));
    let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, vec!["shared", "other"]);
    assert!(d.iter().all(|x| x.requirement.is_any()));
    assert!(d.iter().all(|x| x.relation == Relation::Requires));
}

#[test]
fn a_map_of_name_to_requirement_parses() {
    let d = parse_deps(json!({ "acme/shared": "^1.2", "other": "*" }));
    let shared = d.iter().find(|x| x.name == "acme/shared").unwrap();
    assert!(shared.requirement.matches(&v("1.5.0")));
    assert!(!shared.requirement.matches(&v("2.0.0")));
}

#[test]
fn the_long_form_carries_a_relation() {
    let d = parse_deps(json!({
        "acme/legacy": { "version": ">=2, <4", "relation": "suggests" },
        "acme/rival": { "relation": "conflicts" },
        "acme/original": { "relation": "replaces" },
    }));
    let by = |n: &str| d.iter().find(|x| x.name == n).unwrap().relation;
    assert_eq!(by("acme/legacy"), Relation::Suggests);
    assert_eq!(by("acme/rival"), Relation::Conflicts);
    assert_eq!(by("acme/original"), Relation::Replaces);
}

#[test]
fn an_unknown_relation_is_refused_by_name() {
    let err = serde_json::from_value::<Dependencies>(json!({
        "x": { "relation": "recommends" }
    }))
    .unwrap_err()
    .to_string();
    assert!(err.contains("recommends"), "names the bad relation: {err}");
}

#[test]
fn dependencies_round_trip_through_the_map_form() {
    let original = parse_deps(json!({
        "acme/shared": "^1.2",
        "acme/legacy": { "version": ">=2, <4", "relation": "suggests" },
    }));
    let text = serde_json::to_value(&original).unwrap();
    assert_eq!(parse_deps(text), original);
}

#[test]
fn dependencies_survive_a_yaml_round_trip() {
    // The authored form is YAML, so the real round-trip goes through it.
    let original = parse_deps(json!({ "acme/shared": "^1.2" }));
    let yaml = open_harness::config::to_yaml(&original).unwrap();
    let back: Dependencies =
        open_harness::config::from_str(&yaml, open_harness::config::Format::Yaml).unwrap();
    assert_eq!(back, original);
}
