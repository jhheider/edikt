use crate::{Document, parse, parse_expr};

/// Apply `expr` to `src` and return the new source.
fn edit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    doc.apply(&parse_expr(expr).unwrap()).unwrap();
    doc.to_source()
}

fn edit_err(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    doc.apply(&parse_expr(expr).unwrap())
        .unwrap_err()
        .to_string()
}

#[test]
fn new_key_takes_block_layout_like_its_siblings() {
    // The case from jhheider/edikt#83, verbatim.
    let src = "places:\n  city:\n    name: Bahía Carmesí\n    aliases:\n      - the bay\n";
    assert_eq!(
        edit(
            src,
            r#".places.city.deprecated_aliases = ["Puerto Jubilar", "Crimson Bay"]"#
        ),
        format!("{src}    deprecated_aliases:\n      - Puerto Jubilar\n      - Crimson Bay\n")
    );
    // A mapping, nested collections, and empties (only `[]`/`{}` spell those).
    assert_eq!(
        edit("a: 1\n", r#".b = {c: [1, {d: "x"}], e: [], f: {}}"#),
        "a: 1\nb:\n  c:\n    - 1\n    - d: x\n  e: []\n  f: {}\n"
    );
    // No trailing newline stays that way.
    assert_eq!(edit("a: 1", ".b = [2]"), "a: 1\nb:\n  - 2");
}

#[test]
fn scalar_becomes_a_block() {
    // The comment beside the old scalar stays on the key line.
    assert_eq!(
        edit("a: old  # note\nb: 2\n", ".a = [1, 2]"),
        "a:  # note\n  - 1\n  - 2\nb: 2\n"
    );
    assert_eq!(edit("a: old\n", ".a = {k: \"v\"}"), "a:\n  k: v\n");
    // An empty value (`a:`) fills in, keeping its comment.
    assert_eq!(
        edit("a:  # todo\nb: 1\n", ".a = {x: 1}"),
        "a:  # todo\n  x: 1\nb: 1\n"
    );
    // A value on its own line under the key is replaced line for line.
    assert_eq!(edit("a:\n  old\nb: 1\n", ".a = [1]"), "a:\n  - 1\nb: 1\n");
    // Deeper keys indent from their own key.
    assert_eq!(edit("m:\n  a: 1\n", ".m.a = [1]"), "m:\n  a:\n    - 1\n");
}

#[test]
fn sequence_item_becomes_a_compact_block() {
    // The first line takes the item's place after its dash, and the
    // item's comment stays on that line.
    assert_eq!(
        edit("xs:\n  - one  # c\n  - two\n", ".xs[0] = {k: 1, j: [2]}"),
        "xs:\n  - k: 1  # c\n    j:\n      - 2\n  - two\n"
    );
    assert_eq!(
        edit("xs:\n  - one\n", ".xs[0] = [7, 8]"),
        "xs:\n  - - 7\n    - 8\n"
    );
    // An empty item gets its space after the dash.
    assert_eq!(
        edit("xs:\n  -\n  - 2\n", ".xs[0] = {x: 1}"),
        "xs:\n  - x: 1\n  - 2\n"
    );
    // An anchored item keeps its anchor on the dash line, the block below.
    assert_eq!(
        edit("xs:\n  - &a one\n", ".xs[0] = {k: 1}"),
        "xs:\n  - &a\n    k: 1\n"
    );
}

#[test]
fn block_becomes_a_scalar() {
    // The key's comment stays, the items and their comments go with the
    // block, and what follows is untouched.
    assert_eq!(
        edit("a:  # note\n  - 1  # one\n  - 2\nb: 2\n", r#".a = "s""#),
        "a: s  # note\nb: 2\n"
    );
    assert_eq!(edit("a:\n  x: 1\nb: 2\n", ".a = []"), "a: []\nb: 2\n");
    // An anchor stays; a core tag that no longer fits goes.
    assert_eq!(edit("a: &x\n  - 1\n", ".a = 5"), "a: &x 5\n");
    assert_eq!(edit("a: !!seq  # c\n  - 1\n", ".a = 5"), "a: 5  # c\n");
    // A compact item collapses onto its dash line.
    assert_eq!(
        edit("xs:\n  - k: 1\n    j: 2\n  - two\n", ".xs[0] = 5"),
        "xs:\n  - 5\n  - two\n"
    );
}

#[test]
fn block_replaced_by_another_shape() {
    // A different kind indents per the file's style from the key.
    assert_eq!(
        edit("a:\n  - 1\nb: 2\n", ".a = {x: 1}"),
        "a:\n  x: 1\nb: 2\n"
    );
    assert_eq!(edit("a:\n  x: 1\nb: 2\n", ".a = [1]"), "a:\n  - 1\nb: 2\n");
    // The same kind, reshaped, keeps the old block's own column, and a
    // comment above its first item stays.
    assert_eq!(
        edit("a:\n  # head\n     - 1\n     - 2\nb: 1\n", r#".a = ["x"]"#),
        "a:\n  # head\n     - x\nb: 1\n"
    );
    // A compact item swaps kind in place.
    assert_eq!(edit("- k: 1\n  j: 2\n", ".[0] = [1, 2]"), "- - 1\n  - 2\n");
}

#[test]
fn nested_blocks_edit_only_what_changed() {
    // Same shape, one deep change: only that value's bytes move, and the
    // comment beside its sibling survives.
    assert_eq!(
        edit(
            "a:\n  b:\n    c: 1  # deep\n    d: 2\n",
            ".a = {b: {c: 1, d: [9]}}"
        ),
        "a:\n  b:\n    c: 1  # deep\n    d:\n      - 9\n"
    );
    // `|=` growing a list appends; item comments stay.
    assert_eq!(
        edit(
            "tags:  # t\n  - a  # first\n  - b\n",
            r#".tags |= . + ["c"]"#
        ),
        "tags:  # t\n  - a  # first\n  - b\n  - c\n"
    );
    // New keys go after the existing ones.
    assert_eq!(
        edit("m:\n  a: 1  # keep\n", ".m = {a: 1, b: {c: 2}}"),
        "m:\n  a: 1  # keep\n  b:\n    c: 2\n"
    );
    // "Unchanged" is exact, not jq's `1 == 1.0`: the new spelling lands.
    assert_eq!(edit("m:\n  a: 1\n", ".m = {a: 1.0}"), "m:\n  a: 1.0\n");
    // Assigning a collection its own value touches nothing.
    let src = "a:\n  - 1   # odd spacing\n  - {x: 1}\n";
    assert_eq!(edit(src, ".a = .a"), src);
    // A removed or reordered key means a wholesale rewrite.
    assert_eq!(
        edit("m:\n  a: 1  # gone\n  b: 2\n", ".m = {b: 2, a: 1}"),
        "m:\n  b: 2\n  a: 1\n"
    );
    // A merged-in key that stays unchanged isn't copied in as explicit.
    assert_eq!(
        edit(
            "base: &b\n  t: 30\nprod:\n  <<: *b\n  r: 5\n",
            ".prod = {r: 5, t: 30, x: 1}"
        ),
        "base: &b\n  t: 30\nprod:\n  <<: *b\n  r: 5\n  x: 1\n"
    );
}

#[test]
fn flow_context_stays_flow() {
    // Inside a flow collection, a new value is spelled flow.
    assert_eq!(
        edit("f: {a: 1}\n", r#".f.a = {z: "a,b"}"#),
        "f: {a: {z: \"a,b\"}}\n"
    );
    // A flow collection replaced keeps flow style and its comment.
    assert_eq!(
        edit("f: [1, 2]  # c\n", ".f = {x: [1]}"),
        "f: {x: [1]}  # c\n"
    );
    assert_eq!(edit("f: [1,\n  2]\n", ".f = [3]"), "f: [3]\n");
    // A flow value's elements change in place.
    assert_eq!(edit("f: [1,2]\n", ".f = [1, 3]"), "f: [1,3]\n");
    // New keys and items join a flow collection in flow style.
    assert_eq!(
        edit("f: {a: 1}\n", ".f.b = [1, 2]"),
        "f: {a: 1, b: [1, 2]}\n"
    );
    assert_eq!(edit("f: {}\n", ".f.b = 1"), "f: {b: 1}\n");
    assert_eq!(edit("f: [1, 2]\n", ".f += [{a: 1}]"), "f: [1, 2, {a: 1}]\n");
    assert_eq!(edit("f: [1,]\n", ".f += [2]"), "f: [1, 2,]\n");
    assert_eq!(edit("f: []\n", ".f += [3]"), "f: [3]\n");
    assert_eq!(edit("f: [1, 2]\n", ".f |= . + [3]"), "f: [1, 2, 3]\n");
    // Growing a multi-line flow collection would reflow it: refused.
    assert!(edit_err("f: [1,\n  2]\n", ".f += [3]").contains("multi-line flow"));
    assert!(edit_err("f: {a: 1,\n  b: 2}\n", ".f.c = 3").contains("multi-line flow"));
}

#[test]
fn follows_the_files_indent_width() {
    // Four-space file: four-space levels.
    assert_eq!(
        edit("a:\n    x: 1\nb: 3\n", ".b = {k: [1, 2]}"),
        "a:\n    x: 1\nb:\n    k:\n        - 1\n        - 2\n"
    );
    // Indentless sequences stay indentless.
    assert_eq!(
        edit("a:\n    x:\n    - 1\nb: 3\n", ".b = {k: [1, 2]}"),
        "a:\n    x:\n    - 1\nb:\n    k:\n    - 1\n    - 2\n"
    );
    assert_eq!(
        edit("xs:\n- 1\n", ".xs += [{a: 1, b: [2]}]"),
        "xs:\n- 1\n- a: 1\n  b:\n  - 2\n"
    );
    // Nothing nested to learn from: two spaces.
    assert_eq!(edit("a: 1\n", ".a = {b: 1}"), "a:\n  b: 1\n");
}

#[test]
fn append_collections_to_a_block_sequence() {
    assert_eq!(
        edit("xs:\n  - 1\n", ".xs += [{a: 1, b: 2}, [3]]"),
        "xs:\n  - 1\n  - a: 1\n    b: 2\n  - - 3\n"
    );
    // An anchored sequence: the dash is past the anchor's line.
    assert_eq!(
        edit("xs: &s\n  - 1\n", ".xs += [2]"),
        "xs: &s\n  - 1\n  - 2\n"
    );
    assert_eq!(
        edit("xs:\n  - 1", ".xs += [{a: 1}]"),
        "xs:\n  - 1\n  - a: 1"
    );
}

#[test]
fn assignment_creates_missing_parents() {
    // jhheider/edikt#85: `=` creates every missing level, laid out like
    // any new collection. Under a block mapping, at the file's width:
    assert_eq!(
        edit("a:\n    x: 1\n", ".a.b.c = [1]"),
        "a:\n    x: 1\n    b:\n        c:\n            - 1\n"
    );
    // At the root, and after a file without a trailing newline.
    assert_eq!(edit("a: 1\n", ".b.c = 1"), "a: 1\nb:\n  c: 1\n");
    assert_eq!(edit("a: 1", ".b.c = 1"), "a: 1\nb:\n  c: 1");
    // Under a compact list item, at that mapping's column.
    assert_eq!(
        edit("xs:\n  - a: 1\n", r#".xs[0].b.c = "v""#),
        "xs:\n  - a: 1\n    b:\n      c: v\n"
    );
    // Indentless sequences stay indentless in the created levels.
    assert_eq!(
        edit("m:\n    x:\n    - 1\n", ".m.k.l = [1, 2]"),
        "m:\n    x:\n    - 1\n    k:\n        l:\n        - 1\n        - 2\n"
    );
    // In an empty or flow parent, flow.
    assert_eq!(edit("f: {}\n", ".f.a.b = 1"), "f: {a: {b: 1}}\n");
    assert_eq!(
        edit("f: {a: 1}\n", ".f.b.c = [1]"),
        "f: {a: 1, b: {c: [1]}}\n"
    );
    // CRLF files get CRLF lines.
    assert_eq!(edit("a: 1\r\n", ".b.c = 1"), "a: 1\r\nb:\r\n  c: 1\r\n");
    // Every document of a stream gets the missing levels.
    assert_eq!(
        edit("---\nk: A\n---\nk: B\nm:\n  x: 1\n", ".m.z = 1"),
        "---\nk: A\nm:\n  z: 1\n---\nk: B\nm:\n  x: 1\n  z: 1\n"
    );
    // No array elements out of thin air, and no key inside a scalar.
    assert!(edit_err("a: 1\n", ".b.c[0] = 1").contains("cannot create array elements"));
    assert!(edit_err("a: 1\n", ".a.b = 1").contains("path not found"));
    // `|=` and `+=` still need the path to exist.
    assert!(edit_err("a: 1\n", ".b.c |= 1").contains("path not found"));
}

#[test]
fn roots_aliases_streams_and_crlf() {
    assert_eq!(edit("- 1\n- 2\n", ". = {a: [1]}"), "a:\n  - 1\n");
    assert_eq!(edit("a: 1\n", ". = [1]"), "- 1\n");
    assert_eq!(edit("hello  # c\n", ". = {a: 1}"), "a: 1  # c\n");
    // An alias replaced by a block writes the block; the anchor's target
    // replaced keeps its anchor for the aliases.
    assert_eq!(
        edit("base: &b\n  - 1\nother: *b\n", ".other = [1, 2]"),
        "base: &b\n  - 1\nother:\n  - 1\n  - 2\n"
    );
    assert_eq!(
        edit("base: &b\n  - 1\nother: *b\n", ".base = {x: 1}"),
        "base: &b\n  x: 1\nother: *b\n"
    );
    // Every document of a stream gets the block.
    assert_eq!(
        edit("---\na: 1\n---\na: 2\n", ".a = [3]"),
        "---\na:\n  - 3\n---\na:\n  - 3\n"
    );
    // CRLF files get CRLF lines.
    assert_eq!(
        edit("a: 1\r\nb: 2\r\n", ".a = [1, 2]"),
        "a:\r\n  - 1\r\n  - 2\r\nb: 2\r\n"
    );
    assert_eq!(
        edit("a:\r\n  - 1\r\nb: 2\r\n", ".a = 5"),
        "a: 5\r\nb: 2\r\n"
    );
}
