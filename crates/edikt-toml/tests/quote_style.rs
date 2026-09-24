//! Assigning a string in place keeps the replaced string's quote style
//! (basic `"`, literal `'`, and their multi-line `"""`/`'''` forms), falling
//! back only when the new value can't be spelled in it (jhheider/edikt#81).

use edikt_toml::{Document, Value, parse, parse_expr};

fn edit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    doc.apply(&parse_expr(expr).unwrap()).unwrap();
    doc.to_source()
}

fn read(src: &str, path: &str) -> Vec<Value> {
    edikt_core::eval(&parse_expr(path).unwrap(), &parse(src).unwrap().to_value()).unwrap()
}

#[test]
fn set_keeps_or_falls_back_from_the_old_style() {
    let s = |x: &str| Value::Str(x.into());
    let cases: &[(&str, &str, Value, &str)] = &[
        // Each style keeps itself.
        ("a = 'old'\n", ".a", s("new"), "a = 'new'\n"),
        ("a = \"old\"\n", ".a", s("new"), "a = \"new\"\n"),
        ("a = '''old'''\n", ".a", s("new"), "a = '''new'''\n"),
        (
            "a = \"\"\"old\"\"\"\n",
            ".a",
            s("new"),
            "a = \"\"\"new\"\"\"\n",
        ),
        // A basic string escapes rather than switching to a literal.
        (
            "a = \"old\"\n",
            ".a",
            s("say \"hi\" \\ bye"),
            "a = \"say \\\"hi\\\" \\\\ bye\"\n",
        ),
        (
            "a = \"old\"\n",
            ".a",
            s("x\ny\u{1}"),
            "a = \"x\\ny\\u0001\"\n",
        ),
        ("a = \"old\"\n", ".a", s("café"), "a = \"café\"\n"),
        // A literal holds backslashes and double quotes as they are.
        (
            "a = 'old'\n",
            ".a",
            s("C:\\dir \"x\""),
            "a = 'C:\\dir \"x\"'\n",
        ),
        // ... but not `'`, a line break, or a control character: basic then.
        ("a = 'old'\n", ".a", s("it's"), "a = \"it's\"\n"),
        (
            "a = 'old'\n",
            ".a",
            s("two\nlines"),
            "a = \"two\\nlines\"\n",
        ),
        ("a = 'old'\n", ".a", s("del\u{7f}"), "a = \"del\\u007F\"\n"),
        // Multi-line literal holds line breaks; `'''`, a trailing `'`, or a
        // leading newline (which TOML trims) fall back to multi-line basic.
        ("a = '''old'''\n", ".a", s("x\ny"), "a = '''x\ny'''\n"),
        (
            "a = '''old'''\n",
            ".a",
            s("a'''b"),
            "a = \"\"\"a'''b\"\"\"\n",
        ),
        (
            "a = '''old'''\n",
            ".a",
            s("\nlead"),
            "a = \"\"\"\\nlead\"\"\"\n",
        ),
        // Multi-line basic keeps raw line breaks and escapes the rest.
        (
            "a = \"\"\"old\"\"\"\n",
            ".a",
            s("x\n\"y\""),
            "a = \"\"\"x\n\\\"y\\\"\"\"\"\n",
        ),
        // Only strings carry a quote style.
        ("a = 'old'\n", ".a", Value::Int(5), "a = 5\n"),
        ("a = \"old\"\n", ".a", Value::Bool(true), "a = true\n"),
        // An inline comment and spacing survive.
        (
            "a = 'old'   # keep\n",
            ".a",
            s("new"),
            "a = 'new'   # keep\n",
        ),
        // Inline tables and array elements too, and an element keeps its
        // own spacing.
        (
            "t = { x = 'y', z = \"w\" }\n",
            ".t.x",
            s("n"),
            "t = { x = 'n', z = \"w\" }\n",
        ),
        ("a = [ 'x', 'y' ]\n", ".a[1]", s("n"), "a = [ 'x', 'n' ]\n"),
        ("a = [ \"x\", 1 ]\n", ".a[0]", s("n"), "a = [ \"n\", 1 ]\n"),
        // A table key under a header.
        (
            "[pkg]\nname = 'old' # n\n",
            ".pkg.name",
            s("new"),
            "[pkg]\nname = 'new' # n\n",
        ),
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
    assert_eq!(edit("a = 'v1'\n", ".a |= . + \"x\""), "a = 'v1x'\n");
}
