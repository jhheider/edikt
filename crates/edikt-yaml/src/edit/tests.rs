//! Deletes and edits through flow collections, and the value guard
//! (jhheider/edikt#111).

use super::Strictness;
use super::guard::Intent;
use crate::{Document, Step, parse, parse_expr};

fn edit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    doc.apply(&parse_expr(expr).unwrap())
        .unwrap_or_else(|e| panic!("{expr} over {src:?}: {e}"));
    doc.to_source()
}

fn refuse(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    let err = doc.apply(&parse_expr(expr).unwrap()).unwrap_err();
    // A refusal changes nothing.
    assert_eq!(doc.to_source(), src, "{expr}");
    err.to_string()
}

#[test]
fn deletes_one_flow_element_not_its_key() {
    // The reports: the whole `k` entry went, exit 0.
    assert_eq!(edit("k: [1, 2]\nz: 3\n", "del(.k[1])"), "k: [1]\nz: 3\n");
    assert_eq!(
        edit("k: {a: 1, b: 2}\nz: 3\n", "del(.k.a)"),
        "k: {b: 2}\nz: 3\n"
    );
    assert_eq!(
        edit("items:\n  - [1, 2]\n  - [3]\n", "del(.items[0][1])"),
        "items:\n  - [1]\n  - [3]\n"
    );
    // And `del(...[])` on a flow item was a no-op.
    assert_eq!(
        edit("items:\n  - [1, 2]\n  - [3]\n", "del(.items[0][])"),
        "items:\n  - []\n  - [3]\n"
    );
}

#[test]
fn single_line_flow_keeps_its_separators() {
    let del = |i: usize| edit("k: [1, 2, 3]\n", &format!("del(.k[{i}])"));
    assert_eq!(del(0), "k: [2, 3]\n");
    assert_eq!(del(1), "k: [1, 3]\n");
    assert_eq!(del(2), "k: [1, 2]\n");
    // A trailing comma stays trailing; inner padding stays.
    assert_eq!(edit("k: [1, 2, 3,]\n", "del(.k[2])"), "k: [1, 2,]\n");
    assert_eq!(edit("k: [1, 2, 3, ]\n", "del(.k[0])"), "k: [2, 3, ]\n");
    assert_eq!(edit("k: [ x , y ]\n", "del(.k[1])"), "k: [ x ]\n");
    assert_eq!(edit("k: [ x , y ]\n", "del(.k[0])"), "k: [ y ]\n");
    assert_eq!(edit("k: [ x , y , z ]\n", "del(.k[1])"), "k: [ x , z ]\n");
    // The only element leaves the empty collection.
    assert_eq!(edit("k: [ 1 ]\n", "del(.k[0])"), "k: []\n");
    assert_eq!(edit("k: [1,]\n", "del(.k[0])"), "k: []\n");
    assert_eq!(edit("k: {a: 1}\n", "del(.k.a)"), "k: {}\n");
    // Quoted flow indicators inside elements don't confuse the scan.
    assert_eq!(
        edit("k: [\"a, b\", 'c]', d]\n", "del(.k[1])"),
        "k: [\"a, b\", d]\n"
    );
    // Implicit nulls, anchors and tags go with their element.
    assert_eq!(edit("k: {a, b: , c: 1}\n", "del(.k.b)"), "k: {a, c: 1}\n");
    assert_eq!(edit("k: {a, b: , c: 1}\n", "del(.k.a)"), "k: {b: , c: 1}\n");
    assert_eq!(edit("k: [&a 1, !!str 2]\n", "del(.k[0])"), "k: [!!str 2]\n");
    assert_eq!(edit("k: &x [1, 2]\n", "del(.k[0])"), "k: &x [2]\n");
    assert_eq!(edit("a: !!set {x, y}\n", "del(.a.x)"), "a: !!set {y}\n");
    // A root flow collection.
    assert_eq!(edit("[1, 2]\n", "del(.[0])"), "[2]\n");
    assert_eq!(edit("{a: 1}\n", "del(.a)"), "{}\n");
}

