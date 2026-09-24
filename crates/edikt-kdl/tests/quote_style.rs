//! Assigning a string in place keeps the replaced string's spelling: quoted
//! `"..."`, raw `#"..."#`, or a bare identifier string (jhheider/edikt#81),
//! falling back to quoted only when the new value can't be spelled that way.

use edikt_kdl::{Document, Value, parse, parse_expr};

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
        // Each spelling keeps itself.
        ("a \"old\"\n", ".a", s("new"), "a \"new\"\n"),
        ("a old\n", ".a", s("new"), "a new\n"),
        ("a #\"old\"#\n", ".a", s("new"), "a #\"new\"#\n"),
        ("a ##\"old\"##\n", ".a", s("new"), "a ##\"new\"##\n"),
        // Raw strings hold quotes and backslashes verbatim, adding a `#` only
        // when the value contains the closing delimiter.
        (
            "a #\"old\"#\n",
            ".a",
            s("C:\\ \"x\""),
            "a #\"C:\\ \"x\"\"#\n",
        ),
        ("a #\"old\"#\n", ".a", s("x\"#y"), "a ##\"x\"#y\"##\n"),
        // A line break or a disallowed code point can't go in a one-line raw
        // string: quoted, escaped.
        ("a #\"old\"#\n", ".a", s("x\ny"), "a \"x\\ny\"\n"),
        ("a #\"old\"#\n", ".a", s("x\u{1}"), "a \"x\\u{1}\"\n"),
        // An identifier string stays bare only while the value is a valid
        // identifier; otherwise it is quoted.
        ("a old\n", ".a", s("two words"), "a \"two words\"\n"),
        ("a old\n", ".a", s("true"), "a \"true\"\n"),
        ("a old\n", ".a", s("5"), "a \"5\"\n"),
        ("a old\n", ".a", s(""), "a \"\"\n"),
        // A quoted string stays quoted even when the value could be bare, and
        // escapes what it must.
        (
            "a \"old\"\n",
            ".a",
            s("say \"hi\" \\"),
            "a \"say \\\"hi\\\" \\\\\"\n",
        ),
        (
            "a \"old\"\n",
            ".a",
            s("t\tn\n\u{7f}\u{feff}"),
            "a \"t\\tn\\n\\u{7f}\\u{feff}\"\n",
        ),
        ("a \"old\"\n", ".a", s("café"), "a \"café\"\n"),
        // A multi-line string has no one-line form to keep: quoted.
        ("a \"\"\"\n  old\n  \"\"\"\n", ".a", s("new"), "a \"new\"\n"),
        // Properties and the other arguments of a row.
        ("d x=\"q\" y=bare\n", ".d.x", s("n"), "d x=\"n\" y=bare\n"),
        ("d x=\"q\" y=bare\n", ".d.y", s("n"), "d x=\"q\" y=n\n"),
        (
            "d x=\"q\" y=bare\n",
            ".d.y",
            s("n m"),
            "d x=\"q\" y=\"n m\"\n",
        ),
        ("a \"x\" y\n", ".a[1]", s("n"), "a \"x\" n\n"),
        ("a \"x\" y\n", ".a[0]", s("n"), "a \"n\" y\n"),
        // Only strings carry a quote style.
        ("a \"old\"\n", ".a", Value::Int(5), "a 5\n"),
        ("a #\"old\"#\n", ".a", Value::Bool(true), "a #true\n"),
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
