//! Appending to an array keeps its layout (jhheider/edikt#91): a new element
//! in a one-item-per-line array goes on its own line at the siblings'
//! indentation, keeping the trailing-comma style; an inline array stays inline.
//! `.a += [x]`, `.a[len] = x`, and a replacement that keeps the old elements
//! as a prefix all append the same way.

use edikt_toml::{Document, Value, parse, parse_expr};

fn edit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    doc.apply(&parse_expr(expr).unwrap()).unwrap();
    doc.to_source()
}

/// The length of the array at `path` in `src`.
fn len(src: &str, path: &str) -> usize {
    let got = edikt_core::eval(&parse_expr(path).unwrap(), &parse(src).unwrap().to_value());
    match got.unwrap().as_slice() {
        [Value::Array(items)] => items.len(),
        _ => panic!("`{path}` is not an array in:\n{src}"),
    }
}

/// Appending `"c"` to `.full` gives `want`, by every append spelling.
fn appends(src: &str, want: &str) {
    appends_at(".full", src, want);
}

fn appends_at(path: &str, src: &str, want: &str) {
    let at_len = format!(r#"{path}[{}] = "c""#, len(src, path));
    let add = format!(r#"{path} += ["c"]"#);
    let update = format!(r#"{path} |= . + ["c"]"#);
    for expr in [&add, &update, &at_len] {
        let got = edit(src, expr);
        assert_eq!(got, want, "expr `{expr}` on:\n{src}");
        assert!(parse(&got).is_ok(), "not TOML:\n{got}");
    }
}

#[test]
fn one_per_line_with_trailing_comma() {
    appends(
        "full = [\n    \"a\",\n    \"b\",\n]\n",
        "full = [\n    \"a\",\n    \"b\",\n    \"c\",\n]\n",
    );
}

#[test]
fn one_per_line_without_trailing_comma() {
    appends(
        "full = [\n  \"a\",\n  \"b\"\n]\n",
        "full = [\n  \"a\",\n  \"b\",\n  \"c\"\n]\n",
    );
}

#[test]
fn nested_in_a_table_with_an_indented_bracket() {
    appends_at(
        ".features.full",
        "[features]\n  full = [\n      \"a\",\n  ]\nother = 1\n",
        "[features]\n  full = [\n      \"a\",\n      \"c\",\n  ]\nother = 1\n",
    );
    // Empty: one level past the bracket's own indentation.
    appends_at(
        ".features.full",
        "[features]\n  full = [\n  ]\n",
        "[features]\n  full = [\n      \"c\",\n  ]\n",
    );
}

#[test]
fn comments_between_items_are_not_copied() {
    appends(
        "full = [\n    # first\n    \"a\",\n    # second\n    \"b\",\n]\n",
        "full = [\n    # first\n    \"a\",\n    # second\n    \"b\",\n    \"c\",\n]\n",
    );
}

#[test]
fn a_comment_beside_the_last_item_stays_with_it() {
    // With a trailing comma the comment is after the comma ...
    appends(
        "full = [\n    \"a\",\n    \"b\", # keep me\n]\n",
        "full = [\n    \"a\",\n    \"b\", # keep me\n    \"c\",\n]\n",
    );
    // ... without one it is before the bracket.
    appends(
        "full = [\n    \"a\",\n    \"b\" # keep me\n]\n",
        "full = [\n    \"a\",\n    \"b\", # keep me\n    \"c\"\n]\n",
    );
}

#[test]
fn an_own_line_comment_before_the_bracket_stays_above_the_new_item() {
    appends(
        "full = [\n    \"a\",\n    # \"b\",\n]\n",
        "full = [\n    \"a\",\n    # \"b\",\n    \"c\",\n]\n",
    );
}

#[test]
fn inline_arrays_stay_inline() {
    appends("full = [\"a\", \"b\"]\n", "full = [\"a\", \"b\", \"c\"]\n");
    appends("full = [\"a\"]\n", "full = [\"a\", \"c\"]\n");
    appends(
        "full = [ \"a\", \"b\" ]\n",
        "full = [ \"a\", \"b\", \"c\" ]\n",
    );
    appends("full = [\"a\",\"b\"]\n", "full = [\"a\",\"b\",\"c\"]\n");
    appends(
        "full = [\"a\", \"b\",]\n",
        "full = [\"a\", \"b\", \"c\",]\n",
    );
    appends("full = []\n", "full = [\"c\"]\n");
}

#[test]
fn items_inline_but_bracket_on_its_own_line() {
    appends(
        "full = [\"a\", \"b\",\n]\n",
        "full = [\"a\", \"b\", \"c\",\n]\n",
    );
}

#[test]
fn empty_multi_line_array_indents_one_level_past_its_bracket() {
    appends("full = [\n]\n", "full = [\n    \"c\",\n]\n");
    appends(
        "full = [\n    # nothing yet\n]\n",
        "full = [\n    # nothing yet\n    \"c\",\n]\n",
    );
}

#[test]
fn the_kept_prefix_is_untouched_bytes() {
    // Old elements keep their exact spelling (literal quotes, a datetime that
    // the value model sees as a string) when the new value only extends them.
    appends(
        "full = [\n    'a',\n    1979-05-27,\n]\n",
        "full = [\n    'a',\n    1979-05-27,\n    \"c\",\n]\n",
    );
}

#[test]
fn assigning_an_array_its_own_value_changes_nothing() {
    let src = "full = [\n    \"a\", # x\n    \"b\",\n]\n";
    assert_eq!(edit(src, ".full = .full"), src);
}

#[test]
fn a_replacement_that_is_not_an_extension_still_rewrites() {
    assert_eq!(
        edit("full = [\n    \"a\",\n    \"b\",\n]\n", r#".full = ["z"]"#),
        "full = [\"z\"]\n"
    );
}

#[test]
fn appending_several_items_lays_each_out() {
    assert_eq!(
        edit(
            "full = [\n    \"a\",\n]\n",
            r#".full += ["b", "c"] | .full[3] = "d""#
        ),
        "full = [\n    \"a\",\n    \"b\",\n    \"c\",\n    \"d\",\n]\n"
    );
}

#[test]
fn closing_bracket_on_the_last_items_line() {
    appends(
        "full = [\n    \"a\",\n    \"b\"]\n",
        "full = [\n    \"a\",\n    \"b\",\n    \"c\"]\n",
    );
}

#[test]
fn padded_empty_inline_array() {
    appends("full = [ ]\n", "full = [ \"c\" ]\n");
}

#[test]
fn appending_an_array_element_to_an_array_of_arrays() {
    assert_eq!(
        edit("m = [\n    [1, 2],\n]\n", ".m += [[3, 4]]"),
        "m = [\n    [1, 2],\n    [3, 4],\n]\n"
    );
}

#[test]
fn appending_to_an_array_of_tables_adds_a_block() {
    let src = "[[bin]]\nname = \"a\" # first\n\n[x]\ny = 1\n";
    let want = "[[bin]]\nname = \"a\" # first\n\n[[bin]]\nname = \"b\"\n\n[x]\ny = 1\n";
    assert_eq!(edit(src, r#".bin += [{name: "b"}]"#), want);
    assert_eq!(edit(src, r#".bin[1] = {name: "b"}"#), want);
}

#[test]
fn fan_out_appends_to_each_nested_array() {
    assert_eq!(
        edit("[a]\nx = [\n  1,\n]\ny = [2]\n", ".a[] += [9]"),
        "[a]\nx = [\n  1,\n  9,\n]\ny = [2, 9]\n"
    );
}
