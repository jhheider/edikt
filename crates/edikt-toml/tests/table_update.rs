//! An update that leaves a `[table]`'s value alone, or changes only what is
//! inside it, keeps it a standard table: `.[] |= .` used to rewrite every
//! `[t]` as `t = { ... }`, a layout change from an identity edit.

use edikt_toml::{Document, parse, parse_expr};

fn edit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    doc.apply(&parse_expr(expr).unwrap()).unwrap();
    doc.to_source()
}

const SRC: &str = "top = 1\n\n# the t table\n[t]\na = 1 # one\nb = \"x\"\n\n[t.sub]\nc = [1, 2]\n\n[[bin]]\nname = \"a\"\n";

#[test]
fn identity_updates_change_nothing() {
    for expr in [
        ".[] |= .",
        ".t |= .",
        ".t.sub |= .",
        ".bin |= .",
        ".bin[] |= .",
    ] {
        assert_eq!(edit(SRC, expr), SRC, "`{expr}`");
    }
}

#[test]
fn an_update_inside_a_table_edits_just_its_keys() {
    // Changed key in place, new key appended, dropped key deleted; the
    // header, comments and the sub-table are untouched.
    assert_eq!(
        edit(SRC, r#".t |= {a: 2, sub: .sub, d: true}"#),
        "top = 1\n\n# the t table\n[t]\na = 2 # one\nd = true\n\n[t.sub]\nc = [1, 2]\n\n[[bin]]\nname = \"a\"\n"
    );
}

/// The document itself is a standard table too (jhheider/edikt#107): an
/// identity update of `.` changes nothing, where it used to error "cannot set
/// the whole document", and any other update goes key by key.
#[test]
fn the_document_updates_like_a_table() {
    for expr in [". |= .", ". = .", ". |= {top: .top, t: .t, bin: .bin}"] {
        assert_eq!(edit(SRC, expr), SRC, "`{expr}`");
    }
    // A changed key is set in place, a dropped one deleted, a new one added;
    // the tables and their comments keep their bytes.
    assert_eq!(
        edit(SRC, r#". |= {top: 2, t: .t, bin: .bin, new: "n"}"#),
        "top = 2\nnew = \"n\"\n\n# the t table\n[t]\na = 1 # one\nb = \"x\"\n\n[t.sub]\nc = [1, 2]\n\n[[bin]]\nname = \"a\"\n"
    );
    assert_eq!(
        edit(SRC, ". |= del(.t)"),
        "top = 1\n\n[[bin]]\nname = \"a\"\n"
    );
}

#[test]
fn the_document_is_never_set_to_a_non_table() {
    let mut doc = parse("a = 1\n").unwrap();
    let err = doc.apply(&parse_expr(". = 5").unwrap()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "a TOML document is a table, so `.` can only be set to an object (got number)"
    );
    assert_eq!(doc.to_source(), "a = 1\n");
}
