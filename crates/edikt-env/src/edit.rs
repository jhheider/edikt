//! Format-preserving edits for `.env` / `.properties`.
//!
//! Flat: paths are a single `.key`. `set` replaces just the entry's value node
//! (key/separator/spacing kept); `del` detaches the whole line. Values are
//! strings — an array or object errors (the format is flat and string-only).

use crate::syntax::{Sk, SyntaxNode, sk};
use crate::{Env, project};
use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval};
use rowan::{GreenNode, GreenNodeBuilder};

pub fn apply(doc: &mut Env, expr: &Expr) -> Result<(), EditError> {
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

pub(crate) fn scalar_string(value: &Value) -> Result<String, EditError> {
    match value {
        Value::Array(_) | Value::Object(_) => Err(EditError::new(
            "this format is flat and string-only; cannot store an array or object",
        )),
        other => Ok(other.to_raw_string()),
    }
}
