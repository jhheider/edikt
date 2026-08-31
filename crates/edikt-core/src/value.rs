//! The value calculus domain.
//!
//! `Value` is the in-memory model the expression evaluator and format conversion
//! operate over. It is *not* how format-preserving edits round-trip (those stay
//! in the CST); it is the domain for computed values (`.count + 1`), query
//! output, and cross-format conversion.
//!
//! Numbers keep an `Int`/`Float` distinction so integer output stays clean.
//! Objects are insertion-ordered (`Vec` of pairs) because key order is
//! user-visible.

use std::cmp::Ordering;

#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

// Equality follows the total order, so `Int(1) == Float(1.0)`. This lets `Expr`
// (which embeds `Value` in literals) derive `PartialEq` for tests.
impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        self.value_eq(other)
    }
}

impl Value {
    /// jq-style truthiness: only `false` and `null` are falsy.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Null | Value::Bool(false))
    }

    /// The type name, as reported by the `type` builtin.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Int(_) | Value::Float(_) => "number",
            Value::Str(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// Numeric coercion to f64 for arithmetic and comparison.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Total order across types (jq's ordering:
    /// null < bool < number < string < array < object).
    pub fn order(&self, other: &Value) -> Ordering {
        fn rank(v: &Value) -> u8 {
            match v {
                Value::Null => 0,
                Value::Bool(_) => 1,
                Value::Int(_) | Value::Float(_) => 2,
                Value::Str(_) => 3,
                Value::Array(_) => 4,
                Value::Object(_) => 5,
            }
        }
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Str(a), Value::Str(b)) => a.cmp(b),
            (Value::Array(a), Value::Array(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    let c = x.order(y);
                    if c != Ordering::Equal {
                        return c;
                    }
                }
                a.len().cmp(&b.len())
            }
            (Value::Object(a), Value::Object(b)) => {
                // Compare by sorted keys, then by the values at those keys.
                let mut ka: Vec<&String> = a.iter().map(|(k, _)| k).collect();
                let mut kb: Vec<&String> = b.iter().map(|(k, _)| k).collect();
                ka.sort();
                kb.sort();
                match ka.cmp(&kb) {
                    Ordering::Equal => {}
                    ne => return ne,
                }
                for k in ka {
                    let va = a.iter().find(|(kk, _)| kk == k).map(|(_, v)| v);
                    let vb = b.iter().find(|(kk, _)| kk == k).map(|(_, v)| v);
                    if let (Some(va), Some(vb)) = (va, vb) {
                        let c = va.order(vb);
                        if c != Ordering::Equal {
                            return c;
                        }
                    }
                }
                Ordering::Equal
            }
            _ => {
                // Numbers compare numerically; otherwise fall back to type rank.
                if let (Some(a), Some(b)) = (self.as_f64(), other.as_f64()) {
                    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
                } else {
                    rank(self).cmp(&rank(other))
                }
            }
        }
    }

    /// Structural equality consistent with [`Value::order`].
    pub fn value_eq(&self, other: &Value) -> bool {
        self.order(other) == Ordering::Equal
    }

    /// Render as a raw scalar for query output (no surrounding quotes on
    /// strings). Composite values are rendered as JSON.
    pub fn to_raw_string(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format_f64(*f),
            Value::Array(_) | Value::Object(_) => self.to_json(),
        }
    }

    /// Render as compact JSON.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => out.push_str(&i.to_string()),
            Value::Float(f) => out.push_str(&format_f64(*f)),
            Value::Str(s) => write_json_string(s, out),
            Value::Array(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_json(out);
                }
                out.push(']');
            }
            Value::Object(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(k, out);
                    out.push(':');
                    v.write_json(out);
                }
                out.push('}');
            }
        }
    }
}

impl From<&str> for Value {
    fn from(input: &str) -> Self {
        Self::Str(input.to_string())
    }
}

impl From<String> for Value {
    fn from(input: String) -> Self {
        Self::Str(input)
    }
}