#[test]
fn multi_line_flow_loses_the_element_line() {
    let src = "k: [\n  1,\n  2,\n  3\n]\n";
    assert_eq!(edit(src, "del(.k[0])"), "k: [\n  2,\n  3\n]\n");
    assert_eq!(edit(src, "del(.k[1])"), "k: [\n  1,\n  3\n]\n");
    // No trailing comma before: none after.
    assert_eq!(edit(src, "del(.k[2])"), "k: [\n  1,\n  2\n]\n");
    assert_eq!(
        edit("k: {\n  a: 1,\n  b: 2,\n}\n", "del(.k.b)"),
        "k: {\n  a: 1,\n}\n"
    );
    // Several to a line.
    assert_eq!(
        edit("k: [1, 2,\n  3, 4]\n", "del(.k[1])"),
        "k: [1,\n  3, 4]\n"
    );
    assert_eq!(
        edit("k: [1, 2,\n  3, 4]\n", "del(.k[2])"),
        "k: [1, 2,\n  4]\n"
    );
    assert_eq!(edit("k: [\n  1,\n  2]\n", "del(.k[1])"), "k: [\n  1]\n");
    // CRLF throughout.
    assert_eq!(
        edit("k: [\r\n  1,\r\n  2\r\n]\r\nz: 3\r\n", "del(.k[1])"),
        "k: [\r\n  1\r\n]\r\nz: 3\r\n"
    );
    assert_eq!(
        edit("k: [1, 2]\r\nz: 3\r\n", "del(.k[1])"),
        "k: [1]\r\nz: 3\r\n"
    );
}

#[test]
fn flow_comments_stay_unless_on_the_deleted_line() {
    let src = "k: [\n  1,  # one\n  # about two\n  2,  # two\n  3  # three\n]\n";
    assert_eq!(
        edit(src, "del(.k[0])"),
        "k: [\n  # about two\n  2,  # two\n  3  # three\n]\n"
    );
    assert_eq!(
        edit(src, "del(.k[2])"),
        "k: [\n  1,  # one\n  # about two\n  2  # two\n]\n"
    );
    // A comment of its own keeps the emptied collection open around it.
    assert_eq!(
        edit("k: [\n  # only\n  1\n]\n", "del(.k[0])"),
        "k: [\n  # only\n]\n"
    );
    assert_eq!(edit("k: [\n  1 # c\n]\n", "del(.k[0])"), "k: []\n");
    // A comment after the first element's comma: `[#` wouldn't parse.
    assert_eq!(edit("k: [1, # c\n  2]\n", "del(.k[0])"), "k: [ # c\n  2]\n");
    assert_eq!(edit("k: [1, # c\n  2]\n", "del(.k[1])"), "k: [1 # c\n]\n");
    // `del(.k[])` ends at the bracket, leaving the line's comment.
    assert_eq!(edit("k: [1, 2] # c\n", "del(.k[])"), "k: [] # c\n");
}

#[test]
fn nested_flow_and_single_pairs() {
    let src = "k: [1, [2, 3], {a: [4, 5]}]\n";
    assert_eq!(edit(src, "del(.k[1][0])"), "k: [1, [3], {a: [4, 5]}]\n");
    assert_eq!(edit(src, "del(.k[2].a[1])"), "k: [1, [2, 3], {a: [4]}]\n");
    assert_eq!(edit(src, "del(.k[2].a)"), "k: [1, [2, 3], {}]\n");
    assert_eq!(edit(src, "del(.k[1])"), "k: [1, {a: [4, 5]}]\n");
    assert_eq!(
        edit("k: [{a: 1, b: 2}, {a: 3}]\n", "del(.k[].a)"),
        "k: [{b: 2}, {}]\n"
    );
    assert_eq!(
        edit("k: [1, 2, 3]\n", "del(.k[] | select(. == 2))"),
        "k: [1, 3]\n"
    );
    // A `[k: v]` pair: libyaml's span for it runs past its comma.
    assert_eq!(edit("x: [a: 1, b]\n", "del(.x[0])"), "x: [b]\n");
    assert_eq!(edit("x: [b, a: 1]\n", "del(.x[1])"), "x: [b]\n");
    assert_eq!(edit("x: [a: 1, b]\n", "del(.x[0].a)"), "x: [{}, b]\n");
    assert_eq!(edit("x: [p: 1, q]\n", ".x[0] = 2"), "x: [2, q]\n");
    assert!(
        refuse("x: [p: 1, q]\n", ".x[0].r = 2")
            .contains("cannot add a key to the single-pair mapping at .x[0]")
    );
    // An explicit `?` key is refused, not guessed at.
    assert!(refuse("k: {? a : 1, b: 2}\n", "del(.k.b)").contains("flow collection"));
}

#[test]
fn a_last_block_element_leaves_its_collection_empty() {
    // Not null (`a:`), which is another value.
    assert_eq!(edit("a:\n  b: 1\nz: 1\n", "del(.a.b)"), "a: {}\nz: 1\n");
    assert_eq!(edit("a:\n  - 1\nz: 1\n", "del(.a[0])"), "a: []\nz: 1\n");
    assert_eq!(edit("a: &x\n  b: 1\n", "del(.a.b)"), "a: &x {}\n");
    assert_eq!(edit("a: 1\n", "del(.a)"), "{}\n");
    assert_eq!(edit("# head\n- 1\n", "del(.[0])"), "# head\n[]\n");
    assert_eq!(edit("- a: 1\n- 2\n", "del(.[0].a)"), "- {}\n- 2\n");
    assert_eq!(edit("- - 1\n- 2\n", "del(.[0][0])"), "- []\n- 2\n");
}

