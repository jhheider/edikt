//! Scalar resolution and inline emission.
//!
//! libyaml hands us a scalar's *text* plus a *style* flag; the YAML 1.2 core
//! schema decides whether a plain scalar is null/bool/int/float or a string
//! (quoted scalars are always strings). serde_yaml used to do this for us; doing
//! it ourselves is ~a screenful and strictly more controllable.
//!
//! The inverse (turning a `Value` back into scalar bytes for an edit) must
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

/// Core-schema integer: decimal (`[-+]?[0-9]+`), `0x...` hex, `0o...` octal.
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
/// in YAML; only the `.`-prefixed forms are floats).
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
/// of an existing scalar. A collection has no single inline spelling; callers
/// lay those out with [`crate::layout`], so one reaching here is a bug.
pub(crate) fn emit_scalar_inline(value: &Value) -> Result<String, EditError> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Int(i) => Ok(i.to_string()),
        Value::Float(f) => Ok(format_float(*f)),
        Value::Str(s) => Ok(emit_string(s)),
        Value::Array(_) | Value::Object(_) => Err(EditError::new(
            "internal: a mapping or sequence reached the inline scalar emitter",
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

/// The spelling of an existing scalar token, as far as re-spelling a new value
/// in it goes. Block (`|`/`>`) scalars never reach here ([`crate::block`]
/// respells them), and an alias (`*a`) has no quote style, so both read as plain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuoteStyle {
    Plain,
    Single,
    Double,
}

impl QuoteStyle {
    /// The style of a scalar body (its properties already split off).
    pub(crate) fn of(body: &str) -> Self {
        match body.as_bytes().first() {
            Some(b'"') => Self::Double,
            Some(b'\'') => Self::Single,
            _ => Self::Plain,
        }
    }
}

/// Render a scalar [`Value`] to replace an existing scalar in place, reusing
/// that scalar's quote style (jhheider/edikt#81).
///
/// Only strings carry a quote style: a number, bool, or null is written plain,
/// since quoting it would change its type. A string keeps the old style when
/// it can be spelled in it and falls back to double quotes (which spell
/// anything) when it can't: a single-quoted scalar has no escapes, so a
/// control character or line break can't go there, and a plain scalar can't
/// hold anything [`needs_quoting`] flags. Plain has two more limits: inside a
/// flow collection (`flow`) a `,[]{}` would end the scalar early, and a word a
/// YAML 1.1 reader types differently (`yes`, `2001-12-14`, `1_000`) is quoted
/// unless the replaced scalar (`old_plain`) was that same kind of word, so a
/// file relying on 1.1 booleans can still flip `yes` to `no`.
pub(crate) fn emit_scalar_styled(
    value: &Value,
    style: QuoteStyle,
    flow: bool,
    old_plain: &str,
) -> Result<String, EditError> {
    let Value::Str(s) = value else {
        return emit_scalar_inline(value);
    };
    Ok(match style {
        QuoteStyle::Double => double_quote(s),
        QuoteStyle::Single if single_quotable(s) => single_quote(s),
        QuoteStyle::Single => double_quote(s),
        QuoteStyle::Plain => {
            let yaml11 = yaml11_kind(s);
            if needs_quoting_core(s)
                || (flow && s.contains([',', '[', ']', '{', '}']))
                || (yaml11.is_some() && yaml11 != yaml11_kind(old_plain))
            {
                double_quote(s)
            } else {
                s.to_string()
            }
        }
    })
}

/// Split a scalar's source token into its node properties (an anchor `&a`
/// and/or a tag `!t`, with the whitespace after them) and the scalar body.
/// A plain scalar can't start with `&` or `!`, so a leading one is always a
/// property.
pub(crate) fn split_properties(token: &str) -> (&str, &str) {
    let mut body = token;
    while body.starts_with(['&', '!']) {
        let len = body
            .find([' ', '\t', '\n', '\r', ',', '[', ']', '{', '}'])
            .unwrap_or(body.len());
        body = body[len..].trim_start_matches([' ', '\t']);
    }
    token.split_at(token.len() - body.len())
}

/// The properties to keep when a scalar's value is replaced. An anchor always
/// stays (an alias elsewhere may name it). A core-schema tag (`!!str`,
/// `!!int`, ...) stays only while it still names the new value's type, since
/// `!!str 5` would pin a new int back to a string; any other tag is the
/// application's (`!Ref`, `!!binary`) and stays.
pub(crate) fn kept_properties<'a>(props: &'a str, value: &Value) -> std::borrow::Cow<'a, str> {
    let pieces: Vec<&str> = props.split_whitespace().collect();
    let keep = |p: &&str| match core_tag(p) {
        Some(ty) => ty == value_tag(value),
        None => true,
    };
    if pieces.iter().all(keep) {
        return props.into();
    }
    let kept: Vec<&str> = pieces.into_iter().filter(keep).collect();
    if kept.is_empty() {
        "".into()
    } else {
        format!("{} ", kept.join(" ")).into()
    }
}

