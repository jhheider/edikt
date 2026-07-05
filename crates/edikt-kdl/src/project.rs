//! Project a KDL document onto the [`Value`] model, per the convention in
//! CLAUDE.md: nodes group by name (repeats -> arrays); a node is its children
//! object, its lone argument, its argument array, or - when arguments mix with
//! props/children - an object with the arguments under the reserved key `"-"`.

use edikt_core::Value;
use kdl::{KdlDocument, KdlNode, KdlValue};

/// The reserved key holding a mixed node's positional arguments.
pub(crate) const ARGS_KEY: &str = "-";

pub(crate) fn doc_to_value(doc: &KdlDocument) -> Value {
    Value::Object(
        group_nodes(doc)
            .into_iter()
            .map(|(name, nodes)| {
                let v = if nodes.len() == 1 {
                    node_to_value(nodes[0])
                } else {
                    Value::Array(nodes.iter().map(|n| node_to_value(n)).collect())
                };
                (name, v)
            })
            .collect(),
    )
}

/// Nodes grouped by name in first-appearance order (repeats stay in document
/// order). This grouping *is* the object shape of a document.
pub(crate) fn group_nodes(doc: &KdlDocument) -> Vec<(String, Vec<&KdlNode>)> {
    let mut groups: Vec<(String, Vec<&KdlNode>)> = Vec::new();
    for node in doc.nodes() {
        let name = node.name().value().to_string();
        match groups.iter_mut().find(|(n, _)| *n == name) {
            Some((_, v)) => v.push(node),
            None => groups.push((name, vec![node])),
        }
    }
    groups
}

pub(crate) fn node_to_value(node: &KdlNode) -> Value {
    let args: Vec<&KdlValue> = node
        .entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect();
    let props: Vec<(String, Value)> = node
        .entries()
        .iter()
        .filter_map(|e| e.name().map(|n| (n.value().to_string(), scalar(e.value()))))
        .collect();
    let children = node.children();

    if props.is_empty() && children.is_none() {
        return match args.len() {
            0 => Value::Null,
            1 => scalar(args[0]),
            _ => Value::Array(args.into_iter().map(scalar).collect()),
        };
    }
    let mut entries: Vec<(String, Value)> = Vec::new();
    match args.len() {
        0 => {}
        1 => entries.push((ARGS_KEY.into(), scalar(args[0]))),
        _ => entries.push((
            ARGS_KEY.into(),
            Value::Array(args.into_iter().map(scalar).collect()),
        )),
    }
    entries.extend(props);
    if let Some(doc) = children {
        let Value::Object(kids) = doc_to_value(doc) else {
            unreachable!("a document projects to an object");
        };
        entries.extend(kids);
    }
    Value::Object(entries)
}

/// One KDL scalar to the value model. Integers wider than i64 (KDL holds
/// i128) degrade to floats, like JSON parsers do.
pub(crate) fn scalar(v: &KdlValue) -> Value {
    match v {
        KdlValue::String(s) => Value::Str(s.clone()),
        KdlValue::Integer(i) => match i64::try_from(*i) {
            Ok(i) => Value::Int(i),
            Err(_) => Value::Float(*i as f64),
        },
        KdlValue::Float(f) => Value::Float(*f),
        KdlValue::Bool(b) => Value::Bool(*b),
        KdlValue::Null => Value::Null,
    }
}

/// One value-model scalar to KDL (containers are structural, handled by the
/// node builders).
pub(crate) fn to_kdl_scalar(v: &Value) -> Option<KdlValue> {
    Some(match v {
        Value::Str(s) => KdlValue::String(s.clone()),
        Value::Int(i) => KdlValue::Integer(*i as i128),
        Value::Float(f) => KdlValue::Float(*f),
        Value::Bool(b) => KdlValue::Bool(*b),
        Value::Null => KdlValue::Null,
        Value::Array(_) | Value::Object(_) => return None,
    })
}
