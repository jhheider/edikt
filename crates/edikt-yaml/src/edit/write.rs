//! The span-tree edit primitives on [`Yaml`]: set (replacing a value in
//! place, element by element when a collection keeps its shape, or creating
//! a missing key), append, delete, and the recompose after each splice.

use super::extent::{block_end, newline, trim_newline};
use super::resolve::{Resolved, in_flow, nest, resolve, slot_of};
use super::{Strictness, miss};
use crate::Yaml;
use crate::block::BlockScalar;
use crate::compose::{Node, NodeKind, collect_merge, node_to_value};
use crate::layout::{Indent, column, flow_text, inline_text, is_flow, line_start};
use crate::scalar::{QuoteStyle, emit_scalar_styled, kept_properties, split_properties};
use crate::splice::{Slot, append_items, block_replace, new_key, scalar_to_block};
use edikt_core::{EditError, Step, Value, render_path};
use std::ops::Range;

impl Yaml {
    /// The root node of document `idx`.
    fn root(&self, idx: usize) -> &Node {
        &self.docs[idx]
    }

    /// The value of document `idx` (for evaluating an edit's RHS/predicate
    /// against that document).
    pub(crate) fn doc_value(&self, idx: usize) -> Value {
        node_to_value(self.root(idx))
    }

    /// The value at `path` within document `idx`, if it resolves (`|=`/`+=`).
    pub(crate) fn value_at(&self, idx: usize, path: &[Step]) -> Option<Value> {
        match resolve(self.root(idx), path) {
            Resolved::Found(node) => Some(node_to_value(node)),
            _ => None,
        }
    }

    /// Set the value at `path` in document `idx`, or create it as a new leaf
    /// key, creating any missing mappings above it. A path that doesn't resolve is an error when strict, a no-op when
    /// lenient (one of many mapped documents).
    pub(crate) fn set(
        &mut self,
        idx: usize,
        path: &[Step],
        value: &Value,
        strict: Strictness,
    ) -> Result<(), EditError> {
        let (range, text) = match resolve(self.root(idx), path) {
            Resolved::Found(_) => return self.replace(idx, path, value, strict),
            Resolved::MissingField { parent, key, rest } => {
                // Missing levels below the key are created as nested mappings,
                // laid out like any other new collection (jhheider/edikt#85).
                let value = nest(rest, value);
                let indent = Indent::infer(&self.source, &self.docs);
                new_key(&self.source, parent, &key, &value, indent)?
            }
            Resolved::Uncreatable => match strict {
                Strictness::Strict => {
                    return Err(EditError::new(format!(
                        "cannot create array elements by index: {}",
                        render_path(path)
                    )));
                }
                Strictness::Lenient => return Ok(()),
            },
            Resolved::NotFound => return miss(strict, path),
        };
        self.commit(range, &text)
    }