/// The core-schema type a tag names (`!!str` -> `str`), or `None` for any
/// other tag or an anchor.
fn core_tag(piece: &str) -> Option<&str> {
    let ty = piece.strip_prefix("!!").or_else(|| {
        piece
            .strip_prefix("!<tag:yaml.org,2002:")?
            .strip_suffix('>')
    })?;
    matches!(
        ty,
        "str" | "int" | "float" | "bool" | "null" | "seq" | "map"
    )
    .then_some(ty)
}

/// The core-schema tag name of a value's type.
fn value_tag(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "str",
        Value::Array(_) => "seq",
        Value::Object(_) => "map",
    }
}

/// A plain word a YAML 1.1 reader (PyYAML, go-yaml v2, and the many tools built
/// on them) resolves to a non-string, though the 1.2 core schema edikt reads by
/// keeps it a string. Grouped by kind so a replacement of the same kind can stay
/// plain.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Yaml11 {
    Bool,
    Int,
    Timestamp,
}

fn yaml11_kind(s: &str) -> Option<Yaml11> {
    const BOOLS: &[&str] = &[
        "y", "Y", "yes", "Yes", "YES", "n", "N", "no", "No", "NO", "on", "On", "ON", "off", "Off",
        "OFF",
    ];
    if BOOLS.contains(&s) {
        return Some(Yaml11::Bool);
    }
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let b = body.as_bytes();
    let digits_or = |extra: &[u8]| b.iter().all(|c| c.is_ascii_digit() || extra.contains(c));
    let has_digit = b.iter().any(u8::is_ascii_digit);
    // `0b1010`, `1_000` / `1_000.5`, and base-60 `1:30` / `1:30.5`.
    if (body.starts_with("0b")
        && b.len() > 2
        && b[2..].iter().all(|c| matches!(c, b'0' | b'1' | b'_')))
        || (has_digit && body.contains('_') && digits_or(b"_."))
        || (has_digit && b[0].is_ascii_digit() && body.contains(':') && digits_or(b":._"))
    {
        return Some(Yaml11::Int);
    }
    // A timestamp starts `YYYY-M-D`.
    let date = s.split(['T', 't', ' ']).next().unwrap_or("");
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() == 3
        && parts[0].len() == 4
        && (1..=2).contains(&parts[1].len())
        && (1..=2).contains(&parts[2].len())
        && parts.iter().all(|p| p.bytes().all(|c| c.is_ascii_digit()))
    {
        return Some(Yaml11::Timestamp);
    }
    None
}

/// Can `s` be spelled single-quoted on one line? Only `'` has an escape there
/// (doubled), so a character that must be escaped can't.
fn single_quotable(s: &str) -> bool {
    !s.chars().any(needs_escape)
}

