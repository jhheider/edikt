//! Scalar resolution and inline emission.
//!
//! libyaml hands us a scalar's *text* plus a *style* flag; the YAML 1.2 core
//! schema decides whether a plain scalar is null/bool/int/float or a string
//! (quoted scalars are always strings). serde_yaml used to do this for us; doing
//! it ourselves is ~a screenful and strictly more controllable.
//!
//! The inverse — turning a `Value` back into scalar bytes for an edit — must
//! round-trip the *type*: a string that looks like a number (`"3"`) is quoted so
//! it re-reads as a string, a `Float(1.0)` keeps its `.0`, etc.

use edikt_core::{EditError, Value};
use libyaml_safer::ScalarStyle;

/// Resolve a scalar event (text + style + optional explicit tag) to a [`Value`].
pub(crate) fn resolve_scalar(text: &str, style: ScalarStyle, tag: Option<&str>) -> Value {
    // A quoted / block scalar is unambiguously a string, whatever it spells.
    if style != ScalarStyle::Plain && style != ScalarStyle::Any {
        return Value::Str(text.to_string());
    }
    // An explicit `!!str` tag pins it to string too.
    if matches!(tag, Some("tag:yaml.org,2002:str" | "!!str")) {
        return Value::Str(text.to_string());
    }
    resolve_plain(text)
}

/// YAML 1.2 core-schema resolution of a plain scalar's text.
fn resolve_plain(s: &str) -> Value {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        _ => {
            if let Some(i) = parse_int(s) {
                Value::Int(i)
            } else if let Some(f) = parse_float(s) {
                Value::Float(f)
            } else {
                Value::Str(s.to_string())
            }
        }
    }
}

/// Core-schema integer: decimal (`[-+]?[0-9]+`), `0x…` hex, `0o…` octal.
fn parse_int(s: &str) -> Option<i64> {
    if let Some(hex) = s.strip_prefix("0x") {
        return i64::from_str_radix(hex, 16).ok();
    }
    if let Some(oct) = s.strip_prefix("0o") {
        return i64::from_str_radix(oct, 8).ok();
    }
    // Reject a bare `+`/`-` and require all digits after an optional sign so we
    // don't accidentally swallow things like `1_000` or `1.0`.
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse::<i64>().ok();
    }
    None
}

/// Core-schema float: `.inf`/`.nan` variants, or a decimal with a `.` or exponent.
/// Deliberately does *not* accept bare `inf`/`nan`/`infinity` (those are strings
/// in YAML — only the `.`-prefixed forms are floats).
fn parse_float(s: &str) -> Option<f64> {
    match s {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => return Some(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" => return Some(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Some(f64::NAN),
        _ => {}
    }
    // Must look numeric and carry a fraction or exponent (else it's an int/string).
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let looks_floaty = !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
        && body.bytes().any(|b| matches!(b, b'.' | b'e' | b'E'))
        && body.bytes().any(|b| b.is_ascii_digit());
    if looks_floaty {
        return s.parse::<f64>().ok();
    }
    None
}

/// Render a scalar [`Value`] to inline YAML bytes suitable for splicing in place
/// of an existing scalar. Collections error — restructuring a block in place
/// isn't lossless, so edikt refuses rather than reflow.
pub(crate) fn emit_scalar_inline(value: &Value) -> Result<String, EditError> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Int(i) => Ok(i.to_string()),
        Value::Float(f) => Ok(format_float(*f)),
        Value::Str(s) => Ok(emit_string(s)),
        Value::Array(_) | Value::Object(_) => Err(EditError::new(
            "edikt sets scalar values in YAML losslessly; it can't yet replace or \
             create a mapping/sequence in block context",
        )),
    }
}

/// A YAML key rendered inline (same quoting rules as a string value).
pub(crate) fn emit_key(key: &str) -> String {
    emit_string(key)
}

/// Format a float so it re-reads as a float (never as an int).
pub(crate) fn format_float(f: f64) -> String {
    if f.is_nan() {
        return ".nan".to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-.inf" } else { ".inf" }.to_string();
    }
    let s = format!("{f}");
    if s.bytes().any(|b| matches!(b, b'.' | b'e' | b'E')) {
        s
    } else {
        format!("{s}.0")
    }
}

