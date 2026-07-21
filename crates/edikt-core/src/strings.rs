//! String builtins: the regex family (`test`, `match`, `capture`, `sub`,
//! `gsub`) plus `split` and `join`.
//!
//! jq-named and jq-shaped: `match` yields jq's match objects (`offset` /
//! `length` / `string` / `captures`, with codepoint offsets), flags are jq's
//! (`i`, `x`, plus `s`/`m`, and `g` for global), and no match means an empty
//! stream: a silent miss at the CLI (or exit 1 under `--exit-status`). The one
//! deliberate divergence: jq splices captures into replacements with string
//! interpolation, which edikt's expression language doesn't have; `sub`/`gsub`
//! use `$1` / `$name` references instead (sed-flavored, like the rest of the
//! execution model; `$$` is a literal `$`).

use crate::eval::EvalError;
use crate::value::Value;
use regex::{Regex, RegexBuilder};

/// Compile `re` with jq-style flags. `g` is not a regex option but a behavior
/// switch, so it comes back as a separate bool.
pub(crate) fn build(re: &str, flags: &str) -> Result<(Regex, bool), EvalError> {
    let mut b = RegexBuilder::new(re);
    let mut global = false;
    for f in flags.chars() {
        match f {
            'g' => global = true,
            'i' => {
                b.case_insensitive(true);
            }
            'x' => {
                b.ignore_whitespace(true);
            }
            's' => {
                b.dot_matches_new_line(true);
            }
            'm' => {
                b.multi_line(true);
            }
            other => {
                return Err(EvalError::new(format!(
                    "unknown regex flag `{other}` (expected g, i, x, s, m)"
                )));
            }
        }
    }
    let re = b
        .build()
        .map_err(|e| EvalError::new(format!("invalid regex: {e}")))?;
    Ok((re, global))
}

/// `test(re; flags)`: does the input match?
pub(crate) fn test(s: &str, re: &str, flags: &str) -> Result<Value, EvalError> {
    let (re, _) = build(re, flags)?;
    Ok(Value::Bool(re.is_match(s)))
}

/// `match(re; flags)`: jq match objects, one per match (all of them under
/// `g`, else just the first); no match is an empty stream.
pub(crate) fn find(s: &str, re: &str, flags: &str) -> Result<Vec<Value>, EvalError> {
    let (re, global) = build(re, flags)?;
    let mut out = Vec::new();
    for caps in re.captures_iter(s) {
        out.push(match_object(s, &re, &caps));
        if !global {
            break;
        }
    }
    Ok(out)
}

/// `capture(re; flags)`: an object of the named captures, one per match
/// (stream under `g`); unmatched named groups are null.
pub(crate) fn capture(s: &str, re: &str, flags: &str) -> Result<Vec<Value>, EvalError> {
    let (re, global) = build(re, flags)?;
    let mut out = Vec::new();
    for caps in re.captures_iter(s) {
        let pairs = re
            .capture_names()
            .flatten()
            .map(|name| {
                let v = caps
                    .name(name)
                    .map(|m| Value::Str(m.as_str().to_string()))
                    .unwrap_or(Value::Null);
                (name.to_string(), v)
            })
            .collect();
        out.push(Value::Object(pairs));
        if !global {
            break;
        }
    }
    Ok(out)
}

/// `sub(re; repl; flags)` / `gsub(re; repl)`: replace the first match (or
/// every match under `g`). `repl` may reference captures as `$1` / `$name`.
pub(crate) fn sub(s: &str, re: &str, repl: &str, flags: &str) -> Result<Value, EvalError> {
    let (re, global) = build(re, flags)?;
    let replaced = if global {
        re.replace_all(s, repl)
    } else {
        re.replace(s, repl)
    };
    Ok(Value::Str(replaced.into_owned()))
}

/// One jq match object: codepoint offsets, the matched string, and every
/// capture group (unmatched groups: offset -1, string null, jq's convention).
fn match_object(s: &str, re: &Regex, caps: &regex::Captures) -> Value {
    let whole = caps.get(0).expect("group 0 always participates");
    let names: Vec<Option<&str>> = re.capture_names().collect();
    let captures = (1..caps.len())
        .map(|i| {
            let name = names
                .get(i)
                .and_then(|n| n.map(|n| Value::Str(n.to_string())))
                .unwrap_or(Value::Null);
            match caps.get(i) {
                Some(m) => Value::Object(vec![
                    ("offset".into(), Value::Int(char_offset(s, m.start()))),
                    (
                        "length".into(),
                        Value::Int(m.as_str().chars().count() as i64),
                    ),
                    ("string".into(), Value::Str(m.as_str().to_string())),
                    ("name".into(), name),
                ]),
                None => Value::Object(vec![
                    ("offset".into(), Value::Int(-1)),
                    ("length".into(), Value::Int(0)),
                    ("string".into(), Value::Null),
                    ("name".into(), name),
                ]),
            }
        })
        .collect();
    Value::Object(vec![
        ("offset".into(), Value::Int(char_offset(s, whole.start()))),
        (
            "length".into(),
            Value::Int(whole.as_str().chars().count() as i64),
        ),
        ("string".into(), Value::Str(whole.as_str().to_string())),
        ("captures".into(), Value::Array(captures)),
    ])
}

