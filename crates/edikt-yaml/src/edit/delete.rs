//! Deleting from the span tree: one mapping entry or sequence item, or a
//! container emptied by a trailing `del(...[])`.
//!
//! A block element goes with its whole lines. An element of a flow
//! collection (`[...]`/`{...}`) goes with its separator, the collection
//! keeping its layout (see [`super::flow`]). Deleting a collection's only
//! element leaves it empty (`[]`/`{}`), never null, the way jq does.

use super::extent::{block_end, insert_end, trim_newline};
use super::flow;
use super::resolve::{Resolved, in_flow, resolve};
use crate::Yaml;
use crate::compose::{Node, NodeKind};
use crate::layout::{content_start, is_flow, line_start};
use crate::scalar::split_properties;
use edikt_core::{EditError, Step};
use std::ops::Range;

impl Yaml {
    /// Delete the mapping entry or sequence item at `path` in document `idx`.
    pub(crate) fn delete(&mut self, idx: usize, path: &[Step]) -> Result<(), EditError> {
        // Fan-out delete: resolve the iterate to concrete index/key paths and
        // splice each through the ordinary single-target machinery, back-to-
        // front so indices stay valid as the collection shrinks.
        if path.contains(&Step::Iterate) {
            // A **trailing** iterate (`del(.a[])`) empties the container it
            // names: rewrite the whole entry/container to its inline empty
            // spelling (`a: []` / `a: {}`), the jq-analogue of leaving `[]`.
            // Block-form items and the comments inside the emptied region go
            // with it. A nested iterate (`del(.a[].b)`) composes the per-item
            // deletes below instead.
            if let Some(Step::Iterate) = path.last() {
                return self.delete_within(idx, &path[..path.len() - 1]);
            }
            let whole = self.doc_value(idx);
            let paths = edikt_core::expand_delete_paths(path, &whole)?;
            for p in &paths {
                self.delete(idx, p)?;
            }
            return Ok(());
        }
        let Some((last, parent_path)) = path.split_last() else {
            return Err(EditError::new("cannot delete the whole document"));
        };
        // jq semantics (and the other formats): deleting a missing key, an
        // out-of-range index, or through an absent parent is a **no-op**.
        let Resolved::Found(parent) = resolve(self.root(idx), parent_path) else {
            return Ok(());
        };
        let (pos, count) = match (last, &parent.kind) {
            (Step::Field(k), NodeKind::Mapping(entries)) => {
                let Some(pos) = entries.iter().position(|e| &e.key == k) else {
                    return Ok(());
                };
                (pos, entries.len())
            }
            (Step::Index(i), NodeKind::Sequence(items)) => {
                let Some(pos) = edikt_core::resolve_index(*i, items.len()) else {
                    return Ok(());
                };
                (pos, items.len())
            }
            _ => return Ok(()),
        };
        let source = &self.source;
        let (range, text) = if is_flow(source, parent) {
            flow::delete(source, parent, pos)?
        } else if count == 1 {
            // The last element out leaves the collection empty, not null.
            return self.delete_within(idx, parent_path);
        } else {
            (block_delete(source, parent, pos)?, String::new())
        };
        self.commit(range, &text)
    }

    /// Empty the container at `prefix`, rewriting it to its inline empty
    /// spelling (`[]`/`{}`), like jq leaves `del(.a[])`. A mapping value
    /// keeps its key (and its anchor or tag); a sequence item keeps its
    /// dash. A missing or non-container target is a no-op, matching delete.
    fn delete_within(&mut self, idx: usize, prefix: &[Step]) -> Result<(), EditError> {
        let source = &self.source;
        let root = self.root(idx);
        let Resolved::Found(node) = resolve(root, prefix) else {
            return Ok(());
        };
        let empty = match node.kind {
            NodeKind::Sequence(_) => "[]",
            NodeKind::Mapping(_) => "{}",
            NodeKind::Scalar(_) => return Ok(()),
        };
        let end = end_of(source, node, in_flow(source, root, prefix));
        let (range, text) = match prefix.split_last() {
            // The root container (`del(.[])`): the whole document's bytes.
            None => (line_start(source, node.span.start)..end, empty.to_string()),
            Some((Step::Field(k), rest)) => {
                let Resolved::Found(Node {
                    kind: NodeKind::Mapping(entries),
                    ..
                }) = resolve(root, rest)
                else {
                    return Ok(());
                };
                let Some(entry) = entries.iter().find(|e| &e.key == k) else {
                    return Ok(());
                };
                let key = &source[entry.key_span.clone()];
                let props = split_properties(&source[node.span.clone()]).0.trim_end();
                let text = if props.is_empty() {
                    format!("{key}: {empty}")
                } else {
                    format!("{key}: {props} {empty}")
                };
                (entry.key_span.start..end, text)
            }
            // A sequence item: its content, after the dash and any anchor.
            Some(_) => (content_start(source, node)..end, empty.to_string()),
        };
        self.commit(range, &text)
    }
}

/// Where the container `node` ends: its closing bracket for a flow
/// collection, the end of its single pair for a `[k: v]` mapping inside one,
/// else the end of its last content line (a block scalar's trailing blank
/// lines stay, as separators).
fn end_of(source: &str, node: &Node, inside_flow: bool) -> usize {
    if is_flow(source, node) || inside_flow {
        flow::content_end(source, node)
    } else {
        trim_newline(source, insert_end(source, node))
    }
}

/// The bytes that delete element `pos` of the block collection `parent`,
/// which has another element to keep: its whole lines, or, for the first
/// element of a compact `- key: v` / `- - x` item (sharing its line with the
/// item's dash), everything up to the next element, which moves up onto
/// the dash line.
fn block_delete(source: &str, parent: &Node, pos: usize) -> Result<Range<usize>, EditError> {
    let (start, value, next) = match &parent.kind {
        NodeKind::Mapping(entries) => (
            entries[pos].key_span.start,
            &entries[pos].value,
            entries.get(pos + 1).map(|e| e.key_span.start),
        ),
        NodeKind::Sequence(items) => (
            dash_of(source, &items[pos]),
            &items[pos],
            items.get(pos + 1).map(|n| dash_of(source, n)),
        ),
        NodeKind::Scalar(_) => return Err(EditError::new("cannot delete from a scalar")),
    };
    let end = block_end(source, value);
    let line = line_start(source, start);
    let prefix = &source[line..start];
    let compact = prefix.contains('-') && prefix.bytes().all(|b| matches!(b, b' ' | b'-'));
    match next {
        Some(next) if compact => {
            if source[end.min(next)..next].contains('#') {
                return Err(EditError::new(
                    "cannot delete the first entry of a compact `- ` item without \
                     dropping the comments after it",
                ));
            }
            Ok(start..next)
        }
        _ => Ok(line..end),
    }
}

/// Where a block sequence item begins: its dash, or its own start when no
/// dash precedes it on the line.
fn dash_of(source: &str, item: &Node) -> usize {
    let before = source[..item.span.start].trim_end_matches([' ', '\t']);
    if before.ends_with('-') {
        before.len() - 1
    } else {
        item.span.start
    }
}
