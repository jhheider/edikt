//! Project a JSONC CST onto the [`Value`] model (the data-model view).
//!
//! Trivia is dropped here by design: this is what querying and conversion see,
//! not the format-preserving source view.

use crate::syntax::{Sk, SyntaxNode, is_key};
use edikt_core::Value;
use rowan::NodeOrToken;

/// Project the whole document (from its `Root`) to a value.
pub(crate) fn to_value(root: &SyntaxNode) -> Value {
    root.children()
        .find(|n| n.kind() == Sk::Value)
        .map(|v| value_node(&v))
        .unwrap_or(Value::Null)
}

/// Is `kind` a scalar value token (used to detect an empty/absent value)?
pub(crate) fn is_value_token(kind: Sk) -> bool {
    // `Ident` is deliberately absent: a bare word is a JSON5 *key*, never a
    // value, so a lone `foo` must still fail the top-level-value check rather
    // than parse as a document.
    matches!(
        kind,
        Sk::Str | Sk::SingleStr | Sk::Num | Sk::True | Sk::False | Sk::Null
    )
}

pub(crate) fn value_node(node: &SyntaxNode) -> Value {
    if let Some(obj) = node.children().find(|n| n.kind() == Sk::Object) {
        return object(&obj);
    }
    if let Some(arr) = node.children().find(|n| n.kind() == Sk::Array) {
        return array(&arr);
    }
    for elem in node.children_with_tokens() {
        if let NodeOrToken::Token(t) = elem {
            match t.kind() {
                Sk::Str | Sk::SingleStr => return Value::Str(unescape(t.text())),
                Sk::Num => return number(t.text()),
                Sk::True => return Value::Bool(true),
                Sk::False => return Value::Bool(false),
                Sk::Null => return Value::Null,
                _ => {}
            }
        }
    }
    Value::Null
}

fn object(node: &SyntaxNode) -> Value {
    let mut pairs = Vec::new();
    for member in node.children().filter(|n| n.kind() == Sk::Member) {
        let key = key_token(&member)
            .map(|t| key_text(t.kind(), t.text()))
            .unwrap_or_default();
        let val = member
            .children()
            .find(|n| n.kind() == Sk::Value)
            .map(|v| value_node(&v))
            .unwrap_or(Value::Null);
        pairs.push((key, val));
    }
    Value::Object(pairs)
}

fn array(node: &SyntaxNode) -> Value {
    let items = node
        .children()
        .filter(|n| n.kind() == Sk::Value)
        .map(|v| value_node(&v))
        .collect();
    Value::Array(items)
}

/// The key token of a member: the first key-position token *before* the colon.
///
/// Bounded by the colon because a member's value may itself be a string, and
/// with JSON5 key spellings a plain "first key-ish token" search would happily
/// return the value of a keyless member.
pub(crate) fn key_token(member: &SyntaxNode) -> Option<crate::syntax::SyntaxToken> {
    member
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .take_while(|t| t.kind() != Sk::Colon)
        .find(|t| is_key(t.kind()))
}

/// The decoded name of a key token, per its spelling.
pub(crate) fn key_text(kind: Sk, text: &str) -> String {
    match kind {
        // A bare identifier, or a reserved word used as a JSON5 key, is its own
        // text; only the quoted spellings carry escapes.
        Sk::Ident | Sk::True | Sk::False | Sk::Null => text.to_string(),
        _ => unescape(text),
    }
}

fn number(text: &str) -> Value {
    // JSON5 non-finite literals, which no radix parse would accept.
    match text {
        "Infinity" | "+Infinity" => return Value::Float(f64::INFINITY),
        "-Infinity" => return Value::Float(f64::NEG_INFINITY),
        "NaN" => return Value::Float(f64::NAN),
        _ => {}
    }
    // JSON5 hex, with an optional sign ahead of the `0x`. Parsed as i64 so hex
    // stays an integer rather than degrading through f64.
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, text.strip_prefix('+').unwrap_or(text)),
    };
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        return match i64::from_str_radix(hex, 16) {
            Ok(i) => Value::Int(sign * i),
            // Wider than i64: fall back to f64 rather than silently zeroing.
            Err(_) => {
                Value::Float(u128::from_str_radix(hex, 16).map_or(0.0, |u| sign as f64 * u as f64))
            }
        };
    }
    // `1.` and `.5` are JSON5 spellings that Rust's f64 parser accepts as-is;
    // a leading `+` it does not, hence parsing `digits` with the sign reapplied.
    if text.contains(['.', 'e', 'E']) {
        return Value::Float(digits.parse::<f64>().map_or(0.0, |f| sign as f64 * f));
    }
    match digits.parse::<i64>() {
        Ok(i) => Value::Int(sign * i),
        Err(_) => Value::Float(digits.parse::<f64>().map_or(0.0, |f| sign as f64 * f)),
    }
}

/// Unescape a quoted string token (quotes included).
///
/// Handles JSON's double quotes and JSON5's single quotes; the escape grammar is
/// otherwise shared, plus JSON5's backslash-newline line continuation.
pub(crate) fn unescape(tok: &str) -> String {
    let inner = tok
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| tok.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(tok);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            // JSON5: an escaped single quote, and a line continuation, which
            // contributes nothing to the value.
            Some('\'') => out.push('\''),
            Some('\n') => {}
            Some('\r') => {
                if chars.clone().next() == Some('\n') {
                    chars.next();
                }
            }
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{0008}'),
            Some('f') => out.push('\u{000c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
