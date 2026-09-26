//! Assignment through a path expression (`select(...)` inside the LHS, #88):
//! the LHS resolves to concrete paths, and each is an ordinary surgical
//! splice, so only the matched lines change.

use edikt_yaml::{Document, parse, parse_expr};

const REGISTRY: &str = include_str!("../../../fixtures/yaml/registry.yaml");

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
fn one_match_changes_one_line() {
    let out = edit(
        REGISTRY,
        r#"(.npcs[] | select(.id == "tobin") | .status) = "found""#,
    );
    assert_eq!(
        changed(REGISTRY, &out),
        pairs(&[(
            "    status: missing  # since session 4",
            "    status: found  # since session 4"
        )])
    );
}

#[test]
fn multiple_matches_each_change() {
    let out = edit(
        REGISTRY,
        r#"(.npcs[] | select(.status == "alive") | .status) = "dead""#,
    );
    assert_eq!(
        changed(REGISTRY, &out),
        pairs(&[
            ("    status: alive", "    status: dead"),
            ("    status: alive", "    status: dead"),
        ])
    );
}

#[test]
fn nested_select_reaches_a_list_item() {
    let out = edit(
        REGISTRY,
        r#"(.npcs[] | select(.id == "tobin") | .tags[] | select(. == "informant")) = "traitor""#,
    );
    assert_eq!(
        changed(REGISTRY, &out),
        pairs(&[("      - informant", "      - traitor")])
    );
}

#[test]
fn update_and_add_assign_keep_style() {
    // `|=` sees the match's own value; the double quotes and comment stay.
    let out = edit(
        REGISTRY,
        r#"(.npcs[] | select(.id == "mara") | .name) |= ascii_upcase"#,
    );
    assert_eq!(
        changed(REGISTRY, &out),
        pairs(&[(
            r#"    name: "Mara Voss"   # harbourmaster"#,
            r#"    name: "MARA VOSS"   # harbourmaster"#
        )])
    );
    // `+=` appends into the flow sequence in place.
    let out = edit(
        REGISTRY,
        r#"(.npcs[] | select(.id == "mara") | .tags) += ["spy"]"#,
    );
    assert_eq!(
        changed(REGISTRY, &out),
        pairs(&[("    tags: [dock, ally]", "    tags: [dock, ally, spy]")])
    );
}

#[test]
fn zero_matches_is_a_byte_identical_no_op() {
    for expr in [
        r#"(.npcs[] | select(.id == "nobody") | .status) = "x""#,
        r#"(.npcs[] | select(.id == "nobody") | .status) |= . + "x""#,
        r#"del(.npcs[] | select(.id == "nobody"))"#,
        r#"(.nope[] | select(.id == "x") | .status) = "x""#,
    ] {
        assert_eq!(edit(REGISTRY, expr), REGISTRY, "{expr}");
    }
}

#[test]
fn del_through_select_removes_only_the_matches() {
    let out = edit(REGISTRY, r#"del(.npcs[] | select(.status == "alive"))"#);
    // Exactly what the hand-typed indices do, back to front.
    assert_eq!(out, edit(REGISTRY, "del(.npcs[2]) | del(.npcs[0])"));
    assert_eq!(
        out,
        "# NPC registry: one entry per character, keyed by id
npcs:

  # the smuggler arc
  - id: tobin
    name: 'Tobin Reed'
    status: missing  # since session 4
    tags:
      - smuggler
      - informant
"
    );
}

#[test]
fn select_creates_a_missing_key_on_each_match() {
    let out = edit(
        REGISTRY,
        r#"(.npcs[] | select(.id == "mara") | .faction) = "harbour""#,
    );
    assert!(
        out.contains("    tags: [dock, ally]\n    faction: harbour\n"),
        "{out}"
    );
}