    /// Replace the existing node at `path` with `value`, laid out for where it
    /// sits (see [`crate::splice`]). A collection that already holds `value`
    /// is left alone, and one that `value` only changes or extends in place is
    /// edited element by element, so untouched elements keep their bytes.
    fn replace(
        &mut self,
        idx: usize,
        path: &[Step],
        value: &Value,
        strict: Strictness,
    ) -> Result<(), EditError> {
        let source = &self.source;
        let root = self.root(idx);
        let Resolved::Found(node) = resolve(root, path) else {
            return miss(strict, path);
        };
        let collection = matches!(value, Value::Array(_) | Value::Object(_));
        if collection && Value::identical(&node_to_value(node), value) {
            return Ok(());
        }
        if let Some(plan) = elementwise(source, node, value) {
            return self.apply_plan(idx, path, plan, strict);
        }
        let scalar = matches!(node.kind, NodeKind::Scalar(_));
        if let Some(block) = BlockScalar::of(source, node) {
            return self.replace_block(idx, path, &block, value, strict);
        }
        let token = &source[node.span.clone()];
        // A quoted or plain scalar wrapped over several lines can't be
        // replaced without reflowing the lines around it; refuse cleanly
        // rather than emit something that fails to re-parse.
        if scalar && token.contains('\n') {
            return Err(EditError::new(
                "cannot set a multi-line quoted or plain scalar in place yet",
            ));
        }
        // Replace the body, not its properties: an anchor may be named by an
        // alias elsewhere, and a tag is the file's (unless it is a core tag
        // that no longer fits).
        let (props, body) = split_properties(token);
        let props = kept_properties(props, value);
        let ctx_flow = in_flow(source, root, path);
        let (range, text) = if ctx_flow || is_flow(source, node) {
            let text = if collection {
                flow_text(value)?
            } else {
                emit_scalar_styled(value, QuoteStyle::of(body), ctx_flow, body)?
            };
            (node.span.clone(), format!("{props}{text}"))
        } else if scalar && let Some(text) = inline_text(value) {
            // A scalar takes the old body's quote style; an empty collection
            // is `[]`/`{}`.
            let text = if collection {
                text?
            } else {
                emit_scalar_styled(value, QuoteStyle::of(body), false, body)?
            };
            (node.span.clone(), format!("{props}{text}"))
        } else {
            let slot = slot_of(root, path);
            let indent = Indent::infer(source, &self.docs);
            if scalar {
                scalar_to_block(source, node, slot, &props, value, indent)?
            } else {
                block_replace(source, node, slot, value, indent)?
            }
        };
        self.commit(range, &text)
    }

