//! Format-preserving edits and conversion emit over `kdl-rs`.
//!
//! Edits are surgical: set an argument or property in place (only its value
//! text changes), create a leaf node, delete a node/prop/argument, extend a
//! run of repeated nodes. Replacing a whole node body wholesale is refused
//! rather than reflowed; same policy as YAML.

use crate::Kdl;
use crate::project::{self, ARGS_KEY};
use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval};
use kdl::{FormatConfig, KdlDocument, KdlEntry, KdlEntryFormat, KdlNode, KdlValue};

pub fn apply(doc: &mut Kdl, expr: &Expr) -> Result<(), EditError> {
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
            let sum = eval_one(
                &Expr::Binary(
                    BinOp::Add,
                    Box::new(Expr::Path(Vec::new())),
                    Box::new(Expr::Literal(addend)),
                ),
                &current,
            )?;
            doc.set(steps, &sum)
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

// --- set ------------------------------------------------------------------

pub(crate) fn set_in_doc(
    doc: &mut KdlDocument,
    path: &[Step],
    value: &Value,
    depth: usize,
) -> Result<(), EditError> {
    let Some(Step::Field(name)) = path.first() else {
        return Err(EditError::new(
            "KDL assignment paths start with a node name",
        ));
    };
    let occurrences: Vec<usize> = (0..doc.nodes().len())
        .filter(|&i| doc.nodes()[i].name().value() == name)
        .collect();

    match occurrences.len() {
        0 => {
            if path.len() > 1 {
                return Err(EditError::new(format!("no node `{name}`")));
            }
            insert_nodes(doc, value_to_nodes(name, value)?, doc.nodes().len(), depth);
            Ok(())
        }
        1 => {
            let node = &mut doc.nodes_mut()[occurrences[0]];
            if path.len() == 1 {
                set_node(node, value)
            } else {
                set_in_node(node, &path[1..], value, depth + 1)
            }
        }
        n => match path.get(1) {
            Some(Step::Index(i)) => {
                let idx = resolve_index(*i, n)
                    .ok_or_else(|| EditError::new(format!("`{name}` index out of range")))?;
                let node = &mut doc.nodes_mut()[occurrences[idx]];
                if path.len() == 2 {
                    set_node(node, value)
                } else {
                    set_in_node(node, &path[2..], value, depth + 1)
                }
            }
            None => extend_repeated(doc, name, &occurrences, value, depth),
            _ => Err(EditError::new(format!(
                "`{name}` is repeated: address one occurrence (.{name}[i])"
            ))),
        },
    }
}

fn set_in_node(
    node: &mut KdlNode,
    path: &[Step],
    value: &Value,
    depth: usize,
) -> Result<(), EditError> {
    match &path[0] {
        Step::Field(k) if k == ARGS_KEY => match path.get(1) {
            None => replace_args(node, value),
            Some(Step::Index(i)) if path.len() == 2 => set_arg(node, *i, value),
            _ => Err(EditError::new("arguments are scalars; cannot go deeper")),
        },
        Step::Field(k) => {
            if node.entry(k.as_str()).is_some() {
                if path.len() > 1 {
                    return Err(EditError::new(format!("property `{k}` is a scalar")));
                }
                let kv = scalar_only(value)?;
                let entry = node.entry_mut(k.as_str()).unwrap();
                set_entry_value(entry, kv);
                return Ok(());
            }
            if node.children().is_some() {
                set_in_doc(node.children_mut().as_mut().unwrap(), path, value, depth)
            } else if node.entries().is_empty() {
                // A bare node grows a children block.
                set_in_doc(node.ensure_children(), path, value, depth)
            } else {
                Err(EditError::new(format!(
                    "`{}` holds arguments, not children; cannot create `{k}` inside it",
                    node.name().value()
                )))
            }
        }
        Step::Index(i) => set_arg(node, *i, value),
        Step::Iterate => Err(EditError::new(
            "`[]` in assignment paths is not supported for KDL",
        )),
        Step::Comment(_) => Err(EditError::new(
            "editing comments (`#`) is not supported yet (planned for v0.2)",
        )),
    }
}

/// Set a whole node from a value. Scalar/argument-row nodes update in place;
/// replacing a container body wholesale is refused rather than reflowed.
fn set_node(node: &mut KdlNode, value: &Value) -> Result<(), EditError> {
    let is_leaf = node.children().is_none() && node.entries().iter().all(|e| e.name().is_none());
    if !is_leaf {
        return Err(EditError::new(format!(
            "replacing the whole body of `{}` is not supported; set its keys instead",
            node.name().value()
        )));
    }
    replace_args(node, value)
}

/// Replace a node's positional arguments with a scalar or array of scalars.
/// If the new array starts with the current arguments, the extras append and
/// the existing entries keep their bytes (`+=` reduces to this).
fn replace_args(node: &mut KdlNode, value: &Value) -> Result<(), EditError> {
    let new: Vec<KdlValue> = match value {
        Value::Array(items) => items
            .iter()
            .map(scalar_only)
            .collect::<Result<Vec<_>, _>>()?,
        scalar => vec![scalar_only(scalar)?],
    };
    let current: Vec<usize> = (0..node.entries().len())
        .filter(|&i| node.entries()[i].name().is_none())
        .collect();
    let is_prefix = current.len() <= new.len()
        && current
            .iter()
            .zip(&new)
            .all(|(&i, nv)| node.entries()[i].value() == nv);
    if is_prefix {
        for nv in new.into_iter().skip(current.len()) {
            node.entries_mut().push(arg_entry(nv));
        }
        return Ok(());
    }
    if current.len() == new.len() {
        // Same shape: update each argument's value in place.
        for (&i, nv) in current.iter().zip(new) {
            set_entry_value(&mut node.entries_mut()[i], nv);
        }
        return Ok(());
    }
    // Different shape: rebuild the argument row (properties stay untouched).
    node.entries_mut().retain(|e| e.name().is_some());
    for nv in new {
        node.entries_mut().push(arg_entry(nv));
    }
    Ok(())
}

fn set_arg(node: &mut KdlNode, i: i64, value: &Value) -> Result<(), EditError> {
    let args: Vec<usize> = (0..node.entries().len())
        .filter(|&j| node.entries()[j].name().is_none())
        .collect();
    let idx = resolve_index(i, args.len())
        .ok_or_else(|| EditError::new("argument index out of range"))?;
    let kv = scalar_only(value)?;
    set_entry_value(&mut node.entries_mut()[args[idx]], kv);
    Ok(())
}

/// `.name = [bigger array]` on repeated nodes: if the existing occurrences
/// match the array's prefix, the extras append as new nodes (this is how `+=`
/// lands); anything else would rewrite untargeted nodes, so it is refused.
fn extend_repeated(
    doc: &mut KdlDocument,
    name: &str,
    occurrences: &[usize],
    value: &Value,
    depth: usize,
) -> Result<(), EditError> {
    let Value::Array(items) = value else {
        return Err(EditError::new(format!(
            "`{name}` is repeated: address one occurrence (.{name}[i])"
        )));
    };
    let matches_prefix = occurrences.len() <= items.len()
        && occurrences
            .iter()
            .zip(items)
            .all(|(&i, v)| project::node_to_value(&doc.nodes()[i]) == *v);
    if !matches_prefix {
        return Err(EditError::new(format!(
            "replacing the repeated `{name}` nodes wholesale is not supported; \
             append with `+=` or address one occurrence (.{name}[i])"
        )));
    }
    let mut new_nodes = Vec::new();
    for v in &items[occurrences.len()..] {
        new_nodes.extend(value_to_nodes(name, v)?);
    }
    let at = occurrences.last().unwrap() + 1;
    insert_nodes(doc, new_nodes, at, depth);
    Ok(())
}

// --- delete ----------------------------------------------------------------

pub(crate) fn delete_in_doc(doc: &mut KdlDocument, path: &[Step]) -> Result<(), EditError> {
    let Some(Step::Field(name)) = path.first() else {
        return Err(EditError::new("KDL paths start with a node name"));
    };
    let occurrences: Vec<usize> = (0..doc.nodes().len())
        .filter(|&i| doc.nodes()[i].name().value() == name)
        .collect();
    if occurrences.is_empty() {
        return Ok(()); // jq semantics: deleting a miss is a no-op
    }
    if path.len() == 1 {
        doc.nodes_mut().retain(|n| n.name().value() != name);
        return Ok(());
    }
    if let Step::Index(i) = path[1] {
        let Some(idx) = resolve_index(i, occurrences.len()) else {
            return Ok(());
        };
        if path.len() == 2 {
            if occurrences.len() > 1 {
                doc.nodes_mut().remove(occurrences[idx]);
            } else {
                // A single node indexed: the index addresses its arguments.
                return delete_in_node(&mut doc.nodes_mut()[occurrences[idx]], &path[1..]);
            }
            return Ok(());
        }
        return delete_in_node(&mut doc.nodes_mut()[occurrences[idx]], &path[2..]);
    }
    if occurrences.len() > 1 {
        return Err(EditError::new(format!(
            "`{name}` is repeated: address one occurrence (.{name}[i])"
        )));
    }
    delete_in_node(&mut doc.nodes_mut()[occurrences[0]], &path[1..])
}

fn delete_in_node(node: &mut KdlNode, path: &[Step]) -> Result<(), EditError> {
    match &path[0] {
        Step::Field(k) if k == ARGS_KEY => match path.get(1) {
            None => {
                node.entries_mut().retain(|e| e.name().is_some());
                Ok(())
            }
            Some(Step::Index(i)) if path.len() == 2 => {
                remove_arg(node, *i);
                Ok(())
            }
            _ => Err(EditError::new("arguments are scalars; cannot go deeper")),
        },
        Step::Field(k) => {
            if node.entry(k.as_str()).is_some() {
                if path.len() > 1 {
                    return Err(EditError::new(format!("property `{k}` is a scalar")));
                }
                node.entries_mut()
                    .retain(|e| e.name().map(|n| n.value()) != Some(k.as_str()));
                return Ok(());
            }
            match node.children_mut() {
                Some(children) => delete_in_doc(children, path),
                None => Ok(()), // nothing there: no-op
            }
        }
        Step::Index(i) => {
            remove_arg(node, *i);
            Ok(())
        }
        Step::Iterate => Err(EditError::new("del(.[]) is not supported for KDL")),
        Step::Comment(_) => Err(EditError::new(
            "deleting comments (`#`) is not supported yet (planned for v0.2)",
        )),
    }
}

fn remove_arg(node: &mut KdlNode, i: i64) {
    let args: Vec<usize> = (0..node.entries().len())
        .filter(|&j| node.entries()[j].name().is_none())
        .collect();
    if let Some(idx) = resolve_index(i, args.len()) {
        node.entries_mut().remove(args[idx]);
    }
}

// --- building --------------------------------------------------------------

/// A value as fresh node(s) named `name`, inverting the projection:
///
/// - an array whose elements are **all scalars** -> one node carrying them as
///   positional arguments (`["a","b"]` -> `name "a" "b"`);
/// - an array with any **object or array** element -> one node per element (a
///   run of repeated nodes), each object->node, each inner array->that node's
///   arguments, each scalar->a one-argument node;
/// - an object -> one node (args under `"-"`, then props, then children);
/// - a scalar -> a one-argument node; `null` -> a bare node.
///
/// A triply-nested array (an array element that is itself an array of arrays)
/// has no KDL spelling and errors cleanly.
pub(crate) fn value_to_nodes(name: &str, value: &Value) -> Result<Vec<KdlNode>, EditError> {
    if let Value::Array(items) = value
        && items
            .iter()
            .any(|v| matches!(v, Value::Object(_) | Value::Array(_)))
    {
        let mut out = Vec::new();
        for item in items {
            out.push(one_node(name, item)?);
        }
        return Ok(out);
    }
    Ok(vec![one_node(name, value)?])
}

/// Build exactly one node named `name` from `value`.
fn one_node(name: &str, value: &Value) -> Result<KdlNode, EditError> {
    let mut node = KdlNode::new(name);
    match value {
        Value::Object(entries) => {
            for (k, v) in entries {
                if k == ARGS_KEY {
                    match v {
                        Value::Array(args) => {
                            for a in args {
                                node.entries_mut().push(arg_entry(scalar_only(a)?));
                            }
                        }
                        scalar => node.entries_mut().push(arg_entry(scalar_only(scalar)?)),
                    }
                } else {
                    for child in value_to_nodes(k, v)? {
                        node.ensure_children().nodes_mut().push(child);
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                node.entries_mut().push(arg_entry(scalar_only(item)?));
            }
        }
        Value::Null => {} // a bare node
        scalar => node.entries_mut().push(arg_entry(scalar_only(scalar)?)),
    }
    Ok(node)
}

/// Insert freshly built nodes at position `at`, formatted to sit at `depth`
/// (indentation matches the level; internals autoformat below it).
fn insert_nodes(doc: &mut KdlDocument, nodes: Vec<KdlNode>, at: usize, depth: usize) {
    let config = FormatConfig::builder().indent_level(depth).build();
    for (i, mut node) in nodes.into_iter().enumerate() {
        node.autoformat_config(&config);
        doc.nodes_mut().insert(at + i, node);
    }
}

// --- emit -------------------------------------------------------------------

/// Emit a value as a fresh KDL document (autoformatted; there is no layout to
/// preserve on this path). Returns text and warnings (none: KDL holds every
/// feature).
pub fn emit(value: &Value) -> Result<(String, Vec<String>), EditError> {
    Ok((build_document(value)?.to_string(), Vec::new()))
}

pub(crate) fn build_document(value: &Value) -> Result<KdlDocument, EditError> {
    let Value::Object(entries) = value else {
        return Err(EditError::new(
            "KDL output requires a top-level object (a document is a list of nodes)",
        ));
    };
    let mut doc = KdlDocument::new();
    for (k, v) in entries {
        for node in value_to_nodes(k, v)? {
            doc.nodes_mut().push(node);
        }
    }
    doc.autoformat();
    Ok(doc)
}

// --- shared helpers ----------------------------------------------------------

fn scalar_only(v: &Value) -> Result<KdlValue, EditError> {
    project::to_kdl_scalar(v).ok_or_else(|| {
        EditError::new(format!(
            "a KDL argument/property holds a scalar, not {}",
            v.type_name()
        ))
    })
}

/// Update an entry's value *and* its stored text repr (kdl-rs renders the
/// repr, so `set_value` alone would print the stale bytes).
fn set_entry_value(entry: &mut KdlEntry, value: KdlValue) {
    let repr = value.to_string();
    entry.set_value(value);
    if let Some(f) = entry.format_mut() {
        f.value_repr = repr;
    }
}

/// A fresh positional-argument entry with its own leading space.
fn arg_entry(value: KdlValue) -> KdlEntry {
    let mut e = KdlEntry::new(value.clone());
    e.set_format(KdlEntryFormat {
        value_repr: value.to_string(),
        leading: " ".into(),
        ..Default::default()
    });
    e
}

fn resolve_index(i: i64, len: usize) -> Option<usize> {
    let idx = if i < 0 { len as i64 + i } else { i };
    (0..len as i64).contains(&idx).then_some(idx as usize)
}
