//! Project an INI CST onto the [`Value`] model.
//!
//! Shape: a top-level object whose keys are preamble entry keys and section
//! names; each named section is an object of its entries. Values are strings.

use crate::syntax::{Sk, SyntaxNode};
use edikt_core::Value;

pub(crate) fn to_value(root: &SyntaxNode) -> Value {
    let mut top: Vec<(String, Value)> = Vec::new();
    for section in root.children().filter(|n| n.kind() == Sk::Section) {
        match section.children().find(|n| n.kind() == Sk::Header) {
            Some(header) => {
                top.push((
                    section_name(&header),
                    Value::Object(section_entries(&section)),
                ));
            }
            None => top.extend(section_entries(&section)),
        }
    }
    Value::Object(top)
}

pub(crate) fn section_entries(section: &SyntaxNode) -> Vec<(String, Value)> {
    section
        .children()
        .filter(|n| n.kind() == Sk::Entry)
        .map(|entry| (entry_key(&entry), Value::Str(entry_value(&entry))))
        .collect()
}

pub(crate) fn section_name(header: &SyntaxNode) -> String {
    token_text(header, Sk::Name)
}

pub(crate) fn entry_key(entry: &SyntaxNode) -> String {
    token_text(entry, Sk::Key)
}

/// The entry's value text (the `Value` node holds just the trimmed core string).
pub(crate) fn entry_value(entry: &SyntaxNode) -> String {
    entry
        .children()
        .find(|n| n.kind() == Sk::Value)
        .map(|v| v.text().to_string())
        .unwrap_or_default()
}

fn token_text(node: &SyntaxNode, kind: Sk) -> String {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == kind)
        .map(|t| t.text().to_string())
        .unwrap_or_default()
}
