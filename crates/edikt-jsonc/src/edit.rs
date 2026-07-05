//! Format-preserving edits via rowan structural-sharing splice.
//!
//! An edit resolves the target *value node* in the CST and swaps it for a fresh
//! subtree; rowan shares every untouched green node, so serialization stays
//! byte-identical everywhere except the value we replaced. The replacement's own
//! bytes are compact JSON - we format what we insert, never what we didn't touch.
//!
//! Supports `set` (`=`, `|=`), `+=`/append, `del`, and new-key creation
//! (inserting a member into the deepest existing object, matching its style).
//! Not yet: creating through an array index, and iterate-in-assignment
//! (`.a[] = x`).

use crate::syntax::{Sk, SyntaxElement, SyntaxNode, SyntaxToken};
use crate::{Jsonc, parser, project};
use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval};
use rowan::{GreenNode, NodeOrToken};

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
        Expr::AddAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let current = doc
                .value_at(steps)
                .ok_or_else(|| EditError::new("path not found"))?;
            let whole = doc.to_value();
            let addend = eval_one(rhs, &whole)?;
            match (&current, &addend) {
                // Array + array -> format-preserving element insert.
                (Value::Array(_), Value::Array(items)) => doc.append(steps, items),
                // Everything else (number, string) -> compute and replace the node.
                _ => doc.set(steps, &add_values(&current, &addend)?),
            }
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
            "expected an assignment (`path = value`, `path |= expr`) or `del(path)`",
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

/// Compute `current + addend` using the evaluator's `+` semantics.
fn add_values(current: &Value, addend: &Value) -> Result<Value, EditError> {
    let expr = Expr::Binary(
        BinOp::Add,
        Box::new(Expr::Path(Vec::new())),
        Box::new(Expr::Literal(addend.clone())),
    );
    eval_one(&expr, current)
}

/// The original source text of each value node selected by `path`, in document
/// order (aligned with the evaluator). A structural result is returned as its
/// exact bytes - comments, layout, trailing commas - not a re-serialization.
pub fn source_slice(root: &SyntaxNode, path: &[Step]) -> Vec<String> {
    let Some(top) = root.children().find(|n| n.kind() == Sk::Value) else {
        return Vec::new();
    };
    let mut current = vec![top];
    for step in path {
        let mut next = Vec::new();
        for node in &current {
            match step {
                Step::Iterate => next.extend(iterate_values(node)),
                _ => next.extend(step_into(node, step)),
            }
        }
        current = next;
    }
    current.iter().map(|n| n.text().to_string()).collect()
}

/// The element value nodes of an array, or the member value nodes of an object.
fn iterate_values(value_node: &SyntaxNode) -> Vec<SyntaxNode> {
    if let Some(array) = value_node.children().find(|n| n.kind() == Sk::Array) {
        array.children().filter(|n| n.kind() == Sk::Value).collect()
    } else if let Some(object) = value_node.children().find(|n| n.kind() == Sk::Object) {
        object
            .children()
            .filter(|n| n.kind() == Sk::Member)
            .filter_map(|m| m.children().find(|n| n.kind() == Sk::Value))
            .collect()
    } else {
        Vec::new()
    }
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
        // A comment is not a navigable value node.
        Step::Comment(_) => None,
    }
}

/// Find the member with `key` inside an object node.
pub(crate) fn find_member(object: &SyntaxNode, key: &str) -> Option<SyntaxNode> {
    object
        .children()
        .filter(|n| n.kind() == Sk::Member)
        .find(|member| {
            member
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == Sk::Str)
                .map(|t| project::unescape(t.text()))
                .as_deref()
                == Some(key)
        })
}

/// Detach a member together with the whitespace before it (its line indent), so
/// the whole line disappears. The member owns its trailing comma, so that goes
/// with it. Operates on a `clone_for_update` tree.
pub(crate) fn delete_member(member: &SyntaxNode) {
    let leading_ws = leading_ws_of(&member.prev_sibling_or_token());
    member.detach();
    if let Some(ws) = leading_ws {
        ws.detach();
    }
}

/// Detach an array element with exactly one comma+whitespace separator, so no
/// dangling comma or doubled space is left. Operates on a `clone_for_update` tree.
pub(crate) fn delete_element(value: &SyntaxNode) {
    match value.next_sibling_or_token() {
        // Not the last element: drop this value + the following comma + its ws.
        Some(NodeOrToken::Token(comma)) if comma.kind() == Sk::Comma => {
            let trailing_ws = leading_ws_of(&comma.next_sibling_or_token());
            value.detach();
            comma.detach();
            if let Some(ws) = trailing_ws {
                ws.detach();
            }
        }
        // Last element: drop the preceding whitespace and comma instead.
        _ => {
            let leading_ws = leading_ws_of(&value.prev_sibling_or_token());
            let preceding_comma = match &leading_ws {
                Some(ws) => ws.prev_sibling_or_token(),
                None => value.prev_sibling_or_token(),
            };
            value.detach();
            if let Some(ws) = leading_ws {
                ws.detach();
            }
            match preceding_comma {
                Some(NodeOrToken::Token(comma)) if comma.kind() == Sk::Comma => comma.detach(),
                _ => {}
            }
        }
    }
}