/// `s` single-quoted, `'` doubled.
fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A character that can't appear literally in a one-line quoted scalar: a
/// non-printable (per the YAML character set) or a line break, including the
/// Unicode ones YAML 1.1 honours (NEL, LS, PS). Tab is printable but is escaped
/// too, for readability and so it can't be mistaken for separation space.
pub(crate) fn needs_escape(c: char) -> bool {
    !matches!(c,
        '\u{20}'..='\u{7E}'
        | '\u{A0}'..='\u{2027}'
        | '\u{202A}'..='\u{D7FF}'
        | '\u{E000}'..='\u{FEFE}'
        | '\u{FF00}'..='\u{FFFD}'
        | '\u{10000}'..)
}

/// Would this string be misread (as null/bool/number, by the YAML 1.2 core
/// schema or by a YAML 1.1 reader) or break plain style? What a fresh scalar
/// (a new key, a conversion) goes by.
pub(crate) fn needs_quoting(s: &str) -> bool {
    needs_quoting_core(s) || yaml11_kind(s).is_some()
}

/// [`needs_quoting`] minus the YAML 1.1 words, which an in-place replacement
/// weighs against the scalar it replaces.
fn needs_quoting_core(s: &str) -> bool {
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
        || s.contains(['"', '\\'])
        || s.chars().any(needs_escape)
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
            '\u{85}' => out.push_str("\\N"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            c if (c as u32) <= 0xFF && needs_escape(c) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if needs_escape(c) => out.push_str(&format!("\\u{:04x}", c as u32)),
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
        // A word a YAML 1.1 reader would type as a bool/int/date is quoted too.
        assert_eq!(emit_string("yes"), "\"yes\"");
        assert_eq!(emit_string("1_000"), "\"1_000\"");
        assert_eq!(emit_string("2001-12-14"), "\"2001-12-14\"");
    }

    #[test]
    fn resolves_octal_and_str_tagged_scalars() {
        // `0o` octal (the `0x` hex path is covered by `resolves_core_schema`).
        assert_eq!(
            resolve_scalar("0o17", ScalarStyle::Plain, None),
            Value::Int(15)
        );
        // An explicit `!!str` (or its long form) tag pins a plain scalar to
        // string, even when its text would otherwise resolve to a number/bool.
        assert_eq!(
            resolve_scalar("42", ScalarStyle::Plain, Some("!!str")),
            Value::Str("42".into())
        );
        assert_eq!(
            resolve_scalar("true", ScalarStyle::Plain, Some("tag:yaml.org,2002:str")),
            Value::Str("true".into())
        );
    }

    #[test]
    fn inline_refuses_collections() {
        // A collection is laid out by `layout`, never spelled as one token.
        let err = emit_scalar_inline(&Value::Array(vec![Value::Int(1)]))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("reached the inline scalar emitter"),
            "got: {err}"
        );
        assert!(emit_scalar_inline(&Value::Object(vec![])).is_err());
    }

    #[test]
    fn format_float_specials_and_fraction() {
        // NaN / infinities take their YAML spellings.
        assert_eq!(format_float(f64::NAN), ".nan");
        assert_eq!(format_float(f64::INFINITY), ".inf");
        assert_eq!(format_float(f64::NEG_INFINITY), "-.inf");
        // A value whose default formatting already carries a `.` is kept as-is
        // (the `.0` suffix is only appended when the text would read as an int).
        assert_eq!(format_float(1.5), "1.5");
    }

    #[test]
    fn double_quote_escapes_every_branch() {
        // Quote, backslash, newline, tab, CR, NUL, and a generic control char -
        // each forces quoting (via `needs_quoting`) and hits its own escape arm.
        assert_eq!(emit_string("a\"b"), r#""a\"b""#);
        assert_eq!(emit_string("a\\b"), r#""a\\b""#);
        assert_eq!(emit_string("a\nb"), r#""a\nb""#);
        assert_eq!(emit_string("a\tb"), r#""a\tb""#);
        assert_eq!(emit_string("a\rb"), r#""a\rb""#);
        assert_eq!(emit_string("a\0b"), r#""a\0b""#);
        assert_eq!(emit_string("a\u{1}b"), "\"a\\x01b\"");
    }
}
