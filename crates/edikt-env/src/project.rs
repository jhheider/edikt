//! Project a `.env` / `.properties` CST onto the [`Value`] model: a flat object
//! of string entries.

use crate::syntax::{Sk, SyntaxNode};
use edikt_core::Value;

pub(crate) fn to_value(root: &SyntaxNode) -> Value {
    let pairs = root
        .children()
        .filter(|n| n.kind() == Sk::Entry)
        .map(|entry| (entry_key(&entry), Value::Str(entry_value(&entry))))
        .collect();
    Value::Object(pairs)
}

pub(crate) fn entry_key(entry: &SyntaxNode) -> String {
    entry
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == Sk::Key)
        .map(|t| t.text().to_string())
        .unwrap_or_default()
}

pub(crate) fn entry_value(entry: &SyntaxNode) -> String {
    entry
        .children()
        .find(|n| n.kind() == Sk::Value)
        .map(|v| v.text().to_string())
        .unwrap_or_default()
}
