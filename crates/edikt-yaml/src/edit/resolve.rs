//! Walking an edit path over the span tree: the node it lands on, or the
//! mapping a missing key would be created in, and where that node sits.

use crate::compose::{Node, NodeKind};
use crate::scalar::split_properties;
use crate::splice::Slot;
use edikt_core::{Step, Value, normalize_index};

/// The result of walking a path over the span tree.
pub(crate) enum Resolved<'a, 'p> {
    /// The path landed on an existing node.
    Found(&'a Node),
    /// Field `key` is absent on this (existing) mapping: an insert point.
    /// `rest` is the rest of the path below it (empty for a leaf key), all
    /// fields; `=` creates those levels as nested mappings (jhheider/edikt#85).
    MissingField {
        parent: &'a Node,
        key: String,
        rest: &'p [Step],
    },
    /// A missing key with an index below it: `=` can't invent array elements.
    Uncreatable,
    /// The path does not resolve (bad step, missing intermediate, out of range).
    NotFound,
}

pub(crate) fn resolve<'a, 'p>(node: &'a Node, path: &'p [Step]) -> Resolved<'a, 'p> {
    let Some((step, rest)) = path.split_first() else {
        return Resolved::Found(node);
    };
    match step {
        Step::Field(k) => match &node.kind {
            NodeKind::Mapping(entries) => match entries.iter().find(|e| &e.key == k) {
                Some(e) => resolve(&e.value, rest),
                None if rest.iter().all(|s| matches!(s, Step::Field(_))) => {
                    Resolved::MissingField {
                        parent: node,
                        key: k.clone(),
                        rest,
                    }
                }
                None if rest.iter().any(|s| matches!(s, Step::Index(_))) => Resolved::Uncreatable,
                None => Resolved::NotFound,
            },
            _ => Resolved::NotFound,
        },
        Step::Index(i) => match &node.kind {
            NodeKind::Sequence(items) => {
                let idx = normalize_index(*i, items.len());
                match idx.and_then(|n| items.get(n)) {
                    Some(item) => resolve(item, rest),
                    None => Resolved::NotFound,
                }
            }
            _ => Resolved::NotFound,
        },
        Step::Iterate => Resolved::NotFound,
        // Comment edits (`#`) are a Phase-2 feature; resolve treats them as
        // absent so the set/delete paths fall through to their clean errors.
        Step::Comment(_) => Resolved::NotFound,
    }
}

/// Does the node at `path` sit inside a flow collection (`[...]`/`{...}`),
/// where a plain scalar ends at the first `,[]{}`?
pub(crate) fn in_flow(source: &str, root: &Node, path: &[Step]) -> bool {
    let mut node = root;
    for step in path {
        let (_, body) = split_properties(&source[node.span.clone()]);
        if body.starts_with(['[', '{']) {
            return true;
        }
        let next = match (step, &node.kind) {
            (Step::Field(k), NodeKind::Mapping(entries)) => {
                entries.iter().find(|e| &e.key == k).map(|e| &e.value)
            }
            (Step::Index(i), NodeKind::Sequence(items)) => {
                normalize_index(*i, items.len()).and_then(|n| items.get(n))
            }
            _ => None,
        };
        match next {
            Some(n) => node = n,
            None => return false,
        }
    }
    false
}

/// The nested mapping a missing path's tail names: `[.b, .c]` with `v` is
/// `{b: {c: v}}`. `rest` is all fields ([`resolve`] guarantees it).
pub(crate) fn nest(rest: &[Step], value: &Value) -> Value {
    rest.iter()
        .rev()
        .fold(value.clone(), |inner, step| match step {
            Step::Field(k) => Value::Object(vec![(k.clone(), inner)]),
            _ => inner,
        })
}

/// Where the node at `path` sits: the document root, a mapping value (with
/// its key's span), or a sequence item.
pub(crate) fn slot_of(root: &Node, path: &[Step]) -> Slot {
    let Some((last, parent)) = path.split_last() else {
        return Slot::Root;
    };
    if let (Step::Field(k), Resolved::Found(parent)) = (last, resolve(root, parent))
        && let NodeKind::Mapping(entries) = &parent.kind
        && let Some(e) = entries.iter().find(|e| &e.key == k)
    {
        return Slot::Value {
            key: (e.key_span.start, e.key_span.end),
        };
    }
    Slot::Item
}
