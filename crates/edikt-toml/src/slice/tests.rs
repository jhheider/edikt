use crate::{Document, Expr, Step, Value, parse, parse_expr};
use edikt_core::eval;

/// The source slices a pure-path query gets, checked against the evaluator:
/// one per result, and every structural one re-parses as a TOML document to
/// exactly the value the query selected.
fn slices(src: &str, expr: &str) -> Vec<String> {
    slices_at(src, parse_expr(expr).unwrap().as_path().unwrap())
}

fn slices_at(src: &str, path: &[Step]) -> Vec<String> {
    let doc = parse(src).unwrap();
    let got = doc.source_slice(path);
    if got.is_empty() {
        return got;
    }
    let results = eval(&Expr::Path(path.to_vec()), &doc.to_value()).unwrap();
    assert_eq!(got.len(), results.len(), "slices misaligned: {got:?}");
    for (slice, want) in got.iter().zip(&results) {
        if matches!(want, Value::Object(_)) {
            let reparsed = parse(slice).unwrap_or_else(|e| panic!("{slice:?}: {e}"));
            assert_eq!(&reparsed.to_value(), want, "slice: {slice:?}");
        }
    }
    got
}

fn one(src: &str, expr: &str) -> String {
    let mut s = slices(src, expr);
    assert_eq!(s.len(), 1, "{expr}: {s:?}");
    s.remove(0)
}

const CARGO: &str = "[package]\nname = \"x\"\nedition.workspace = true\nauthors.workspace = true\n";

#[test]
fn dotted_keys_come_back_as_written() {
    // jhheider/edikt#101: the emitter turned each dotted key into a `[table]`.
    assert_eq!(
        one(CARGO, ".package"),
        "name = \"x\"\nedition.workspace = true\nauthors.workspace = true"
    );
}

const MIXED: &str = "\
# top
[package]   # the package
name = \"x\" # the name
edition.workspace = true
meta = { a = 1, b = [1, 2] }
list = [
  1, # one
  2,
]

# sub comment
[package.metadata.docs]
all = true

[[package.bin]]
name = \"a\"

[[package.bin]]
name = \"b\"
[package.bin.extra]
k = 1

[other]
x = 1
# trailing
";

#[test]
fn sub_tables_are_re_rooted_and_everything_else_is_verbatim() {
    assert_eq!(
        one(MIXED, ".package"),
        "\
name = \"x\" # the name
edition.workspace = true
meta = { a = 1, b = [1, 2] }
list = [
  1, # one
  2,
]

# sub comment
[metadata.docs]
all = true

[[bin]]
name = \"a\"

[[bin]]
name = \"b\"
[bin.extra]
k = 1"
    );
    // An implicit table has no body; its sub-tables carry their comments.
    assert_eq!(
        one(MIXED, ".package.metadata"),
        "# sub comment\n[docs]\nall = true"
    );
    assert_eq!(one(MIXED, ".package.metadata.docs"), "all = true");
    // The last section runs to the end of the file, trailing comment included.
    assert_eq!(one(MIXED, ".other"), "x = 1\n# trailing");
    // The whole document is the whole file.
    assert_eq!(one(MIXED, "."), MIXED.trim_end());
}

#[test]
fn array_of_tables_elements_slice_with_their_sub_tables() {
    assert_eq!(one(MIXED, ".package.bin[0]"), "name = \"a\"");
    assert_eq!(
        one(MIXED, ".package.bin[1]"),
        "name = \"b\"\n[extra]\nk = 1"
    );
    assert_eq!(
        one(MIXED, ".package.bin[-1]"),
        "name = \"b\"\n[extra]\nk = 1"
    );
    assert_eq!(
        slices(MIXED, ".package.bin[]"),
        ["name = \"a\"", "name = \"b\"\n[extra]\nk = 1"]
    );
    // Nested arrays of tables re-root to `[[...]]` headers of their own.
    let src = "[[a]]\nn = 1\n[[a.b]]\nm = 1\n[[a.b]]\nm = 2\n[[a]]\nn = 2\n";
    assert_eq!(one(src, ".a[0]"), "n = 1\n[[b]]\nm = 1\n[[b]]\nm = 2");
    assert_eq!(one(src, ".a[0].b[1]"), "m = 2");
    assert_eq!(one(src, ".a[1]"), "n = 2");
    assert!(slices(src, ".a[5]").is_empty());
}

#[test]
fn nested_and_interleaved_sub_tables() {
    // A descendant defined before its parent's own header, and an unrelated
    // table in between: the body comes first, then descendants in file order.
    let src = "[a.b.c]\nz = 3\n\n[other]\nq = 0\n\n[a]\nx = 1 # own\n\n[a.b]\ny = 2";
    // The blank line before `[a.b]` is that header's (where a head comment
    // would sit), so it travels with it.
    assert_eq!(one(src, ".a"), "x = 1 # own\n[b.c]\nz = 3\n\n[b]\ny = 2");
    assert_eq!(one(src, ".a.b"), "y = 2\n[c]\nz = 3");
    // The last section has no final newline; a later piece still starts on a
    // line of its own.
    let src = "[a.c]\nz = 3\n[a]\nx = 1";
    assert_eq!(one(src, ".a"), "x = 1\n[c]\nz = 3");
}