    /// Replace the block scalar (`|`/`>`) at `path` with `value` (#89). A
    /// string stays a block scalar, respelled in place by
    /// [`BlockScalar::respell`]; it must read back as `value`, or the edit
    /// is undone and taken the other way. Anything else (and a string a
    /// block scalar can't hold) first collapses the block onto its header
    /// line as a plain placeholder, then replaces that one-line scalar the
    /// usual way, so it lands as it would over `key: old`.
    fn replace_block(
        &mut self,
        idx: usize,
        path: &[Step],
        block: &BlockScalar,
        value: &Value,
        strict: Strictness,
    ) -> Result<(), EditError> {
        let before = self.source.clone();
        if let Value::Str(s) = value {
            let root = self.root(idx);
            let parent = match slot_of(root, path) {
                Slot::Root => Some(0),
                Slot::Value { key: (start, _) } => Some(column(&before, start)),
                Slot::Item => {
                    // The dash precedes the node, anchor and tag included.
                    let at = match resolve(root, path) {
                        Resolved::Found(node) => node.span.start,
                        _ => block.header.start,
                    };
                    let dash = before[..at].trim_end_matches([' ', '\t']);
                    dash.ends_with('-').then(|| column(&before, dash.len() - 1))
                }
            };
            let offset = Indent::infer(&before, &self.docs).unit;
            if let Some((range, text)) = block.respell(&before, s, parent, offset, newline(&before))
                && self.commit(range, &text).is_ok()
            {
                if self
                    .value_at(idx, path)
                    .is_some_and(|v| Value::identical(&v, value))
                {
                    return Ok(());
                }
                self.restore(before.clone())?;
            }
        }
        let range = block.header.start..trim_newline(&before, block.content.end);
        let text = format!("~{}", block.header_rest(&before));
        self.commit(range, &text)?;
        match self.replace(idx, path, value, strict) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.restore(before)?;
                Err(e)
            }
        }
    }

    /// Put back `source`, a state this document held before (so it parses).
    fn restore(&mut self, source: String) -> Result<(), EditError> {
        let end = self.source.len();
        self.commit(0..end, &source)
    }

    /// Run an [`elementwise`] plan against the collection at `path`.
    fn apply_plan(
        &mut self,
        idx: usize,
        path: &[Step],
        plan: Plan,
        strict: Strictness,
    ) -> Result<(), EditError> {
        let child = |step: Step| [path, &[step]].concat();
        for (step, v) in plan.changes {
            self.set(idx, &child(step), &v, strict)?;
        }
        if !plan.append.is_empty() {
            self.append(idx, path, &plan.append, strict)?;
        }
        for (k, v) in plan.add {
            self.set(idx, &child(Step::Field(k)), &v, strict)?;
        }
        Ok(())
    }

    /// Append `items` to the sequence at `path` in document `idx`.
    pub(crate) fn append(
        &mut self,
        idx: usize,
        path: &[Step],
        items: &[Value],
        strict: Strictness,
    ) -> Result<(), EditError> {
        let (range, text) = match resolve(self.root(idx), path) {
            Resolved::Found(node) => match &node.kind {
                NodeKind::Sequence(_) if items.is_empty() => return Ok(()),
                NodeKind::Sequence(_) => {
                    let indent = Indent::infer(&self.source, &self.docs);
                    append_items(&self.source, node, items, indent)?
                }
                _ => {
                    return Err(EditError::new(format!(
                        "`+=` with an array needs a sequence at {}",
                        render_path(path)
                    )));
                }
            },
            _ => return miss(strict, path),
        };
        self.commit(range, &text)
    }

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
            // deletes below instead, matching YAML's block semantics.
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
        let range = match (last, &parent.kind) {
            (Step::Field(k), NodeKind::Mapping(entries)) => {
                let Some(entry) = entries.iter().find(|e| &e.key == k) else {
                    return Ok(());
                };
                line_start(&self.source, entry.key_span.start)
                    ..block_end(&self.source, &entry.value)
            }
            (Step::Index(i), NodeKind::Sequence(items)) => {
                let Some(idx) = edikt_core::resolve_index(*i, items.len()) else {
                    return Ok(());
                };
                let item = &items[idx];
                line_start(&self.source, item.span.start)..block_end(&self.source, item)
            }
            _ => return Ok(()),
        };
        self.commit(range, "")
    }

    /// Replace `range` with `text`, then recompose every document. Atomic: if
    /// the result no longer parses, nothing changes and the error surfaces.
    /// Recomposing the whole stream keeps later documents' byte marks correct
    /// after an edit shifts offsets.
    /// Replace `range` with `text`, then recompose every document. Atomic: if
    /// the result no longer parses, nothing changes and the error surfaces.
    /// Recomposing the whole stream keeps later documents' byte marks correct
    /// after an edit shifts offsets.
    /// The container emptied by a trailing `del(...[])`: rewrite the entry (or
    /// the root document) to its inline empty spelling, like jq leaves `[]`.
    /// A missing parent or non-container is a no-op, matching delete.
    fn delete_within(&mut self, idx: usize, prefix: &[Step]) -> Result<(), EditError> {
        let empty = |kind: &NodeKind| -> &'static str {
            match kind {
                NodeKind::Sequence(_) => "[]",
                _ => "{}",
            }
        };
        // block_end consumes the trailing newline; the empty rewrite keeps it
        // (all of a CRLF, not just its `\n`).
        let end_of = |n: &Node| trim_newline(&self.source, block_end(&self.source, n));
        match prefix.split_last() {
            // Root container (`del(.[])`): replace the whole document's bytes.
            None => {
                let Resolved::Found(root) = resolve(self.root(idx), &[]) else {
                    return Ok(());
                };
                let range = line_start(&self.source, root.span.start)..end_of(root);
                self.commit(range, empty(&root.kind))
            }
            Some((Step::Field(k), rest)) => {
                let Resolved::Found(map) = resolve(self.root(idx), rest) else {
                    return Ok(());
                };
                let NodeKind::Mapping(entries) = &map.kind else {
                    return Ok(());
                };
                let Some(entry) = entries.iter().find(|e| &e.key == k) else {
                    return Ok(());
                };
                let key = &self.source[entry.key_span.start..entry.key_span.end];
                let range = entry.key_span.start..end_of(&entry.value);
                let text = format!("{key}: {}", empty(&entry.value.kind));
                self.commit(range, &text)
            }
            _ => Ok(()),
        }
    }

    /// Replace `range` with `text`, then recompose every document. Atomic: if
    /// the result no longer parses, nothing changes and the error surfaces.
    /// Recomposing the whole stream keeps later documents' byte marks correct
    /// after an edit shifts offsets.
    fn commit(&mut self, range: Range<usize>, text: &str) -> Result<(), EditError> {
        let mut new_source = self.source.clone();
        new_source.replace_range(range, text);
        let mut docs = crate::compose::compose_all(&new_source)
            .map_err(|e| EditError::new(format!("edit produced invalid YAML: {e}")))?
            .into_vec();
        if docs.is_empty() {
            docs.push(crate::compose::null_node());
        }
        self.source = new_source;
        self.docs = docs;
        Ok(())
    }
}

