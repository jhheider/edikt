use super::*;
use crate::{Document, apply, parse, parse_expr};

fn edit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    apply(&mut doc, &parse_expr(expr).unwrap()).unwrap();
    doc.to_source()
}

fn edit_err(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    let err = apply(&mut doc, &parse_expr(expr).unwrap())
        .unwrap_err()
        .to_string();
    // A refused edit leaves the document as it was.
    assert_eq!(doc.to_source(), src, "after refusing `{expr}`");
    err
}

/// Each file spells one key two ways; `toml_edit` alone writes one spelling
/// for both, so an edit elsewhere used to rewrite an untouched line.
const SHARED: &[&str] = &[
    // The issue's case: an implicit parent under two headers.
    "[\"pkg\".a]\nk = 1\n\n[pkg.b]\nk = 1\n",
    // An explicit parent's own header, then a child's.
    "[pkg]\nx = 1\n[\"pkg\".a]\nk = 1\n",
    // Spacing around the dots is the shared key's too.
    "[pkg . a]\nk = 1\n[pkg.b]\nk = 1\n",
    // Array-of-tables elements share their key and its spacing.
    "[[ bin ]]\nn = 1\n[[bin]]\nn = 2\n",
    // Dotted keys share their prefix table.
    "\"a\".b = 1\na.c = 2\n",
    // ...with a header under that dotted table.
    "[fruit]\n\"apple\".color = 1\n[fruit.apple.texture]\ns = 1\n",
    // ...and inside an inline table, in an array too.
    "t = { \"a\".b = 1, a . c = 2 }\n",
    "arr = [{ \"a\".b = 1, a.c = 2 }, { a.b = 3, a.c = 4 }]\n",
    // Quoted keys holding dots, quotes and escapes.
    "[\"a.b\".x]\nk = 1\n['a.b'.y]\nk = 1\n[\"q\\\"\".x]\nk = 1\n['q\"'.y]\nk = 1\n",
];

#[test]
fn an_edit_elsewhere_keeps_every_spelling() {
    for src in SHARED {
        let src = format!("z = 0\n{src}");
        assert!(
            !respelled(&src).is_empty(),
            "toml_edit no longer respells, so this case tests nothing:\n{src}"
        );
        assert_eq!(edit(&src, ".z = 1"), src.replacen("z = 0", "z = 1", 1));
        assert_eq!(edit(&src, ". |= ."), src);
    }
}

#[test]
fn the_edited_line_keeps_its_own_spelling() {
    let src = "[\"pkg\".a]\nk = 1\n\n[pkg.b]\nk = 1\n";
    assert_eq!(
        edit(src, ".pkg.a.k = 2"),
        "[\"pkg\".a]\nk = 2\n\n[pkg.b]\nk = 1\n"
    );
    assert_eq!(
        edit(src, ".pkg.b.k = 2"),
        "[\"pkg\".a]\nk = 1\n\n[pkg.b]\nk = 2\n"
    );
    assert_eq!(edit(src, "del(.pkg.a)"), "\n[pkg.b]\nk = 1\n");
    let dotted = "\"a\".b = 1\na.c = 2\n";
    assert_eq!(edit(dotted, ".a.c = 3"), "\"a\".b = 1\na.c = 3\n");
    assert_eq!(edit(dotted, "del(.a.b)"), "a.c = 2\n");
    let bins = "[[ bin ]]\nn = 1\n[[bin]]\nn = 2\n[[bin]]\nn = 3\n";
    assert_eq!(
        edit(bins, "del(.bin[0])"),
        "[[bin]]\nn = 2\n[[bin]]\nn = 3\n"
    );
    assert_eq!(
        edit(bins, ".bin[1].n = 9"),
        "[[ bin ]]\nn = 1\n[[bin]]\nn = 9\n[[bin]]\nn = 3\n"
    );
}