/// Emit a string as a plain scalar when unambiguous, else double-quoted.
fn emit_string(s: &str) -> String {
    if needs_quoting(s) {
        double_quote(s)
    } else {
        s.to_string()
    }
}

/// Would this string be misread (as null/bool/number) or break plain style?
pub(crate) fn needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    // Would a plain reader resolve it to something other than this same string?
    if !matches!(resolve_plain(s), Value::Str(_)) {
        return true;
    }
    // Leading/trailing space, or a plain-style indicator that changes meaning.
    let bytes = s.as_bytes();
    if bytes[0].is_ascii_whitespace() || bytes[bytes.len() - 1].is_ascii_whitespace() {
        return true;
    }
    // First char must not be a YAML indicator.
    if matches!(
        bytes[0],
        b'!' | b'&'
            | b'*'
            | b'?'
            | b'|'
            | b'>'
            | b'%'
            | b'@'
            | b'`'
            | b'"'
            | b'\''
            | b'#'
            | b','
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'-'
            | b':'
    ) {
        return true;
    }
    // Any control char, a `: ` (map indicator) or ` #` (comment indicator), or a
    // quote/backslash forces quoting.
    s.contains(": ")
        || s.contains(" #")
        || s.ends_with(':')
        || s.bytes()
            .any(|b| b < 0x20 || b == b'"' || b == b'\\' || b == b'\t')
}

/// Minimal double-quoted YAML string with the standard escapes.
fn double_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_core_schema() {
        let plain = |s| resolve_scalar(s, ScalarStyle::Plain, None);
        assert_eq!(plain("null"), Value::Null);
        assert_eq!(plain("~"), Value::Null);
        assert_eq!(plain(""), Value::Null);
        assert_eq!(plain("true"), Value::Bool(true));
        assert_eq!(plain("False"), Value::Bool(false));
        assert_eq!(plain("3"), Value::Int(3));
        assert_eq!(plain("-17"), Value::Int(-17));
        assert_eq!(plain("0x1F"), Value::Int(31));
        assert_eq!(plain("1.5"), Value::Float(1.5));
        assert_eq!(plain("nginx:1.25"), Value::Str("nginx:1.25".into()));
        assert_eq!(plain("yes"), Value::Str("yes".into())); // 1.2 core: not a bool
        assert_eq!(plain("inf"), Value::Str("inf".into())); // bare inf is a string
    }

    #[test]
    fn quoted_is_always_string() {
        assert_eq!(
            resolve_scalar("3", ScalarStyle::DoubleQuoted, None),
            Value::Str("3".into())
        );
        assert_eq!(
            resolve_scalar("true", ScalarStyle::SingleQuoted, None),
            Value::Str("true".into())
        );
    }

    #[test]
    fn inline_round_trips_type() {
        // A string that looks like a scalar must come back quoted.
        assert_eq!(
            emit_scalar_inline(&Value::Str("3".into())).unwrap(),
            "\"3\""
        );
        assert_eq!(
            emit_scalar_inline(&Value::Str("true".into())).unwrap(),
            "\"true\""
        );
        // A plain word stays plain.
        assert_eq!(
            emit_scalar_inline(&Value::Str("nginx".into())).unwrap(),
            "nginx"
        );
        // Floats keep their point.
        assert_eq!(emit_scalar_inline(&Value::Float(1.0)).unwrap(), "1.0");
        assert_eq!(emit_scalar_inline(&Value::Int(42)).unwrap(), "42");
        assert_eq!(emit_scalar_inline(&Value::Bool(true)).unwrap(), "true");
        assert_eq!(emit_scalar_inline(&Value::Null).unwrap(), "null");
    }

    #[test]
    fn quotes_dangerous_strings() {
        assert_eq!(emit_string(""), "\"\"");
        assert_eq!(emit_string("a: b"), "\"a: b\"");
        assert_eq!(emit_string("trailing "), "\"trailing \"");
        assert_eq!(emit_string("has#hash"), "has#hash"); // '#' not preceded by space is fine
        assert_eq!(emit_string("has # comment"), "\"has # comment\""); // ' #' starts a comment
        assert_eq!(emit_string("- dash"), "\"- dash\"");
    }
}
