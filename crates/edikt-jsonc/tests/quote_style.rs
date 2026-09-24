//! Assigning a string in place keeps a JSON5 single-quoted string
//! single-quoted (jhheider/edikt#81). Double quotes are JSON's only other
//! spelling, and a single-quoted string can hold any value with escapes, so
//! nothing ever needs to fall back.

use edikt_jsonc::{Document, Value, parse, parse_expr};

fn edit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    doc.apply(&parse_expr(expr).unwrap()).unwrap();
    doc.to_source()
}

fn read(src: &str, path: &str) -> Vec<Value> {
    edikt_core::eval(&parse_expr(path).unwrap(), &parse(src).unwrap().to_value()).unwrap()
}

#[test]
fn set_keeps_the_old_quote_style() {
    let s = |x: &str| Value::Str(x.into());
    let src = "{\n  a: 'old', // keep\n  b: \"old\",\n  l: ['x', \"y\"],\n}\n";
    let with = |line: &str, to: &str| src.replacen(line, to, 1);
    let cases: &[(&str, Value, String)] = &[
        (".a", s("new"), with("'old'", "'new'")),
        (".b", s("new"), with("\"old\"", "\"new\"")),
        // Single quotes escape `'` and leave `"` bare; double quotes the reverse.
        (".a", s("it's"), with("'old'", "'it\\'s'")),
        (".a", s("say \"hi\""), with("'old'", "'say \"hi\"'")),
        (".b", s("it's"), with("\"old\"", "\"it's\"")),
        // Everything else escapes as in JSON.
        (
            ".a",
            s("x\ny\\z\t\u{1}"),
            with("'old'", "'x\\ny\\\\z\\t\\u0001'"),
        ),
        (".a", s("café"), with("'old'", "'café'")),
        // Array elements too.
        (".l[0]", s("n"), with("'x'", "'n'")),
        (".l[1]", s("n"), with("\"y\"", "\"n\"")),
        // Only strings carry a quote style.
        (".a", Value::Int(5), with("'old'", "5")),
        (".a", Value::Null, with("'old'", "null")),
    ];
    for (path, value, want) in cases {
        let expr = format!("{path} = {}", value.to_json());
        let got = edit(src, &expr);
        assert_eq!(&got, want, "`{expr}`");
        assert_eq!(read(&got, path), vec![value.clone()], "{got:?}");
    }
}

#[test]
fn update_assign_keeps_the_style() {
    assert_eq!(edit("{a: 'v1'}", ".a |= . + \"x\""), "{a: 'v1x'}");
}
