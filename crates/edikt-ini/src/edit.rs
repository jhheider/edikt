//! Format-preserving edits for INI.
//!
//! Entries carry their value in a `Value` node, so `set` is a `replace_with` of
//! just that node (key/separator/spacing untouched). Each `Entry` spans its whole
//! line including the terminator, so `del` is a single `detach`.
//!
//! INI values are scalars: setting an array or object errors (the format has no
//! nesting or arrays — its `Feature` set says so).

use crate::syntax::{Sk, SyntaxNode, sk};
use crate::{Ini, project};
use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval};
use rowan::{GreenNode, GreenNodeBuilder};

pub fn apply(doc: &mut Ini, expr: &Expr) -> Result<(), EditError> {
    match expr {
        Expr::Assign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let value = eval_one(rhs, &doc.to_value())?;
            doc.set(steps, &value)
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let current = doc
                .value_at(steps)
                .ok_or_else(|| EditError::new("path not found"))?;
            let value = eval_one(rhs, &current)?;
            doc.set(steps, &value)
        }
        Expr::AddAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let current = doc
                .value_at(steps)
                .ok_or_else(|| EditError::new("path not found"))?;
            let addend = eval_one(rhs, &doc.to_value())?;
            doc.set(steps, &add_values(&current, &addend)?)
        }
        Expr::Pipe(a, b) => {
            apply(doc, a)?;
            apply(doc, b)
        }
        Expr::Call(name, args) if name == "del" => {
            if args.len() != 1 {
                return Err(EditError::new("del(...) takes one path argument"));
            }
            let steps = args[0]
                .as_path()
                .ok_or_else(|| EditError::new("del(...) takes a path"))?;
            doc.delete(steps)
        }
        _ => Err(EditError::new(
            "expected an assignment (`path = value`) or `del(path)`",
        )),
    }
}

fn assign_path(lhs: &Expr) -> Result<&[Step], EditError> {
    lhs.as_path()
        .ok_or_else(|| EditError::new("left side of an assignment must be a path"))
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

/// Resolve `.key` (preamble) or `.section.key` to its `Entry` node.
pub(crate) fn resolve_entry(root: &SyntaxNode, path: &[Step]) -> Option<SyntaxNode> {
    match path {
        [Step::Field(key)] => find_entry(&preamble(root)?, key),
        [Step::Field(section), Step::Field(key)] => find_entry(&named_section(root, section)?, key),
        _ => None,
    }
}

fn preamble(root: &SyntaxNode) -> Option<SyntaxNode> {
    root.children()
        .filter(|n| n.kind() == Sk::Section)
        .find(|s| s.children().all(|c| c.kind() != Sk::Header))
}

fn named_section(root: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    root.children()
        .filter(|n| n.kind() == Sk::Section)
        .find(|s| {
            s.children()
                .find(|c| c.kind() == Sk::Header)
                .map(|h| project::section_name(&h))
                .as_deref()
                == Some(name)
        })
}

fn find_entry(section: &SyntaxNode, key: &str) -> Option<SyntaxNode> {
    section
        .children()
        .filter(|n| n.kind() == Sk::Entry)
        .find(|e| project::entry_key(e) == key)
}

/// A `Value` node wrapping the string `s` (empty node if `s` is empty).
pub(crate) fn value_node_green(s: &str) -> GreenNode {
    let mut b = GreenNodeBuilder::new();
    b.start_node(sk(Sk::Value));
    if !s.is_empty() {
        b.token(sk(Sk::ValStr), s);
    }
    b.finish_node();
    b.finish()
}

/// The string form of a scalar value, or an error for arrays/objects.
pub(crate) fn scalar_string(value: &Value) -> Result<String, EditError> {
    match value {
        Value::Array(_) | Value::Object(_) => Err(EditError::new(
            "INI values are scalars; cannot store an array or object",
        )),
        other => Ok(other.to_raw_string()),
    }
}
