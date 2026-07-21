//! Project a JSONC CST onto the [`Value`] model (the data-model view).
//!
//! Trivia is dropped here by design: this is what querying and conversion see,
//! not the format-preserving source view.

use crate::syntax::{Sk, SyntaxNode};
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
    matches!(kind, Sk::Str | Sk::Num | Sk::True | Sk::False | Sk::Null)
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
                Sk::Str => return Value::Str(unescape(t.text())),
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
        let key = member
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .find(|t| t.kind() == Sk::Str)
            .map(|t| unescape(t.text()))
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

fn number(text: &str) -> Value {
    if text.contains(['.', 'e', 'E']) {
        Value::Float(text.parse().unwrap_or(0.0))
    } else {
        match text.parse::<i64>() {
            Ok(i) => Value::Int(i),
            Err(_) => Value::Float(text.parse().unwrap_or(0.0)),
        }
    }
}

/// Unescape a JSON double-quoted string token (quotes included).
pub(crate) fn unescape(tok: &str) -> String {
    let inner = tok
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
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
