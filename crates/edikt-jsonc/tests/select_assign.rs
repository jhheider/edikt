//! Assignment through a path expression (`select(...)` inside the LHS, #88):
//! each match is an ordinary single-node splice, so comments, trailing commas
//! and layout around it survive, and only the matched lines change.

use edikt_jsonc::{Document, parse, parse_expr};

const REGISTRY: &str = include_str!("../../../fixtures/jsonc/registry.jsonc");

fn edit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    doc.apply(&parse_expr(expr).unwrap())
        .unwrap_or_else(|e| panic!("`{expr}`: {e}"));
    doc.to_source()
}

/// The `(before, after)` pairs of lines that differ, for a same-length edit.
fn changed(before: &str, after: &str) -> Vec<(String, String)> {
    let (b, a): (Vec<_>, Vec<_>) = (before.lines().collect(), after.lines().collect());
    assert_eq!(b.len(), a.len(), "line count changed:\n{after}");
    b.iter()
        .zip(&a)
        .filter(|(x, y)| x != y)
        .map(|(x, y)| (x.to_string(), y.to_string()))
        .collect()
}

fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
    p.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn one_match_in_a_compact_and_an_expanded_element() {
    let out = edit(
        REGISTRY,
        r#"(.plugins[] | select(.id == "lint") | .enabled) = true"#,
    );
    assert_eq!(
        changed(REGISTRY, &out),
        pairs(&[(r#"      "enabled": false,"#, r#"      "enabled": true,"#)])
    );
    let out = edit(
        REGISTRY,
        r#"(.plugins[] | select(.id == "fmt") | .version) = "1.3.0""#,
    );
    assert_eq!(
        changed(REGISTRY, &out),
        pairs(&[(
            r#"    { "id": "fmt", "enabled": true, "version": "1.2.0" }, // formatter"#,
            r#"    { "id": "fmt", "enabled": true, "version": "1.3.0" }, // formatter"#
        )])
    );
}

#[test]
fn multiple_matches_and_a_nested_predicate() {
    let out = edit(
        REGISTRY,
        r#"(.plugins[] | select(.enabled) | .version) |= "v" + ."#,
    );
    assert_eq!(
        changed(REGISTRY, &out),
        pairs(&[
            (
                r#"    { "id": "fmt", "enabled": true, "version": "1.2.0" }, // formatter"#,
                r#"    { "id": "fmt", "enabled": true, "version": "v1.2.0" }, // formatter"#
            ),
            (
                r#"      "version": "2.0.0" /* bumped */,"#,
                r#"      "version": "v2.0.0" /* bumped */,"#
            ),
        ])
    );
    // A select whose predicate is itself a pipe, then a second select.
    let out = edit(
        REGISTRY,
        r#"(.plugins[] | select(.id | startswith("t")) | select(.enabled) | .enabled) = false"#,
    );
    assert_eq!(
        changed(REGISTRY, &out),
        pairs(&[(r#"      "enabled": true,"#, r#"      "enabled": false,"#)])
    );
}

#[test]
fn zero_matches_is_a_byte_identical_no_op() {
    for expr in [
        r#"(.plugins[] | select(.id == "nope") | .enabled) = true"#,
        r#"(.plugins[] | select(.id == "nope") | .version) += "x""#,
        r#"del(.plugins[] | select(.id == "nope"))"#,
    ] {
        assert_eq!(edit(REGISTRY, expr), REGISTRY, "{expr}");
    }
}

#[test]
fn del_through_select_matches_the_hand_typed_indices() {
    let out = edit(REGISTRY, r#"del(.plugins[] | select(.enabled))"#);
    assert_eq!(out, edit(REGISTRY, "del(.plugins[2]) | del(.plugins[0])"));
    assert!(out.contains("the linter is pinned"), "{out}");
    assert!(
        !out.contains("\"fmt\"") && !out.contains("\"test\""),
        "{out}"
    );
}