#[test]
fn compact_items_keep_their_dash() {
    // The first key of `- a: 1` shares the dash's line; deleting its line
    // took the whole item (and moved its other keys out of it).
    assert_eq!(edit("- a: 1\n  b: 2\n", "del(.[0].a)"), "- b: 2\n");
    assert_eq!(edit("- a:\n    x: 1\n  b: 2\n", "del(.[0].a)"), "- b: 2\n");
    assert_eq!(edit("- a: 1\n  b: 2\n", "del(.[0].b)"), "- a: 1\n");
    assert_eq!(
        edit("a:\n  - - 1\n    - 2\n", "del(.a[0][0])"),
        "a:\n  - - 2\n"
    );
    assert!(refuse("- a: 1 # one\n  # two\n  b: 2\n", "del(.[0].a)").contains("compact `- ` item"));
}

#[test]
fn a_multi_line_flow_value_goes_whole() {
    // Its span ends at the closing bracket, lines after its last item.
    assert_eq!(edit("k: {\n  a: 1\n}\nz: 2\n", "del(.k)"), "z: 2\n");
    assert_eq!(
        edit("x:\n  k: [\n    1\n  ]\n", ".x.j = 1"),
        "x:\n  k: [\n    1\n  ]\n  j: 1\n"
    );
    assert_eq!(
        edit("x:\n  - [\n    1\n  ]\n", ".x += [2]"),
        "x:\n  - [\n    1\n  ]\n  - 2\n"
    );
}

#[test]
fn setting_an_implicit_null_keeps_its_indicator() {
    assert_eq!(edit("a:\nb: 1\n", ".a = 1"), "a: 1\nb: 1\n");
    assert_eq!(edit("a: # c\nb: 1\n", ".a = 1"), "a: 1 # c\nb: 1\n");
    assert_eq!(edit("a:\r\nb: 1\r\n", ".a = 1"), "a: 1\r\nb: 1\r\n");
    assert_eq!(edit("-\n- 1\n", ".[0] = 5"), "- 5\n- 1\n");
    assert_eq!(edit("k: {a, b: 1}\n", ".k.a = 1"), "k: {a: 1, b: 1}\n");
    assert_eq!(edit("k: {b: 1, a}\n", ".k.a = [1]"), "k: {b: 1, a: [1]}\n");
    assert_eq!(edit("k: [a: , b]\n", ".k[0].a = 1"), "k: [a: 1, b]\n");
    assert!(refuse("? a\nb: 1\n", ".a = 1").contains("`?` key"));
}

#[test]
fn deleting_an_anchor_its_alias_names_is_refused() {
    // The alias would read as null.
    assert!(refuse("- a: &x 1\n  b: *x\n", "del(.[0].a)").contains("remove anchor &x, which *x"));
    assert!(refuse("a: &x 1\nb: *x\n", "del(.a)").contains("anchor &x"));
    // Unless it is the alias going too, or nothing names it.
    assert_eq!(edit("a: &x 1\nb: *x\n", "del(.b)"), "a: &x 1\n");
    assert_eq!(edit("a: &x 1\nb: 2\n", "del(.a)"), "b: 2\n");
}

#[test]
fn the_guard_undoes_a_splice_that_changes_other_values() {
    let src = "k: [1, 2]\nz: 3\n";
    let mut doc = parse(src).unwrap();
    // A primitive that deletes the wrong thing: `.z` for `.k[1]`.
    let k1 = [Step::Field("k".into()), Step::Index(1)];
    let err = doc
        .guarded(0, &k1, Intent::Delete, Strictness::Strict, |d| {
            d.delete(0, &[Step::Field("z".into())])
        })
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot edit .k[1] in place"), "{err}");
    assert_eq!(doc.to_source(), src);
    // A strict edit that silently does nothing is caught too.
    let err = doc
        .guarded(0, &k1, Intent::Delete, Strictness::Strict, |_| Ok(()))
        .unwrap_err();
    assert!(err.to_string().contains(".k[1]"));
    // A lenient one (a document the path misses) may.
    doc.guarded(0, &k1, Intent::Delete, Strictness::Lenient, |_| Ok(()))
        .unwrap();
    // The right primitive passes.
    doc.guarded(0, &k1, Intent::Delete, Strictness::Strict, |d| {
        d.delete(0, &k1)
    })
    .unwrap();
    assert_eq!(doc.to_source(), "k: [1]\nz: 3\n");
}