#[test]
fn header_spelling_survives_re_rooting() {
    // Spacing, quoting and a header's own comment are kept; a quoted key
    // matches by value.
    let src = "[ \"pkg\" . 'the x' . z ]  # note\nk = 1\n[[ \"pkg\" . list ]]\nn = 1\n";
    assert_eq!(
        one(src, ".pkg"),
        "[ 'the x' . z ]  # note\nk = 1\n[[ list ]]\nn = 1"
    );
    assert_eq!(one(src, ".pkg.\"the x\""), "[ z ]  # note\nk = 1");
}

#[test]
fn a_dotted_parent_keeps_its_dotted_lines() {
    let src = "[fruit]\napple.color = \"red\"\n\n[fruit.apple.texture]\nsmooth = true\n";
    assert_eq!(
        one(src, ".fruit"),
        "apple.color = \"red\"\n\n[apple.texture]\nsmooth = true"
    );
    assert_eq!(one(src, ".fruit.apple.texture"), "smooth = true");
}

#[test]
fn crlf_is_kept() {
    let src = "[a]\r\nx = 1 # c\r\n\r\n[a.b]\r\ny = [\r\n  1,\r\n]\r\n";
    assert_eq!(one(src, ".a"), "x = 1 # c\r\n\r\n[b]\r\ny = [\r\n  1,\r\n]");
    // A piece joined after a last line with no ending takes the file's CRLF.
    let src = "[a.c]\r\nz = 3\r\n[a]\r\nx = 1";
    assert_eq!(one(src, ".a"), "x = 1\r\n[c]\r\nz = 3");
}

#[test]
fn a_bom_is_not_part_of_any_slice() {
    let src = "\u{feff}[a]\nx = 1\n";
    assert_eq!(one(src, "."), "[a]\nx = 1");
    assert_eq!(one(src, ".a"), "x = 1");
}

#[test]
fn what_cannot_stand_alone_falls_back_to_emit() {
    // Empty means "emit instead": an inline table or array is a value, not a
    // document; a whole array of tables has no top-level form; a dotted
    // table's lines spell its key.
    for expr in [
        ".package.meta",
        ".package.list",
        ".package.meta.b",
        ".package.bin",
        ".package.edition",
        // One unsliceable result sends the whole query to the emitter.
        ".package[]",
    ] {
        assert!(slices(MIXED, expr).is_empty(), "{expr}");
    }
    // Scalars keep the result aligned (the caller renders them raw anyway).
    assert_eq!(slices(MIXED, ".package.name").len(), 1);
    assert_eq!(slices(MIXED, ".other[]").len(), 1);
    // A miss selects nothing.
    assert!(slices(MIXED, ".nope").is_empty());
}

#[test]
fn reroot_refuses_what_it_cannot_cut() {
    // Only a strict descendant whose leading keys spell the selection is
    // re-rooted; a malformed header is never guessed at.
    for (text, drop, want) in [
        ("[a.b]", &["a"][..], Some("[b]")),
        ("[[ a . \"b\" ]]", &["a"][..], Some("[[ \"b\" ]]")),
        ("[a]", &["a"][..], None),
        ("[x.b]", &["a"][..], None),
        ("[a.'b", &["a"][..], None),
        ("[a b]", &["a"][..], None),
    ] {
        assert_eq!(
            super::reroot(text, drop).map(|(h, _)| h).as_deref(),
            want,
            "{text}"
        );
    }
}

/// Every table path in `value` (array-of-tables elements included).
fn table_paths(value: &Value, path: &mut Vec<Step>, out: &mut Vec<Vec<Step>>) {
    match value {
        Value::Object(entries) => {
            out.push(path.clone());
            for (k, v) in entries {
                path.push(Step::Field(k.clone()));
                table_paths(v, path, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                path.push(Step::Index(i as i64));
                table_paths(v, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

#[test]
fn every_table_in_the_corpus_slices_to_its_own_value() {
    // The fixtures and this workspace's own manifests (dotted
    // `x.workspace = true` keys, `[dependencies]`, `[[bin]]`...): every table
    // either re-parses from its slice to exactly its value (`slices_at`
    // asserts it) or falls back.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = vec![root.join("Cargo.toml")];
    for dir in ["fixtures/toml", "crates"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap() {
            let p = entry.unwrap().path();
            files.push(if p.is_dir() { p.join("Cargo.toml") } else { p });
        }
    }
    let mut sliced = 0;
    for file in files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
    {
        let src = std::fs::read_to_string(file).unwrap();
        let mut paths = Vec::new();
        table_paths(
            &parse(&src).unwrap().to_value(),
            &mut Vec::new(),
            &mut paths,
        );
        for path in &paths {
            sliced += slices_at(&src, path).len();
        }
    }
    assert!(sliced > 50, "only {sliced} tables sliced");
}

#[test]
fn an_edited_document_is_not_sliced() {
    // After an edit the source no longer describes the tree.
    let mut doc = parse(CARGO).unwrap();
    let path = parse_expr(".package.name").unwrap();
    doc.set(path.as_path().unwrap(), &Value::Str("y".into()))
        .unwrap();
    let path = parse_expr(".package").unwrap();
    assert!(doc.source_slice(path.as_path().unwrap()).is_empty());
}
