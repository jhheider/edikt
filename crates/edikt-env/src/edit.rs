//! Format-preserving edits for `.env` / `.properties`.
//!
//! Flat: paths are a single `.key`. `set` replaces just the entry's value node
//! (key/separator/spacing kept); `del` detaches the whole line. Values are
//! strings; an array or object errors (the format is flat and string-only).

use crate::syntax::{Sk, SyntaxNode, sk};
use crate::{Dialect, Env, project};
use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval};
use rowan::{GreenNode, GreenNodeBuilder};

pub fn apply(doc: &mut Env, expr: &Expr) -> Result<(), EditError> {
    // A path-expression target (`(.xs[] | select(...) | .n) = v`, #88)
    // resolves to concrete paths first; each is then an ordinary edit.
    if let Some(each) = edikt_core::lower_mutation(expr, || doc.to_value())
        .map_err(|e| EditError::new(e.to_string()))?
    {
        for e in &each {
            apply(doc, e)?;
        }
        return Ok(());
    }
    match expr {
        Expr::Assign(lhs, rhs) => {
            let key = single_key(lhs)?;
            let value = eval_one(rhs, &doc.to_value())?;
            doc.set(key, &value)
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let key = single_key(lhs)?;
            let current = doc
                .value_at(key)
                .ok_or_else(|| EditError::new("key not found"))?;
            let value = eval_one(rhs, &current)?;
            doc.set(key, &value)
        }
        Expr::AddAssign(lhs, rhs) => {
            let key = single_key(lhs)?;
            let current = doc
                .value_at(key)
                .ok_or_else(|| EditError::new("key not found"))?;
            let addend = eval_one(rhs, &doc.to_value())?;
            doc.set(key, &add_values(&current, &addend)?)
        }
        Expr::Pipe(a, b) => {
            apply(doc, a)?;
            apply(doc, b)
        }
        Expr::Call(name, args) if name == "del" => {
            if args.len() != 1 {
                return Err(EditError::new("del(...) takes one path argument"));
            }
            let key = single_key(&args[0])?;
            doc.delete(key)
        }
        _ => Err(EditError::new(
            "expected an assignment (`key = value`) or `del(key)`",
        )),
    }
}

/// A flat file's path must be exactly one field, `.key`.
fn single_key(expr: &Expr) -> Result<&str, EditError> {
    match expr.as_path() {
        Some([Step::Field(k)]) => Ok(k),
        Some([]) => Err(EditError::new("expected a key, got `.`")),
        _ => Err(EditError::new(
            "this format is flat: paths are a single `.key`",
        )),
    }
}

fn eval_one(expr: &Expr, input: &Value) -> Result<Value, EditError> {
    eval(expr, input)
        .map_err(|e| EditError::new(e.to_string()))?
        .into_iter()
        .next()
        .ok_or_else(|| EditError::new("right side of the assignment produced no value"))
}

fn add_values(current: &Value, addend: &Value) -> Result<Value, EditError> {
    let expr = Expr::Binary(
        BinOp::Add,
        Box::new(Expr::Path(Vec::new())),
        Box::new(Expr::Literal(addend.clone())),
    );
    eval_one(&expr, current)
}

pub(crate) fn find_entry(root: &SyntaxNode, key: &str) -> Option<SyntaxNode> {
    root.children()
        .filter(|n| n.kind() == Sk::Entry)
        .find(|e| project::entry_key(e) == key)
}

pub(crate) fn value_node_green(s: &str) -> GreenNode {
    let mut b = GreenNodeBuilder::new();
    b.start_node(sk(Sk::Value));
    if !s.is_empty() {
        b.token(sk(Sk::ValStr), s);
    }
    b.finish_node();
    b.finish()
}

fn format_name(dialect: Dialect) -> &'static str {
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

pub(crate) fn scalar_string(value: &Value) -> Result<String, EditError> {
    match value {
        Value::Array(_) | Value::Object(_) => Err(EditError::new(
            "this format is flat and string-only; cannot store an array or object",
        )),
        other => Ok(other.to_raw_string()),
    }
}
