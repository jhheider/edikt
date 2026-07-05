//! Format-preserving edits via rowan structural-sharing splice.
//!
//! An edit resolves the target *value node* in the CST and swaps it for a fresh
//! subtree; rowan shares every untouched green node, so serialization stays
//! byte-identical everywhere except the value we replaced. The replacement's own
//! bytes are compact JSON — we format what we insert, never what we didn't touch.
//!
//! M2 scope: `set` (`=` / `|=`) on existing, concrete paths. `del`, append, and
//! new-key creation arrive in later slices.

use crate::syntax::{Sk, SyntaxNode};
use crate::{Jsonc, parser, project};
use edikt_core::{Document, Expr, Step, Value, eval};
use rowan::GreenNode;

/// An edit failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditError {
    pub msg: String,
}

impl EditError {
    pub(crate) fn new(msg: impl Into<String>) -> EditError {
        EditError { msg: msg.into() }
    }
}
impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}
impl std::error::Error for EditError {}

/// Apply a mutation expression to `doc`, preserving format everywhere untouched.
pub fn apply(doc: &mut Jsonc, expr: &Expr) -> Result<(), EditError> {
    match expr {
        Expr::Assign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            // `path = rhs`: rhs is evaluated against the whole document.
            let whole = doc.to_value();
            let value = eval_one(rhs, &whole)?;
            doc.set(steps, &value)
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            // `path |= rhs`: rhs sees the current value at `path`.
            let current = doc
                .value_at(steps)
                .ok_or_else(|| EditError::new("path not found"))?;
            let value = eval_one(rhs, &current)?;
            doc.set(steps, &value)
        }
        Expr::Pipe(a, b) => {
            apply(doc, a)?;
            apply(doc, b)
        }
        Expr::Call(name, _) if name == "del" => Err(EditError::new(
            "del(...) is not supported yet (arrives in a later slice)",
        )),
        _ => Err(EditError::new(
            "expected an assignment: `path = value` or `path |= expr`",
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

/// Walk `path` from the document root to the target value node.
pub(crate) fn resolve_value_node(root: &SyntaxNode, path: &[Step]) -> Option<SyntaxNode> {
    let mut current = root.children().find(|n| n.kind() == Sk::Value)?;
    for step in path {
        current = step_into(&current, step)?;
    }
    Some(current)
}

fn step_into(value_node: &SyntaxNode, step: &Step) -> Option<SyntaxNode> {
    match step {
        Step::Field(key) => {
            let object = value_node.children().find(|n| n.kind() == Sk::Object)?;
            object
                .children()
                .filter(|n| n.kind() == Sk::Member)
                .find_map(|member| {
                    let member_key = member
                        .children_with_tokens()
                        .filter_map(|e| e.into_token())
                        .find(|t| t.kind() == Sk::Str)
                        .map(|t| project::unescape(t.text()));
                    if member_key.as_deref() == Some(key) {
                        member.children().find(|n| n.kind() == Sk::Value)
                    } else {
                        None
                    }
                })
        }
        Step::Index(i) => {
            let array = value_node.children().find(|n| n.kind() == Sk::Array)?;
            let values: Vec<_> = array.children().filter(|n| n.kind() == Sk::Value).collect();
            let idx = if *i < 0 { values.len() as i64 + i } else { *i };
            if idx < 0 {
                return None;
            }
            values.into_iter().nth(idx as usize)
        }
        // Setting through `[]` (all elements) needs multi-target splicing; later.
        Step::Iterate => None,
    }
}

/// Build a `Value`-node green subtree by rendering `value` as compact JSON and
/// reparsing it — the inserted bytes are formatted; surrounding layout is not.
pub(crate) fn value_green(value: &Value) -> GreenNode {
    let json = value.to_json();
    let root = SyntaxNode::new_root(parser::build(&json));
    let value_node = root
        .children()
        .find(|n| n.kind() == Sk::Value)
        .expect("compact JSON always has a top-level value");
    value_node.green().into_owned()
}
