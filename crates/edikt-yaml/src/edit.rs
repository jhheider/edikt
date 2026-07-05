//! Format-preserving edits by byte splice over the span tree.
//!
//! An edit resolves the target node's byte range (from the marks composed in
//! [`crate::compose`]) and replaces exactly those bytes — every other byte of the
//! source is left verbatim, so comments, indentation, and layout survive
//! untouched. After each splice we recompose from the new source, so a later step
//! in the same program sees the updated document.
//!
//! Supported losslessly today: setting a **scalar** value (`=`, `|=`), appending
//! scalar items to a **block sequence** (`+=`), deleting a mapping entry or a
//! sequence item (`del`), and creating a new **leaf key** on an existing block
//! mapping. Restructuring a block in place (replacing a mapping/sequence wholesale,
//! or creating nested keys) is refused rather than reflowed.

use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval, render_path};
use std::ops::Range;

use crate::Yaml;
use crate::compose::{Node, NodeKind, node_to_value};
use crate::scalar::{emit_key, emit_scalar_inline};

/// Apply a mutation expression to `doc`, preserving format everywhere untouched.
pub fn apply(doc: &mut Yaml, expr: &Expr) -> Result<(), EditError> {
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
                .ok_or_else(|| EditError::new(format!("path not found: {}", render_path(steps))))?;
            let value = eval_one(rhs, &current)?;
            doc.set(steps, &value)
        }
        Expr::AddAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let current = doc
                .value_at(steps)
                .ok_or_else(|| EditError::new(format!("path not found: {}", render_path(steps))))?;
            let addend = eval_one(rhs, &doc.to_value())?;
            match (&current, &addend) {
                (Value::Array(_), Value::Array(items)) => doc.append(steps, items),
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

fn add_values(current: &Value, addend: &Value) -> Result<Value, EditError> {
    let expr = Expr::Binary(
        BinOp::Add,
        Box::new(Expr::Path(Vec::new())),
        Box::new(Expr::Literal(addend.clone())),
    );
    eval_one(&expr, current)
}

/// The result of walking a path over the span tree.
enum Resolved<'a> {
    /// The path landed on an existing node.
    Found(&'a Node),
    /// The path's final field is absent on this (existing) mapping — an insert point.
    MissingField { parent: &'a Node, key: String },
    /// The path does not resolve (bad step, missing intermediate, out of range).
    NotFound,
}

fn resolve<'a>(node: &'a Node, path: &[Step]) -> Resolved<'a> {
    let Some((step, rest)) = path.split_first() else {
        return Resolved::Found(node);
    };
    match step {
        Step::Field(k) => match &node.kind {
            NodeKind::Mapping(entries) => match entries.iter().find(|e| &e.key == k) {
                Some(e) => resolve(&e.value, rest),
                None if rest.is_empty() => Resolved::MissingField {
                    parent: node,
                    key: k.clone(),
                },
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

fn normalize_index(i: i64, len: usize) -> Option<usize> {
    let idx = if i < 0 { len as i64 + i } else { i };
    (idx >= 0).then_some(idx as usize)
}

impl Yaml {
    /// The value at `path`, if it resolves (used for `|=` and `+=`).
    pub(crate) fn value_at(&self, path: &[Step]) -> Option<Value> {
        match resolve(&self.doc, path) {
            Resolved::Found(node) => Some(node_to_value(node)),
            _ => None,
        }
    }

    /// Set the scalar at `path`, or create it as a new leaf key.
    pub(crate) fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        let (range, text) = match resolve(&self.doc, path) {
            Resolved::Found(node) => match &node.kind {
                NodeKind::Scalar(_) => {
                    // A multi-line scalar (block `|`/`>`, or a wrapped quoted
                    // scalar) can't be replaced with a single inline token without
                    // reflowing surrounding lines — refuse cleanly rather than
                    // emit something that fails to re-parse.
                    if self.source[node.span.clone()].contains('\n') {
                        return Err(EditError::new(
                            "cannot set a multi-line (block `|`/`>`) scalar in place yet",
                        ));
                    }
                    (node.span.clone(), emit_scalar_inline(value)?)
                }
                _ => {
                    return Err(EditError::new(
                        "edikt sets scalar values in YAML losslessly; replacing a whole \
                         mapping or sequence isn't supported yet",
                    ));
                }
            },
            Resolved::MissingField { parent, key } => new_key(&self.source, parent, &key, value)?,
            Resolved::NotFound => {
                return Err(EditError::new(format!(
                    "path not found: {}",
                    render_path(path)
                )));
            }
        };
        self.commit(range, &text)
    }

    /// Append scalar `items` to the block sequence at `path` (`+=`).
    pub(crate) fn append(&mut self, path: &[Step], items: &[Value]) -> Result<(), EditError> {
        let (range, text) = match resolve(&self.doc, path) {
            Resolved::Found(node) => match &node.kind {
                NodeKind::Sequence(seq) => append_items(&self.source, node, seq, items)?,
                _ => {
                    return Err(EditError::new(format!(
                        "`+=` with an array needs a sequence at {}",
                        render_path(path)
                    )));
                }
            },
            _ => {
                return Err(EditError::new(format!(
                    "path not found: {}",
                    render_path(path)
                )));
            }
        };
        self.commit(range, &text)
    }

    /// Delete the mapping entry or sequence item at `path`.
    pub(crate) fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        let Some((last, parent_path)) = path.split_last() else {
            return Err(EditError::new("cannot delete the whole document"));
        };
        let range = match resolve(&self.doc, parent_path) {
            Resolved::Found(parent) => match (last, &parent.kind) {
                (Step::Field(k), NodeKind::Mapping(entries)) => {
                    let entry = entries.iter().find(|e| &e.key == k).ok_or_else(|| {
                        EditError::new(format!("no key to delete at {}", render_path(path)))
                    })?;
                    line_start(&self.source, entry.key_span.start)
                        ..block_end(&self.source, &entry.value)
                }
                (Step::Index(i), NodeKind::Sequence(items)) => {
                    let idx = normalize_index(*i, items.len())
                        .filter(|n| *n < items.len())
                        .ok_or_else(|| {
                            EditError::new(format!(
                                "index {i} out of range at {} (len {})",
                                render_path(parent_path),
                                items.len()
                            ))
                        })?;
                    let item = &items[idx];
                    line_start(&self.source, item.span.start)..block_end(&self.source, item)
                }
                _ => {
                    return Err(EditError::new(format!(
                        "path not found: {}",
                        render_path(path)
                    )));
                }
            },
            _ => {
                return Err(EditError::new(format!(
                    "path not found: {}",
                    render_path(path)
                )));
            }
        };
        self.commit(range, "")
    }

    /// Replace `range` with `text`, then recompose. Atomic: if the result no
    /// longer parses, nothing changes and the error surfaces.
    fn commit(&mut self, range: Range<usize>, text: &str) -> Result<(), EditError> {
        let mut new_source = self.source.clone();
        new_source.replace_range(range, text);
        let doc = crate::compose::compose_source(&new_source)
            .map_err(|e| EditError::new(format!("edit produced invalid YAML: {e}")))?;
        self.source = new_source;
        self.doc = doc;
        Ok(())
    }
}

/// Build the splice that inserts `key: value` as a new entry in `parent` (a block
/// mapping), matching the last entry's indentation.
fn new_key(
    source: &str,
    parent: &Node,
    key: &str,
    value: &Value,
) -> Result<(Range<usize>, String), EditError> {
    let NodeKind::Mapping(entries) = &parent.kind else {
        return Err(EditError::new("can only add a key to a mapping"));
    };
    let valtext = emit_scalar_inline(value)?;
    let Some(last) = entries.last() else {
        return Err(EditError::new(
            "cannot add a key to an empty or flow mapping yet",
        ));
    };
    let indent = &source[line_start(source, last.key_span.start)..last.key_span.start];
    let at = block_end(source, &last.value);
    let nl = newline(source);
    let line = format!("{indent}{}: {valtext}", emit_key(key));
    if at == source.len() && !ends_with_newline(source) {
        // No trailing newline: start a fresh line for the new entry.
        Ok((at..at, format!("{nl}{line}")))
    } else {
        Ok((at..at, format!("{line}{nl}")))
    }
}

/// Build the splice that appends scalar `items` to a block sequence.
fn append_items(
    source: &str,
    seq_node: &Node,
    items_nodes: &[Node],
    items: &[Value],
) -> Result<(Range<usize>, String), EditError> {
    if items_nodes.is_empty() {
        return Err(EditError::new(
            "cannot append to an empty sequence yet (no item to match indentation)",
        ));
    }
    // A block sequence's start-mark sits on the first item's `-`. Derive the dash
    // column + indentation from there, so this is robust even when the *last* item
    // is a block scalar (whose span starts at its content, not the dash) — the case
    // that used to misreport as a flow sequence.
    let dash = seq_node.span.start;
    if source.as_bytes().get(dash) != Some(&b'-') {
        return Err(EditError::new(
            "cannot append to a flow sequence (`[...]`) yet",
        ));
    }
    let indent = &source[line_start(source, dash)..dash];
    let nl = newline(source);
    let at = block_end(source, seq_node);
    let mut out = String::new();
    for item in items {
        let text = emit_scalar_inline(item)?;
        out.push_str(indent);
        out.push_str("- ");
        out.push_str(&text);
        out.push_str(nl);
    }
    if at == source.len() && !ends_with_newline(source) {
        // No trailing newline at EOF: lead with one, drop the dangling trailer.
        out.insert_str(0, nl);
        out.truncate(out.len() - nl.len());
    }
    Ok((at..at, out))
}

/// The newline style the document uses, so inserted lines match it (a lone `\n`
/// spliced into a CRLF file would leave observably mixed line endings).
fn newline(source: &str) -> &'static str {
    if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Whether the source already ends with a line break (`\n`, hence also `\r\n`).
fn ends_with_newline(source: &str) -> bool {
    source.ends_with('\n')
}

/// The byte offset of the start of the line containing `pos`.
fn line_start(source: &str, pos: usize) -> usize {
    source[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// The byte offset just after the newline that ends the line at/after `end`.
///
/// libyaml lands scalar end-marks mid-line (right after the text) but collection
/// end-marks at the *next* line's start; this normalizes both to "start of the
/// following line" (or EOF).
fn line_after(source: &str, end: usize) -> usize {
    let bytes = source.as_bytes();
    if end == 0 || bytes.get(end - 1) == Some(&b'\n') {
        return end;
    }
    match source[end..].find('\n') {
        Some(i) => end + i + 1,
        None => source.len(),
    }
}

/// The byte offset just after the last physical line of `node`.
///
/// A collection's own end-mark is unreliable for line math — libyaml lands it on
/// the *next sibling's* text (past that sibling's indent), which would overshoot.
/// So we drill to the node's deepest last scalar and take the line after *it*.
fn block_end(source: &str, node: &Node) -> usize {
    match &node.kind {
        NodeKind::Scalar(_) => line_after(source, node.span.end),
        NodeKind::Sequence(items) => match items.last() {
            Some(last) => block_end(source, last),
            None => line_after(source, node.span.end),
        },
        NodeKind::Mapping(entries) => match entries.last() {
            Some(last) => block_end(source, &last.value),
            None => line_after(source, node.span.end),
        },
    }
}

/// The original source text of each node selected by `path`, in document order
/// (aligned with the evaluator). See [`slice_of`] for the per-node form.
pub(crate) fn source_slices(source: &str, root: &Node, path: &[Step]) -> Vec<String> {
    let mut current: Vec<&Node> = vec![root];
    for step in path {
        let mut next: Vec<&Node> = Vec::new();
        for node in &current {
            match step {
                Step::Field(k) => {
                    if let NodeKind::Mapping(entries) = &node.kind
                        && let Some(e) = entries.iter().find(|e| &e.key == k)
                    {
                        next.push(&e.value);
                    }
                }
                Step::Index(i) => {
                    if let NodeKind::Sequence(items) = &node.kind
                        && let Some(item) =
                            normalize_index(*i, items.len()).and_then(|n| items.get(n))
                    {
                        next.push(item);
                    }
                }
                Step::Iterate => match &node.kind {
                    NodeKind::Sequence(items) => next.extend(items.iter()),
                    NodeKind::Mapping(entries) => next.extend(entries.iter().map(|e| &e.value)),
                    _ => {}
                },
                // A comment addresses no value node — source slices never
                // resolve one (the CLI reads comments via `to_commented`).
                Step::Comment(_) => {}
            }
        }
        current = next;
    }
    current.iter().map(|n| slice_of(source, n)).collect()
}

/// The source form of one node: a scalar or flow collection (`[...]`/`{...}`) is
/// returned verbatim; a block collection is returned as its full-line region,
/// dedented to the left margin so the fragment is valid standalone YAML.
fn slice_of(source: &str, node: &Node) -> String {
    match &node.kind {
        NodeKind::Scalar(_) => source[node.span.clone()].to_string(),
        _ if matches!(source.as_bytes().get(node.span.start), Some(b'[' | b'{')) => {
            source[node.span.clone()].to_string()
        }
        _ => {
            let start = line_start(source, node.span.start);
            let end = block_end(source, node);
            dedent(source[start..end].trim_end_matches(['\n', '\r']))
        }
    }
}

/// Strip the common leading whitespace of every non-blank line.
fn dedent(text: &str) -> String {
    let min = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    text.lines()
        .map(|l| if l.len() >= min { &l[min..] } else { l })
        .collect::<Vec<_>>()
        .join("\n")
}
