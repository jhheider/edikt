//! The span-tree edit primitives on [`Yaml`]: set (replacing a value in
//! place, or creating a missing key), append, delete, and the recompose
//! after each splice. A collection written over a collection reaches here
//! as element edits, diffed by the shared driver (#117).

use super::extent::{newline, trim_newline};
use super::flow;
use super::resolve::{Resolved, in_flow, nest, resolve, slot_of};
use super::{Strictness, miss};
use crate::Yaml;
use crate::block::BlockScalar;
use crate::compose::{Node, NodeKind, node_to_value};
use crate::layout::{Indent, column, flow_text, inline_text, is_flow};
use crate::scalar::{QuoteStyle, emit_scalar_styled, kept_properties, split_properties};
use crate::splice::{Slot, append_items, block_replace, new_key, scalar_to_block};
use edikt_core::{EditError, Step, Value, render_path};
use std::ops::Range;

impl Yaml {
    /// The root node of document `idx`.
    pub(super) fn root(&self, idx: usize) -> &Node {
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
                // A `[k: v]` pair inside a flow sequence has no brackets to
                // grow inside, and block lines can't go there (#111).
                let at = path.len() - rest.len() - 1;
                if !is_flow(&self.source, parent)
                    && in_flow(&self.source, self.root(idx), &path[..at])
                {
                    return Err(EditError::new(format!(
                        "cannot add a key to the single-pair mapping at {} in place",
                        render_path(&path[..at])
                    )));
                }
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
    /// sits (see [`crate::splice`]).
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
            // A `[k: v]` pair's span runs on past its comma; stop at its value.
            (
                node.span.start..flow::content_end(source, node),
                format!("{}{props}{text}", null_lead(source, node, true)?),
            )
        } else if scalar && let Some(text) = inline_text(value) {
            // A scalar takes the old body's quote style; an empty collection
            // is `[]`/`{}`.
            let text = if collection {
                text?
            } else {
                emit_scalar_styled(value, QuoteStyle::of(body), false, body)?
            };
            (
                node.span.clone(),
                format!("{}{props}{text}", null_lead(source, node, false)?),
            )
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
            // Folded to the file's prose width first (#108), if it has
            // one, then one line per line of text, should the folded
            // spelling not read back.
            let width = block
                .indent(parent, offset)
                .and_then(|indent| crate::fold::width(&before, &self.docs, block, indent));
            let widths = [width, None];
            let tries = if width.is_some() {
                &widths[..]
            } else {
                &widths[1..]
            };
            for &width in tries {
                if let Some((range, text)) =
                    block.respell(&before, s, parent, offset, newline(&before), width)
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
    pub(super) fn restore(&mut self, source: String) -> Result<(), EditError> {
        let end = self.source.len();
        self.commit(0..end, &source)
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

    /// Replace `range` with `text`, then recompose every document. Atomic: if
    /// the result no longer parses, nothing changes and the error surfaces.
    /// Recomposing the whole stream keeps later documents' byte marks correct
    /// after an edit shifts offsets.
    pub(super) fn commit(&mut self, range: Range<usize>, text: &str) -> Result<(), EditError> {
        let mut new_source = self.source.clone();
        new_source.replace_range(range, text);
        let composed = crate::compose::compose_all(&new_source)
            .map_err(|e| EditError::new(format!("edit produced invalid YAML: {e}")))?;
        // Removing or replacing an anchored node would silently turn its
        // aliases into nulls (#111). A file that already had a dangling
        // alias is the file's business.
        if let Some(name) = &composed.dangling
            && crate::compose::compose_all(&self.source).is_ok_and(|d| d.dangling.is_none())
        {
            return Err(EditError::new(format!(
                "cannot edit this in place: it would remove anchor &{name}, which *{name} still names"
            )));
        }
        let mut docs = composed.into_vec();
        if docs.is_empty() {
            docs.push(crate::compose::null_node());
        }
        self.source = new_source;
        self.docs = docs;
        Ok(())
    }
}

/// What a value written over `node` needs in front of it when `node` is an
/// implicit null, which has no bytes (#111). libyaml marks one right after
/// its `:` or `-`, where a space keeps the value off the indicator (`a:1`
/// would be a key), or, for a bare key in a flow mapping (`{a, b: 1}`), right
/// after the key, which needs its `: `. A `? key` with no value in block
/// context is refused; its null sits at the start of the next line.
fn null_lead(source: &str, node: &Node, flow: bool) -> Result<&'static str, EditError> {
    if !node.span.is_empty() || !matches!(node.kind, NodeKind::Scalar(Value::Null)) {
        return Ok("");
    }
    match source[..node.span.start].bytes().last() {
        Some(b':' | b'-') => Ok(" "),
        Some(b' ' | b'\t') => Ok(""),
        _ if flow => Ok(": "),
        _ => Err(EditError::new(
            "cannot set the value of a `?` key that has none in place yet",
        )),
    }
}
