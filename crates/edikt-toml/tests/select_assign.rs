//! Assignment through a path expression (`select(...)` inside the LHS, #88)
//! over an array of tables: each match is an ordinary surgical edit, so only
//! the matched lines change.

use edikt_toml::{Document, parse, parse_expr};

const BINS: &str = include_str!("../../../fixtures/toml/array-of-tables.toml");

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
fn assign_update_and_add_through_select() {
    let out = edit(
        BINS,
        r#"(.bin[] | select(.name == "cli") | .path) = "src/main.rs""#,
    );
    assert_eq!(
        changed(BINS, &out),
        pairs(&[(r#"path = "src/cli.rs""#, r#"path = "src/main.rs""#)])
    );

    // `|=` keeps the trailing comment on the matched line.
    let out = edit(
        BINS,
        r#"(.bin[] | select(.name == "server") | .name) |= . + "d""#,
    );
    assert_eq!(
        changed(BINS, &out),
        pairs(&[(
            r#"name = "server"   # the daemon"#,
            r#"name = "serverd"   # the daemon"#
        )])
    );

    // Multiple matches, and a nested select into an inline array.
    let out = edit(
        BINS,
        r#"(.bin[] | select(.path | startswith("src/")) | .path) |= ltrimstr("src/")"#,
    );
    assert_eq!(
        changed(BINS, &out),
        pairs(&[
            (r#"path = "src/server.rs""#, r#"path = "server.rs""#),
            (r#"path = "src/cli.rs""#, r#"path = "cli.rs""#),
        ])
    );
    let out = edit(
        BINS,
        r#"(.bin[] | select(has("features")) | .features[] | select(. == "b")) = "B""#,
    );
    assert_eq!(
        changed(BINS, &out),
        pairs(&[(
            r#"features = ["a", "b", "c"]"#,
            r#"features = ["a", "B", "c"]"#
        )])
    );
}

#[test]
fn zero_matches_is_a_byte_identical_no_op() {
    for expr in [
        r#"(.bin[] | select(.name == "nope") | .path) = "x""#,
        r#"del(.bin[] | select(.name == "nope"))"#,
    ] {
        assert_eq!(edit(BINS, expr), BINS, "{expr}");
    }
}

#[test]
fn del_through_select_matches_the_hand_typed_index() {
    let out = edit(BINS, r#"del(.bin[] | select(.name == "server"))"#);
    assert_eq!(out, edit(BINS, "del(.bin[0])"));
    assert!(!out.contains("server"), "{out}");
    assert!(out.contains("name = \"cli\""), "{out}");
}
