//! Assigning a scalar in place keeps the replaced scalar's quote style
//! (jhheider/edikt#81), falling back to another style only when the new value
//! cannot be spelled in it.

use edikt_yaml::{Document, Value, parse, parse_expr};

/// Apply `expr` to `src` and return the new source.
fn edit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    doc.apply(&parse_expr(expr).unwrap()).unwrap();
    doc.to_source()
}

/// What `path` reads as, in every document of `src`.
fn read(src: &str, path: &str) -> Vec<Value> {
    let expr = parse_expr(path).unwrap();
    parse(src)
        .unwrap()
        .to_values()
        .iter()
        .flat_map(|v| edikt_core::eval(&expr, v).unwrap())
        .collect()
}

#[test]
fn issue_81_double_quoted_scalar_stays_double_quoted() {
    let src = "name: \"old\"\nother: \"keep\"\n";
    assert_eq!(
        edit(src, ".name = \"new\""),
        "name: \"new\"\nother: \"keep\"\n"
    );
}

/// `(source, path, new value, expected source)`: set `path` to the value and
/// expect exactly these bytes. Each expected source must also read the value
/// back, so a case can't pass by spelling a different value.
#[test]
fn set_keeps_or_falls_back_from_the_old_style() {
    let s = |x: &str| Value::Str(x.into());
    let cases: &[(&str, &str, Value, &str)] = &[
        // Each style keeps itself.
        ("a: \"old\"\n", ".a", s("new"), "a: \"new\"\n"),
        ("a: 'old'\n", ".a", s("new"), "a: 'new'\n"),
        ("a: old\n", ".a", s("new"), "a: new\n"),
        // A quoted style holds what plain can't, so a string that looks like
        // another type stays in it rather than being re-quoted.
        ("a: 'old'\n", ".a", s("yes"), "a: 'yes'\n"),
        ("a: \"old\"\n", ".a", s("1.10"), "a: \"1.10\"\n"),
        ("a: 'old'\n", ".a", s("a: b # c"), "a: 'a: b # c'\n"),
        // Single quotes escape only `'`, by doubling it.
        ("a: 'old'\n", ".a", s("it's"), "a: 'it''s'\n"),
        // Anything else single quotes can't spell falls back to double.
        ("a: 'old'\n", ".a", s("two\nlines"), "a: \"two\\nlines\"\n"),
        ("a: 'old'\n", ".a", s("tab\there"), "a: \"tab\\there\"\n"),
        ("a: 'old'\n", ".a", s("bell\u{7}"), "a: \"bell\\x07\"\n"),
        // Double-quoted output escapes what it must.
        (
            "a: \"old\"\n",
            ".a",
            s("say \"hi\" \\ bye"),
            "a: \"say \\\"hi\\\" \\\\ bye\"\n",
        ),
        ("a: \"old\"\n", ".a", s("x\ny\r\n"), "a: \"x\\ny\\r\\n\"\n"),
        (
            "a: \"old\"\n",
            ".a",
            s("\u{1}\u{7f}\u{85}\u{feff}\u{2028}"),
            "a: \"\\x01\\x7f\\N\\ufeff\\L\"\n",
        ),
        // Printable non-ASCII is not escaped.
        ("a: \"old\"\n", ".a", s("café ☕"), "a: \"café ☕\"\n"),
        ("a: 'old'\n", ".a", s("café"), "a: 'café'\n"),
        // A plain scalar that would change type or meaning is quoted.
        ("a: old\n", ".a", s("yes"), "a: \"yes\"\n"),
        ("a: old\n", ".a", s("Off"), "a: \"Off\"\n"),
        ("a: old\n", ".a", s("1.10"), "a: \"1.10\"\n"),
        ("a: old\n", ".a", s("42"), "a: \"42\"\n"),
        ("a: old\n", ".a", s("0x1F"), "a: \"0x1F\"\n"),
        ("a: old\n", ".a", s("1_000"), "a: \"1_000\"\n"),
        ("a: old\n", ".a", s("12:30"), "a: \"12:30\"\n"),
        ("a: old\n", ".a", s("2001-12-14"), "a: \"2001-12-14\"\n"),
        ("a: old\n", ".a", s("null"), "a: \"null\"\n"),
        ("a: old\n", ".a", s("~"), "a: \"~\"\n"),
        ("a: old\n", ".a", s("true"), "a: \"true\"\n"),
        ("a: old\n", ".a", s(""), "a: \"\"\n"),
        ("a: old\n", ".a", s("@home"), "a: \"@home\"\n"),
        ("a: old\n", ".a", s("*ref"), "a: \"*ref\"\n"),
        ("a: old\n", ".a", s("- item"), "a: \"- item\"\n"),
        ("a: old\n", ".a", s("k: v"), "a: \"k: v\"\n"),
        ("a: old\n", ".a", s("x #y"), "a: \"x #y\"\n"),
        ("a: old\n", ".a", s(" pad"), "a: \" pad\"\n"),
        ("a: old\n", ".a", s("multi\nline"), "a: \"multi\\nline\"\n"),
        // ... but plain stays plain where it reads the same.
        ("a: old\n", ".a", s("nginx:1.25"), "a: nginx:1.25\n"),
        ("a: old\n", ".a", s("has#hash"), "a: has#hash\n"),
        ("a: old\n", ".a", s("it's"), "a: it's\n"),
        // A file already relying on a YAML 1.1 word of the same kind keeps it
        // plain: flipping `yes` to `no` must not turn a 1.1 bool into a string.
        ("a: yes\n", ".a", s("no"), "a: no\n"),
        ("a: 2001-12-14\n", ".a", s("2026-09-24"), "a: 2026-09-24\n"),
        ("a: yes\n", ".a", s("2026-09-24"), "a: \"2026-09-24\"\n"),
        // Only strings carry a quote style: quoting a number, bool, or null
        // would change its type.
        ("a: \"old\"\n", ".a", Value::Int(5), "a: 5\n"),
        ("a: 'old'\n", ".a", Value::Bool(true), "a: true\n"),
        ("a: \"old\"\n", ".a", Value::Null, "a: null\n"),
        ("a: \"old\"\n", ".a", Value::Float(1.5), "a: 1.5\n"),
        // A number replaced by a string of digits is quoted, as before.
        ("a: 8080\n", ".a", s("9090"), "a: \"9090\"\n"),
        // Block sequence items.
        (
            "- \"a\"\n- 'b'\n- c\n",
            ".[0]",
            s("x"),
            "- \"x\"\n- 'b'\n- c\n",
        ),
        (
            "- \"a\"\n- 'b'\n- c\n",
            ".[1]",
            s("x"),
            "- \"a\"\n- 'x'\n- c\n",
        ),
        // Flow mapping and sequence: a plain scalar there also can't hold
        // `,[]{}`, which would end it early.
        (
            "m: {k: \"v\", p: w, s: 'q'}\n",
            ".m.k",
            s("n"),
            "m: {k: \"n\", p: w, s: 'q'}\n",
        ),
        (
            "m: {k: \"v\", p: w, s: 'q'}\n",
            ".m.s",
            s("n"),
            "m: {k: \"v\", p: w, s: 'n'}\n",
        ),
        (
            "m: {k: \"v\", p: w, s: 'q'}\n",
            ".m.p",
            s("x,y"),
            "m: {k: \"v\", p: \"x,y\", s: 'q'}\n",
        ),
        (
            "m: {k: \"v\", p: w, s: 'q'}\n",
            ".m.p",
            s("nginx:1.25"),
            "m: {k: \"v\", p: nginx:1.25, s: 'q'}\n",
        ),
        ("l: [a, 'b']\n", ".l[1]", s("c"), "l: [a, 'c']\n"),
        ("l: [a, 'b']\n", ".l[0]", s("a]b"), "l: [\"a]b\", 'b']\n"),
        ("l: [a, 'b']\n", ".l[0]", s("{x}"), "l: [\"{x}\", 'b']\n"),
        // The same `,` is fine plain in block context.
        ("a: old\n", ".a", s("x,y"), "a: x,y\n"),
        // An anchor survives (an alias elsewhere names it), and so does a tag
        // that still fits the value.
        (
            "a: &x \"old\"\nb: *x\n",
            ".a",
            s("new"),
            "a: &x \"new\"\nb: *x\n",
        ),
        ("a: &x old\n", ".a", s("yes"), "a: &x \"yes\"\n"),
        ("a: !!str \"5\"\n", ".a", s("6"), "a: !!str \"6\"\n"),
        ("r: !Ref 'Bucket'\n", ".r", s("Other"), "r: !Ref 'Other'\n"),
        ("a: &x !!str 5\n", ".a", s("6"), "a: &x !!str \"6\"\n"),
        // A core tag that no longer fits the value goes; the anchor stays.
        ("a: &x !!str \"5\"\n", ".a", Value::Int(6), "a: &x 6\n"),
        // Replacing an alias writes the value, with no style to inherit.
        (
            "a: &x \"old\"\nb: *x\n",
            ".b",
            s("z"),
            "a: &x \"old\"\nb: z\n",
        ),
        // An inline comment after the scalar is untouched.
        ("a: 'old'   # keep\n", ".a", s("new"), "a: 'new'   # keep\n"),
    ];
    for (src, path, value, want) in cases {
        let expr = format!("{path} = {}", value.to_json());
        let got = edit(src, &expr);
        assert_eq!(&got, want, "{src:?} with `{expr}`");
        assert_eq!(
            read(&got, path),
            vec![value.clone()],
            "{got:?} must read `{path}` back as {value:?}"
        );
    }
}

