//! Format-preserving edits for `.env` / `.properties`.
//!
//! Flat: paths are a single `.key`. `set` replaces just the entry's value node
//! (key/separator/spacing kept); `del` detaches the whole line. Values are
//! strings; an array or object errors (the format is flat and string-only).

use crate::syntax::{Sk, SyntaxNode};
use crate::{Dialect, Env, project};
use edikt_core::{Document, EditError, Expr, Mutable, MutationKind, Step, Value};

pub fn apply(doc: &mut Env, expr: &Expr) -> Result<(), EditError> {
    edikt_core::apply_mutation(doc, expr)
}

/// Paths are a single `.key`, which [`Mutable::check_path`] enforces before
/// any of the primitives below see one.
impl Mutable for Env {
    const FANS_OUT: bool = false;

    fn whole(&self) -> Value {
        self.to_value()
    }
    fn value_at(&self, path: &[Step]) -> Option<Value> {
        Env::value_at(self, single_key(path).ok()?)
    }
    fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        Env::set(self, single_key(path)?, value)
    }
    fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        Env::delete(self, single_key(path)?)
    }
    fn miss(&self, _path: &[Step]) -> Result<(), EditError> {
        Err(EditError::new("key not found"))
    }
    fn check_path(&self, path: &[Step], _kind: MutationKind) -> Result<(), EditError> {
        single_key(path).map(|_| ())
    }
}

/// A flat file's path must be exactly one field, `.key`.
fn single_key(path: &[Step]) -> Result<&str, EditError> {
    match path {
        [Step::Field(k)] => Ok(k),
        [] => Err(EditError::new("expected a key, got `.`")),
        _ => Err(EditError::new(
            "this format is flat: paths are a single `.key`",
        )),
    }
}

pub(crate) fn find_entry(root: &SyntaxNode, key: &str) -> Option<SyntaxNode> {
    root.children()
        .filter(|n| n.kind() == Sk::Entry)
        .find(|e| project::entry_key(e) == key)
}

pub(crate) fn format_name(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Punctuated => ".env",
        Dialect::Spaced => "envspaced",
    }
}

/// Refuse a value this line format cannot hold: the scanner would read the
/// line back differently. There is no quoting to reach for (the contract
/// keeps `.env` free of quoting semantics), so the honest answer is an error.
pub(crate) fn check_value(text: &str, dialect: Dialect) -> Result<(), EditError> {
    let fmt = format_name(dialect);
    if text.contains(['\n', '\r']) {
        return Err(EditError::new(format!(
            "{fmt} can't hold a value with a line break: the rest would read back as \
             another line, and {fmt} has no quoting"
        )));
    }
    if text.trim() != text {
        return Err(EditError::new(format!(
            "{fmt} can't hold a value with leading or trailing whitespace: it reads \
             back trimmed, and {fmt} has no quoting ({text:?})"
        )));
    }
    Ok(())
}

/// Refuse a new key this line format cannot hold (an existing key is
/// already known to read back as itself).
pub(crate) fn check_key(key: &str, dialect: Dialect) -> Result<(), EditError> {
    let fmt = format_name(dialect);
    let why = if key.contains(['\n', '\r']) {
        Some("a line break would split the line")
    } else if key.trim() != key {
        Some("leading or trailing whitespace reads back trimmed")
    } else if key.starts_with(['#', '!']) {
        Some("a line starting with `#` or `!` is a comment")
    } else {
        match dialect {
            Dialect::Punctuated if key.contains(['=', ':']) => {
                Some("`=` or `:` would end the key early")
            }
            Dialect::Spaced if key.is_empty() => Some("the value would read back as the key"),
            Dialect::Spaced if key.contains(char::is_whitespace) => {
                Some("whitespace would end the key early")
            }
            _ => None,
        }
    };
    match why {
        Some(why) => Err(EditError::new(format!(
            "{fmt} can't hold the key {key:?}: {why}, and {fmt} has no quoting"
        ))),
        None => Ok(()),
    }
}
