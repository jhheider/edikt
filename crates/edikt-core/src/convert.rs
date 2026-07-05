//! Data-model helpers for format conversion.
//!
//! Conversion projects a source document to [`Value`] and re-emits it in another
//! format. That drops trivia by nature (it goes through the data model, not the
//! CST), and formats with less structure than the source must flatten. These
//! helpers are the shared machinery; the per-format emitters live in the format
//! crates.

use crate::Value;

/// Render a value as pretty (2-space) JSON with a trailing newline.
pub fn to_pretty_json(value: &Value) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out.push('\n');
    out
}

fn write_pretty(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Object(m) if m.is_empty() => out.push_str("{}"),
        Value::Array(a) if a.is_empty() => out.push_str("[]"),
        Value::Object(m) => {
            out.push_str("{\n");
            for (i, (k, v)) in m.iter().enumerate() {
                indent_by(indent + 1, out);
                out.push_str(&Value::Str(k.clone()).to_json());
                out.push_str(": ");
                write_pretty(v, indent + 1, out);
                if i + 1 < m.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent_by(indent, out);
            out.push('}');
        }
        Value::Array(a) => {
            out.push_str("[\n");
            for (i, v) in a.iter().enumerate() {
                indent_by(indent + 1, out);
                write_pretty(v, indent + 1, out);
                if i + 1 < a.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent_by(indent, out);
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_json()),
    }
}

fn indent_by(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

/// Flatten a value to `(dotted.key, string)` pairs (`{a:{b:1}}` → `a.b = "1"`;
/// arrays index: `{a:[1,2]}` → `a.0`, `a.1`). Used by the flat emitters.
pub fn flatten(value: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk("", value, &mut out);
    out
}

fn walk(prefix: &str, value: &Value, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(m) => {
            for (k, v) in m {
                walk(&join_key(prefix, k), v, out);
            }
        }
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                walk(&join_key(prefix, &i.to_string()), v, out);
            }
        }
        scalar => out.push((prefix.to_string(), scalar_string(scalar))),
    }
}

fn join_key(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// The string form of a scalar (`Str` without quotes; numbers/bools/null as text).
pub fn scalar_string(value: &Value) -> String {
    value.to_raw_string()
}

/// Does `value` contain nesting (an object/array inside an object/array)?
pub fn has_nesting(value: &Value) -> bool {
    match value {
        Value::Object(m) => m.iter().any(|(_, v)| is_container(v)),
        Value::Array(a) => a.iter().any(is_container),
        _ => false,
    }
}

/// Does `value` contain an array anywhere?
pub fn has_array(value: &Value) -> bool {
    match value {
        Value::Array(_) => true,
        Value::Object(m) => m.iter().any(|(_, v)| has_array(v)),
        _ => false,
    }
}

fn is_container(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Array(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_to_dotted_keys() {
        let v = Value::Object(vec![
            ("a".into(), Value::Object(vec![("b".into(), Value::Int(1))])),
            (
                "c".into(),
                Value::Array(vec![Value::Str("x".into()), Value::Str("y".into())]),
            ),
        ]);
        assert_eq!(
            flatten(&v),
            vec![
                ("a.b".to_string(), "1".to_string()),
                ("c.0".to_string(), "x".to_string()),
                ("c.1".to_string(), "y".to_string()),
            ]
        );
    }

    #[test]
    fn pretty_json_indents() {
        let v = Value::Object(vec![(
            "a".into(),
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
        )]);
        assert_eq!(to_pretty_json(&v), "{\n  \"a\": [\n    1,\n    2\n  ]\n}\n");
        assert_eq!(to_pretty_json(&Value::Object(vec![])), "{}\n");
    }

    #[test]
    fn feature_detection() {
        let nested = Value::Object(vec![("a".into(), Value::Object(vec![]))]);
        assert!(has_nesting(&nested));
        assert!(!has_nesting(&Value::Object(vec![(
            "a".into(),
            Value::Int(1)
        )])));
        assert!(has_array(&Value::Object(vec![(
            "a".into(),
            Value::Array(vec![])
        )])));
    }
}