#[test]
fn update_assign_keeps_the_style() {
    assert_eq!(edit("a: 'v1'\n", ".a |= . + \"x\""), "a: 'v1x'\n");
    assert_eq!(edit("a: \"v1\"\n", ".a += \"x\""), "a: \"v1x\"\n");
}

#[test]
fn a_new_key_quotes_what_plain_would_misread() {
    // No old scalar, so no style to keep: plain unless that misreads.
    assert_eq!(edit("a: 1\n", ".b = \"yes\""), "a: 1\nb: \"yes\"\n");
    assert_eq!(edit("a: 1\n", ".b = \"ok\""), "a: 1\nb: ok\n");
}

#[test]
fn multi_document_edits_keep_each_documents_style() {
    let src = "---\na: \"x\"\n---\na: 'y'\n---\na: z\n";
    // Mapped over every document, each keeps its own style.
    assert_eq!(
        edit(src, ".a = \"new\""),
        "---\na: \"new\"\n---\na: 'new'\n---\na: new\n"
    );
    // `^dN` edits one document, in that document's style.
    assert_eq!(
        edit(src, "^d1 | .a = \"new\""),
        "---\na: \"x\"\n---\na: 'new'\n---\na: z\n"
    );
    assert_eq!(
        edit(src, "^d2 | .a = \"true\""),
        "---\na: \"x\"\n---\na: 'y'\n---\na: \"true\"\n"
    );
}