/// A replacement applied element by element: the changed elements, the items
/// to append, and the keys to add.
pub(crate) struct Plan {
    pub(crate) changes: Vec<(Step, Value)>,
    pub(crate) append: Vec<Value>,
    pub(crate) add: Vec<(String, Value)>,
}

/// Plan `value` as element edits to the collection `node` when it keeps the
/// collection's shape: the same kind, every existing key in its order (or
/// every existing item), and anything new after them. Then only the elements
/// that change are touched, and the rest keep their bytes and comments; `.a
/// |= . + [x]` appends one line instead of rewriting the list. A flow
/// collection takes element edits only when nothing is added, so a growing
/// one is respelled whole. `None` means a wholesale replacement.
pub(crate) fn elementwise(source: &str, node: &Node, value: &Value) -> Option<Plan> {
    let mut plan = Plan {
        changes: Vec::new(),
        append: Vec::new(),
        add: Vec::new(),
    };
    match (&node.kind, value) {
        (NodeKind::Sequence(items), Value::Array(new)) if new.len() >= items.len() => {
            for (i, (old, new)) in items.iter().zip(new).enumerate() {
                if !Value::identical(&node_to_value(old), new) {
                    plan.changes.push((Step::Index(i as i64), new.clone()));
                }
            }
            plan.append = new[items.len()..].to_vec();
        }
        (NodeKind::Mapping(entries), Value::Object(new)) => {
            // Physical keys, in order. A merge (`<<`) supplies keys too, which
            // the value view lists after the explicit ones.
            let phys: Vec<_> = entries.iter().filter(|e| e.key != "<<").collect();
            if (1..phys.len()).any(|i| phys[..i].iter().any(|e| e.key == phys[i].key)) {
                return None;
            }
            let mut merged = Vec::new();
            for e in entries.iter().filter(|e| e.key == "<<") {
                collect_merge(&node_to_value(&e.value), &mut merged);
            }
            merged.retain(|(k, _)| !phys.iter().any(|e| &e.key == k));
            // Dropping a merged-in key can't be done element-wise.
            if merged
                .iter()
                .any(|(k, _)| !new.iter().any(|(nk, _)| nk == k))
            {
                return None;
            }
            let mut pos = 0;
            for (k, v) in new {
                if let Some(e) = phys.get(pos).filter(|e| &e.key == k) {
                    if !Value::identical(&node_to_value(&e.value), v) {
                        plan.changes.push((Step::Field(k.clone()), v.clone()));
                    }
                    pos += 1;
                } else if phys.iter().any(|e| &e.key == k) {
                    return None; // reordered
                } else if merged
                    .iter()
                    .any(|(mk, mv)| mk == k && Value::identical(mv, v))
                {
                    // Still supplied, unchanged, by the merge.
                } else if pos < phys.len() {
                    return None; // a new key ahead of existing ones
                } else {
                    plan.add.push((k.clone(), v.clone()));
                }
            }
            if pos < phys.len() {
                return None; // a key was removed
            }
        }
        _ => return None,
    }
    let grows = !plan.append.is_empty() || !plan.add.is_empty();
    (!(grows && is_flow(source, node))).then_some(plan)
}