/// Format an f64 the way jq does: integral values print without a decimal point.
fn format_f64(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr(v: &[Value]) -> Value {
        Value::Array(v.to_vec())
    }
    fn obj(pairs: &[(&str, Value)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn total_order_across_types() {
        // jq's ordering: null < bool < number < string < array < object.
        let ladder = [
            Value::Null,
            Value::Bool(false),
            Value::Bool(true),
            Value::Int(-1),
            Value::Float(2.5),
            Value::Str("a".into()),
            arr(&[Value::Int(1)]),
            obj(&[("k", Value::Int(1))]),
        ];
        for i in 0..ladder.len() {
            for j in 0..ladder.len() {
                let want = i.cmp(&j);
                // Equal ranks (the two bools, two numbers) compare by value, so
                // only assert the strict cross-rank orderings here.
                if want != Ordering::Equal {
                    assert_eq!(
                        ladder[i].order(&ladder[j]),
                        want,
                        "{:?} vs {:?}",
                        ladder[i],
                        ladder[j]
                    );
                }
            }
        }
    }

    #[test]
    fn numbers_compare_across_int_and_float() {
        assert!(Value::Int(1).value_eq(&Value::Float(1.0)));
        assert_eq!(Value::Int(2).order(&Value::Float(2.5)), Ordering::Less);
        assert_eq!(Value::Float(3.0).order(&Value::Int(3)), Ordering::Equal);
        assert_eq!(Value::Int(1).as_f64(), Some(1.0));
        assert_eq!(Value::Str("x".into()).as_f64(), None);
    }

    #[test]
    fn arrays_and_objects_order_structurally() {
        // Arrays compare element-wise, then by length.
        assert_eq!(
            arr(&[Value::Int(1), Value::Int(2)]).order(&arr(&[Value::Int(1), Value::Int(3)])),
            Ordering::Less
        );
        assert_eq!(
            arr(&[Value::Int(1)]).order(&arr(&[Value::Int(1), Value::Int(0)])),
            Ordering::Less
        );
        // Objects compare by sorted keys, then values at those keys.
        assert_eq!(
            obj(&[("a", Value::Int(1))]).order(&obj(&[("b", Value::Int(1))])),
            Ordering::Less
        );
        assert_eq!(
            obj(&[("a", Value::Int(1))]).order(&obj(&[("a", Value::Int(2))])),
            Ordering::Less
        );
        // Key order doesn't affect equality.
        assert!(
            obj(&[("a", Value::Int(1)), ("b", Value::Int(2))])
                .value_eq(&obj(&[("b", Value::Int(2)), ("a", Value::Int(1))]))
        );
    }

    #[test]
    fn raw_string_and_truthiness() {
        assert_eq!(Value::Float(1.0).to_raw_string(), "1");
        assert_eq!(Value::Float(1.5).to_raw_string(), "1.5");
        assert_eq!(Value::Null.to_raw_string(), "null");
        assert_eq!(Value::Bool(true).to_raw_string(), "true");
        assert_eq!(arr(&[Value::Int(1)]).to_raw_string(), "[1]");
        assert_eq!(obj(&[("a", Value::Int(1))]).to_raw_string(), "{\"a\":1}");
        // Only false and null are falsy (jq).
        assert!(Value::Int(0).is_truthy());
        assert!(Value::Str("".into()).is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Null.is_truthy());
    }

    #[test]
    fn json_encoding_escapes_and_floats() {
        assert_eq!(
            Value::Str("a\"b\\c\n\t\r".into()).to_json(),
            "\"a\\\"b\\\\c\\n\\t\\r\""
        );
        // A control char below 0x20 becomes a \u escape.
        assert_eq!(Value::Str("\u{0001}".into()).to_json(), "\"\\u0001\"");
        // Integral floats print without a decimal point; fractional keep it.
        assert_eq!(Value::Float(42.0).to_json(), "42");
        assert_eq!(Value::Float(2.5).to_json(), "2.5");
        assert_eq!(
            obj(&[("n", Value::Null), ("xs", arr(&[Value::Bool(true)]))]).to_json(),
            "{\"n\":null,\"xs\":[true]}"
        );
        assert_eq!(arr(&[]).to_json(), "[]");
    }

    #[test]
    fn type_names() {
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Bool(true).type_name(), "boolean");
        assert_eq!(Value::Int(1).type_name(), "number");
        assert_eq!(Value::Float(1.0).type_name(), "number");
        assert_eq!(Value::Str("x".into()).type_name(), "string");
        assert_eq!(arr(&[]).type_name(), "array");
        assert_eq!(obj(&[]).type_name(), "object");
    }
}