/// If the element is a whitespace token, return it.
fn leading_ws_of(elem: &Option<SyntaxElement>) -> Option<SyntaxToken> {
    match elem {
        Some(NodeOrToken::Token(t)) if t.kind() == Sk::Ws => Some(t.clone()),
        _ => None,
    }
}

/// Insert `items` at the end of an array's source text, matching its existing
/// style (single-line `, x`; multi-line newline + indent + trailing comma). All
/// original bytes are preserved - only the new elements are added.
pub(crate) fn insert_into_array(orig: &str, items: &[Value]) -> String {
    let elems: Vec<String> = items.iter().map(|v| v.to_json()).collect();
    insert_elements(orig, &elems)
}

/// Insert `"key": value` as a new member of an object's source text.
pub(crate) fn insert_into_object(orig: &str, key: &str, value: &Value) -> String {
    let member = format!("{}: {}", json_string(key), value.to_json());
    insert_elements(orig, &[member])
}

/// Insert `new_elems` before the closing bracket of a `[...]`/`{...}` text,
/// matching its style (single-line `, x`; multi-line newline + indent + trailing
/// comma). Every original byte is preserved.
fn insert_elements(orig: &str, new_elems: &[String]) -> String {
    // Brackets are single ASCII bytes at each end.
    let open = &orig[..1];
    let close = &orig[orig.len() - 1..];
    let inner = &orig[1..orig.len() - 1];
    let body = inner.trim_end();
    let close_ws = &inner[body.len()..];

    if body.is_empty() {
        return format!("{open}{}{close}", new_elems.join(", "));
    }

    let mut out = String::from(open);
    out.push_str(body);
    let has_trailing_comma = body.ends_with(',');
    if inner.contains('\n') {
        let indent = detect_indent(inner);
        if !has_trailing_comma {
            out.push(',');
        }
        for elem in new_elems {
            out.push('\n');
            out.push_str(&indent);
            out.push_str(elem);
            out.push(',');
        }
    } else if has_trailing_comma {
        for elem in new_elems {
            out.push(' ');
            out.push_str(elem);
            out.push(',');
        }
    } else {
        for elem in new_elems {
            out.push_str(", ");
            out.push_str(elem);
        }
    }
    out.push_str(close_ws);
    out.push_str(close);
    out
}

/// The indentation of the first non-blank line inside a container body.
fn detect_indent(inner: &str) -> String {
    for line in inner.split('\n') {
        let trimmed = line.trim_start();
        if !trimmed.is_empty() {
            return line[..line.len() - trimmed.len()].to_string();
        }
    }
    "  ".to_string()
}

/// A JSON-quoted, escaped object key.
fn json_string(s: &str) -> String {
    Value::Str(s.to_string()).to_json()
}

/// Parse array source text and return its `Array` node's green subtree.
pub(crate) fn array_green_from_text(text: &str) -> GreenNode {
    node_green_from_text(text, Sk::Array)
}

/// Parse object source text and return its `Object` node's green subtree.
pub(crate) fn object_green_from_text(text: &str) -> GreenNode {
    node_green_from_text(text, Sk::Object)
}

fn node_green_from_text(text: &str, kind: Sk) -> GreenNode {
    let root = SyntaxNode::new_root(parser::build(text));
    root.descendants()
        .find(|n| n.kind() == kind)
        .expect("reparsed text contains the expected node")
        .green()
        .into_owned()
}

/// Walk `path` from `top` as far as it resolves; return the deepest reached value
/// node and the still-unresolved steps (empty if the whole path resolved).
pub(crate) fn walk_partial(top: SyntaxNode, path: &[Step]) -> (SyntaxNode, &[Step]) {
    let mut current = top;
    for (i, step) in path.iter().enumerate() {
        match step_into(&current, step) {
            Some(next) => current = next,
            None => return (current, &path[i..]),
        }
    }
    (current, &[])
}

/// Build a nested value for the not-yet-existing tail of a path (`[.b, .c]` with
/// value `v` becomes `{ "b": { "c": v } }`).
pub(crate) fn nest_value(steps: &[Step], value: &Value) -> Result<Value, EditError> {
    match steps.split_first() {
        None => Ok(value.clone()),
        Some((Step::Field(k), rest)) => {
            Ok(Value::Object(vec![(k.clone(), nest_value(rest, value)?)]))
        }
        Some((Step::Index(_), _)) => Err(EditError::new("cannot create array elements by index")),
        Some((Step::Iterate, _)) => Err(EditError::new("cannot create through `[]`")),
        Some((Step::Comment(_), _)) => Err(EditError::new(
            "editing comments (`#`) is not supported yet (planned for v0.2)",
        )),
    }
}

/// Build a `Value`-node green subtree by rendering `value` as compact JSON and
/// reparsing it - the inserted bytes are formatted; surrounding layout is not.
pub(crate) fn value_green(value: &Value) -> GreenNode {
    let json = value.to_json();
    let root = SyntaxNode::new_root(parser::build(&json));
    let value_node = root
        .children()
        .find(|n| n.kind() == Sk::Value)
        .expect("compact JSON always has a top-level value");
    value_node.green().into_owned()
}