#[test]
fn line_endings_survive_the_restore() {
    // Mixed endings: untouched lines keep their own, the edited line takes
    // the dominant one (CRLF), and the missing final newline stays missing.
    let src = "[\"pkg\".a]\r\nk = 1\n\r\n[pkg.b]\r\nk = 1";
    assert_eq!(
        edit(src, ".pkg.b.k = 2"),
        "[\"pkg\".a]\r\nk = 1\n\r\n[pkg.b]\r\nk = 2"
    );
}

#[test]
fn an_array_element_keeps_its_spelling_while_it_holds_its_place() {
    let src = "arr = [{ \"a\".b = 1, a.c = 2 }, { a.b = 3, a.c = 4 }]\n";
    // Appending shifts nothing.
    assert_eq!(
        edit(src, ".arr += [5]"),
        "arr = [{ \"a\".b = 1, a.c = 2 }, { a.b = 3, a.c = 4 }, 5]\n"
    );
    // Removing one could: refused, rather than respelled.
    let err = edit_err(src, "del(.arr[0])");
    assert!(
        err.starts_with("cannot keep the spelling of `a.c` in an array this edit changes"),
        "{err}"
    );
    // So is changing the element that spells it two ways.
    assert!(edit_err(src, ".arr[0].b = 5").starts_with("cannot keep the spelling"));
}

#[test]
fn comment_writes_keep_the_spellings() {
    let src = "[\"pkg\".a]\nk = 1\n\n[pkg.b]\nk = 1\n";
    let mut doc = parse(src).unwrap();
    doc.set_comment(
        &[
            edikt_core::Step::Field("pkg".into()),
            edikt_core::Step::Field("b".into()),
        ],
        edikt_core::CommentKind::Head,
        "b",
    )
    .unwrap();
    assert_eq!(
        doc.to_source(),
        "[\"pkg\".a]\nk = 1\n\n# b\n[pkg.b]\nk = 1\n"
    );
}

#[test]
fn a_structural_get_slices_such_a_file() {
    // The unedited document now renders as its source, so a get slices it
    // (#103) instead of re-emitting it with a synthesized `[pkg]`.
    use edikt_core::Step::Field;
    let src = "[\"pkg\".a]\nk = 1\n\n[pkg.b]\nk = 1\n";
    let doc = parse(src).unwrap();
    assert_eq!(doc.to_source(), src);
    assert_eq!(doc.source_slice(&[]), vec![src.trim_end().to_string()]);
    assert_eq!(
        doc.source_slice(&[Field("pkg".into())]),
        vec!["[a]\nk = 1\n\n[b]\nk = 1".to_string()]
    );
}

#[test]
fn a_file_spelling_each_key_one_way_has_nothing_to_keep() {
    for src in [
        "",
        "a = 1\n",
        "[pkg.a]\nk = 1\n[pkg.b]\nk = 1\n",
        "a.b = 1\na.c = 2\n[[bin]]\n[[bin]]\n",
        "t = { a.b = 1, a.c = 2 }\r\n",
    ] {
        assert!(respelled(src).is_empty(), "{src:?}");
    }
}

#[test]
fn key_paths_are_found_backwards_from_their_last_key() {
    // `|` marks where the last key starts; the result is what comes before
    // it on the key path.
    fn at(marked: &str, n: usize) -> Option<String> {
        let leaf = marked.find('|').unwrap();
        let src = marked.replace('|', "");
        key_path_start(src.as_bytes(), leaf, n).map(|i| src[i..leaf].to_string())
    }
    let some = |s: &str| Some(s.to_string());
    assert_eq!(at("x = 1\na . |b", 2), some("a . "));
    assert_eq!(at("\"a.b\".|c", 2), some("\"a.b\"."));
    assert_eq!(at("'a\"b'.|c", 2), some("'a\"b'."));
    assert_eq!(at("\"q\\\"\".|c", 2), some("\"q\\\"\"."));
    assert_eq!(at("x = \"q\\\\\".|c", 2), some("\"q\\\\\"."));
    assert_eq!(at("x.y.|z", 3), some("x.y."));
    assert_eq!(at("{ |c", 2), None, "no dot before the last key");
}
