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
        .map(|v| {
            let raw = v.text().to_string();
            // Strip Java .properties continuations: backslash-newline + leading
            // whitespace on continuation lines.
            let mut result = String::new();
            let mut chars = raw.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if chars.peek() == Some(&'\n') {
                        chars.next(); // consume newline
                        // Skip leading whitespace on continuation line
                        while chars
                            .peek()
                            .map(|c| c.is_whitespace() && *c != '\n')
                            .unwrap_or(false)
                        {
                            chars.next();
                        }
                        continue;
                    }
                    result.push(c);
                } else {
                    result.push(c);
                }
            }
            result
        })
        .unwrap_or_default()
}
