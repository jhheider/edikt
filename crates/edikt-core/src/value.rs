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