/// jq offsets are codepoints, the regex crate's are bytes.
fn char_offset(s: &str, byte: usize) -> i64 {
    s[..byte].chars().count() as i64
}

/// `split(sep)`: literal separator (jq's 1-arg form); `split(re; flags)`,
/// regex separator (jq's 2-arg form).
pub(crate) fn split(s: &str, sep: &str, regex_flags: Option<&str>) -> Result<Value, EvalError> {
    let parts: Vec<Value> = match regex_flags {
        None => s.split(sep).map(|p| Value::Str(p.to_string())).collect(),
        Some(flags) => {
            let (re, _) = build(sep, flags)?;
            re.split(s).map(|p| Value::Str(p.to_string())).collect()
        }
    };
    Ok(Value::Array(parts))
}

/// `join(sep)`: stringify scalars, render null as empty, reject containers
/// (jq semantics).
pub(crate) fn join(items: &[Value], sep: &str) -> Result<Value, EvalError> {
    let mut parts = Vec::with_capacity(items.len());
    for v in items {
        match v {
            Value::Null => parts.push(String::new()),
            Value::Array(_) | Value::Object(_) => {
                return Err(EvalError::new(format!("cannot join a {}", v.type_name())));
            }
            scalar => parts.push(scalar.to_raw_string()),
        }
    }
    Ok(Value::Str(parts.join(sep)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_and_flags() {
        assert_eq!(test("Hello", "^h", "i").unwrap(), Value::Bool(true));
        assert_eq!(test("Hello", "^h", "").unwrap(), Value::Bool(false));
        assert!(build("(", "").is_err());
        assert!(build("a", "q").is_err());
    }

    #[test]
    fn match_objects_use_char_offsets() {
        let m = find("héllo world", "w(o)rld", "").unwrap();
        assert_eq!(m.len(), 1);
        let Value::Object(o) = &m[0] else { panic!() };
        assert_eq!(o[0], ("offset".into(), Value::Int(6))); // chars, not bytes
        assert_eq!(o[1], ("length".into(), Value::Int(5)));
        assert_eq!(o[2], ("string".into(), Value::Str("world".into())));
        let Value::Array(caps) = &o[3].1 else {
            panic!()
        };
        let Value::Object(c) = &caps[0] else { panic!() };
        assert_eq!(c[2], ("string".into(), Value::Str("o".into())));
    }

    #[test]
    fn global_match_streams() {
        assert_eq!(find("a1b2", r"\d", "").unwrap().len(), 1);
        assert_eq!(find("a1b2", r"\d", "g").unwrap().len(), 2);
        assert!(find("abc", r"\d", "g").unwrap().is_empty());
    }

    #[test]
    fn capture_named_groups() {
        let c = capture("v1.2", r"v(?<major>\d+)\.(?<minor>\d+)", "").unwrap();
        assert_eq!(
            c,
            vec![Value::Object(vec![
                ("major".into(), Value::Str("1".into())),
                ("minor".into(), Value::Str("2".into())),
            ])]
        );
    }

    #[test]
    fn sub_first_and_global_with_refs() {
        assert_eq!(
            sub("v1.2.3", r"^v", "", "").unwrap(),
            Value::Str("1.2.3".into())
        );
        assert_eq!(
            sub("a-b-c", "-", "_", "g").unwrap(),
            Value::Str("a_b_c".into())
        );
        assert_eq!(
            sub("port=80", r"(?<k>\w+)=(?<v>\w+)", "$v:$k", "").unwrap(),
            Value::Str("80:port".into())
        );
    }

    #[test]
    fn split_literal_and_regex() {
        assert_eq!(
            split("a:b:c", ":", None).unwrap(),
            Value::Array(vec![
                Value::Str("a".into()),
                Value::Str("b".into()),
                Value::Str("c".into()),
            ])
        );
        assert_eq!(
            split("a1b22c", r"\d+", Some("")).unwrap(),
            Value::Array(vec![
                Value::Str("a".into()),
                Value::Str("b".into()),
                Value::Str("c".into()),
            ])
        );
    }

    #[test]
    fn join_scalars() {
        let items = [
            Value::Str("a".into()),
            Value::Int(1),
            Value::Null,
            Value::Bool(true),
        ];
        assert_eq!(join(&items, "-").unwrap(), Value::Str("a-1--true".into()));
        assert!(join(&[Value::Array(vec![])], "-").is_err());
    }
}
