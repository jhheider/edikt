//! Format-preserving edits by byte splice over the span tree.
//!
//! An edit resolves the target node's byte range (from the marks composed in
//! [`crate::compose`]) and replaces exactly those bytes; every other byte of the
//! source is left verbatim, so comments, indentation, and layout survive
//! untouched. After each splice we recompose from the new source, so a later step
//! in the same program sees the updated document.
//!
//! Supported: setting any value (`=`, `|=`), a scalar or a mapping/sequence in
//! place of either; appending items to a sequence (`+=`); deleting a mapping
//! entry or a sequence item (`del`); and creating a new **leaf key** on an
//! existing mapping, with any missing parent mappings above it (`=` creates
//! them, as in every format). A new collection is laid out in the file's own
//! style (see [`crate::splice`]); a replacement that keeps a collection's
//! shape edits only the elements that change.

use edikt_core::{BinOp, EditError, Expr, Step, Value, eval, expand_iter_paths, render_path};
use std::ops::Range;

use crate::Yaml;
use crate::block::BlockScalar;
use crate::compose::{Node, NodeKind, collect_merge, node_to_value};
use crate::layout::{Indent, flow_text, inline_text, is_flow, line_start};
use crate::scalar::{QuoteStyle, emit_scalar_styled, kept_properties, split_properties};
use crate::splice::{Slot, append_items, block_replace, new_key, scalar_to_block};

/// Apply a mutation expression to `doc`, preserving format everywhere untouched.
///
/// A multi-document stream (`---`-separated) maps the edit over **every**
/// document by default. A leading `select(pred)` narrows it to the documents
/// whose value satisfies `pred` - so `select(.kind == "Service") | .spec.x = 1`
/// edits only the Service documents. A single-document stream (the common case)
/// behaves exactly as before.
pub fn apply(doc: &mut Yaml, expr: &Expr) -> Result<Vec<String>, EditError> {
    let n = doc.docs.len();
    let mut warnings = Vec::new();

    // `^dN` names one document by position; the edit is strict there (you asked
    // for that specific document).
    if let Expr::DocSelect(idx, body) = expr {
        if *idx >= n {
            return Err(EditError::new(format!(
                "document `^d{idx}` is out of range ({n} document{})",
                if n == 1 { "" } else { "s" }
            )));
        }
        apply_one(doc, *idx, body, Strictness::Strict)?;
        return Ok(warnings);
    }

    // A leading `select(pred)` picks documents by content; the rest is the edit.
    if let Some((pred, inner)) = peel_select(expr) {
        for idx in 0..n {
            let val = doc.doc_value(idx);
            // A predicate that can't be evaluated against a document (e.g. a
            // scalar document when the predicate indexes a field) means "does
            // not match": skip it, with a warning, rather than aborting the
            // whole edit. `select` picks by content, so a heterogeneous stream
            // shouldn't fail because one document has the wrong shape.
            match doc_matches(pred, &val) {
                Ok(true) => apply_one(doc, idx, &inner, Strictness::Lenient)?,
                Ok(false) => {}
                Err(e) => warnings.push(format!("select skipped document {idx}: {e}")),
            }
        }
        return Ok(warnings);
    }

    // No selector: single doc keeps the strict, prior behavior (a missing path
    // errors, a new leaf key is created); across many docs, apply to each with a
    // missing path treated as a per-document no-op.
    if n <= 1 {
        apply_one(doc, 0, expr, Strictness::Strict)?;
    } else {
        for idx in 0..n {
            apply_one(doc, idx, expr, Strictness::Lenient)?;
        }
    }
    Ok(warnings)
}

/// Whether a path that doesn't resolve is an error (`Strict`, single-doc/named)
/// or a silent no-op (`Lenient`, one of many mapped documents).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Strictness {
    Strict,
    Lenient,
}

/// Apply one edit program to document `idx` in isolation.
fn apply_one(doc: &mut Yaml, idx: usize, expr: &Expr, strict: Strictness) -> Result<(), EditError> {
    match expr {
        Expr::Assign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let whole = doc.doc_value(idx);
            let value = eval_one(rhs, &whole)?;
            if steps.contains(&Step::Iterate) {
                return set_each(doc, idx, steps, &whole, true, |_| Ok(value.clone()), strict);
            }
            doc.set(idx, steps, &value, strict)
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let whole = doc.doc_value(idx);
            if steps.contains(&Step::Iterate) {
                return set_each(
                    doc,
                    idx,
                    steps,
                    &whole,
                    false,
                    |current| eval_one(rhs, current),
                    strict,
                );
            }
            let Some(current) = doc.value_at(idx, steps) else {
                return miss(strict, steps);
            };
            let value = eval_one(rhs, &current)?;
            doc.set(idx, steps, &value, strict)
        }
        Expr::AddAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let whole = doc.doc_value(idx);
            let addend = eval_one(rhs, &whole)?;
            if steps.contains(&Step::Iterate) {
                return set_each(
                    doc,
                    idx,
                    steps,
                    &whole,
                    false,
                    |current| add_values(current, &addend),
                    strict,
                );
            }
            let Some(current) = doc.value_at(idx, steps) else {
                return miss(strict, steps);
            };
            match (&current, &addend) {
                (Value::Array(_), Value::Array(items)) => doc.append(idx, steps, items, strict),
                _ => doc.set(idx, steps, &add_values(&current, &addend)?, strict),
            }
        }
        Expr::Pipe(a, b) => {
            apply_one(doc, idx, a, strict)?;
            apply_one(doc, idx, b, strict)
        }
        Expr::Call(name, args) if name == "del" => {
            if args.len() != 1 {
                return Err(EditError::new("del(...) takes one path argument"));
            }
            let steps = args[0]
                .as_path()
                .ok_or_else(|| EditError::new("del(...) takes a path"))?;
            doc.delete(idx, steps)
        }
        _ => Err(EditError::new(
            "expected an assignment (`path = value`, `path |= expr`) or `del(path)`",
        )),
    }
}

/// A path that didn't resolve: an error when strict, a no-op when lenient.
fn miss(strict: Strictness, steps: &[Step]) -> Result<(), EditError> {
    match strict {
        Strictness::Strict => Err(EditError::new(format!(
            "path not found: {}",
            render_path(steps)
        ))),
        Strictness::Lenient => Ok(()),
    }
}

/// If `expr` is a pipe whose leftmost stage is `select(pred)`, return that
/// predicate and the edit expression with the select removed. Handles a
/// left-associated chain (`select(p) | .a = 1 | .b = 2`).
fn peel_select(expr: &Expr) -> Option<(&Expr, Expr)> {
    let Expr::Pipe(left, right) = expr else {
        return None;
    };
    match left.as_ref() {
        Expr::Call(name, args) if name == "select" && args.len() == 1 => {
            Some((&args[0], (**right).clone()))
        }
        Expr::Pipe(..) => {
            let (pred, rest) = peel_select(left)?;
            Some((pred, Expr::Pipe(Box::new(rest), right.clone())))
        }
        _ => None,
    }
}

/// Does document value `val` satisfy `pred`? Reuses the `select` filter: a
/// document matches when `select(pred)` keeps it.
fn doc_matches(pred: &Expr, val: &Value) -> Result<bool, EditError> {
    let filter = Expr::Call("select".into(), vec![pred.clone()]);
    let kept = eval(&filter, val).map_err(|e| EditError::new(e.to_string()))?;
    Ok(!kept.is_empty())
}

fn assign_path(lhs: &Expr) -> Result<&[Step], EditError> {
    lhs.as_path()
        .ok_or_else(|| EditError::new("left side of an assignment must be a path"))
}

/// Apply `f` to each element selected by `steps` (a path containing at least one
/// `Step::Iterate`), splicing each element in place via the ordinary index-keyed
/// `set` path. Each splice recomposes from the new source, exactly as a
/// sequential program of `.a[i] = ...` edits would - just derived from the
/// element expansion instead of typed by hand.
fn set_each(
    doc: &mut Yaml,
    idx: usize,
    steps: &[Step],
    whole: &Value,
    create: bool,
    f: impl Fn(&Value) -> Result<Value, EditError>,
    strict: Strictness,
) -> Result<(), EditError> {
    let paths = expand_iter_paths(steps, whole).map_err(|e| EditError::new(e.to_string()))?;
    if paths.is_empty() && create {
        return Err(EditError::new("cannot create through `[]`"));
    }
    for path in &paths {
        let Some(current) = doc.value_at(idx, path) else {
            return miss(strict, path);
        };
        let value = f(&current)?;
        doc.set(idx, path, &value, strict)?;
    }
    Ok(())
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
enum Resolved<'a, 'p> {
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

fn resolve<'a, 'p>(node: &'a Node, path: &'p [Step]) -> Resolved<'a, 'p> {
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
fn in_flow(source: &str, root: &Node, path: &[Step]) -> bool {
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

fn normalize_index(i: i64, len: usize) -> Option<usize> {
    let idx = if i < 0 { len as i64 + i } else { i };
    (idx >= 0).then_some(idx as usize)
}

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
        if collection && identical(&node_to_value(node), value) {
            return Ok(());
        }
        if let Some(plan) = elementwise(source, node, value) {
            return self.apply_plan(idx, path, plan, strict);
        }
        let scalar = matches!(node.kind, NodeKind::Scalar(_));
        let token = &source[node.span.clone()];
        // A multi-line scalar (block `|`/`>`, or a wrapped quoted scalar)
        // can't be replaced without reflowing the lines around it; refuse
        // cleanly rather than emit something that fails to re-parse.
        if scalar && token.contains('\n') {
            return Err(EditError::new(
                "cannot set a multi-line (block `|`/`>`) scalar in place yet",
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
            let paths = edikt_core::expand_delete_paths(path, &whole)
                .map_err(|e| EditError::new(e.to_string()))?;
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
                let Some(idx) = normalize_index(*i, items.len()).filter(|n| *n < items.len())
                else {
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

/// The nested mapping a missing path's tail names: `[.b, .c]` with `v` is
/// `{b: {c: v}}`. `rest` is all fields ([`resolve`] guarantees it).
fn nest(rest: &[Step], value: &Value) -> Value {
    rest.iter()
        .rev()
        .fold(value.clone(), |inner, step| match step {
            Step::Field(k) => Value::Object(vec![(k.clone(), inner)]),
            _ => inner,
        })
}

/// Where the node at `path` sits: the document root, a mapping value (with
/// its key's span), or a sequence item.
fn slot_of(root: &Node, path: &[Step]) -> Slot {
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

/// Exactly the same value: same types, same float bits, keys in the same
/// order. Stricter than `==`, which is jq's (`1 == 1.0`, key order ignored),
/// because an element judged unchanged keeps its bytes, and `.a = 1.0` over
/// `a: 1` must still write `1.0`.
fn identical(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| identical(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((xk, xv), (yk, yv))| xk == yk && identical(xv, yv))
        }
        _ => false,
    }
}

/// A replacement applied element by element: the changed elements, the items
/// to append, and the keys to add.
struct Plan {
    changes: Vec<(Step, Value)>,
    append: Vec<Value>,
    add: Vec<(String, Value)>,
}

/// Plan `value` as element edits to the collection `node` when it keeps the
/// collection's shape: the same kind, every existing key in its order (or
/// every existing item), and anything new after them. Then only the elements
/// that change are touched, and the rest keep their bytes and comments; `.a
/// |= . + [x]` appends one line instead of rewriting the list. A flow
/// collection takes element edits only when nothing is added, so a growing
/// one is respelled whole. `None` means a wholesale replacement.
fn elementwise(source: &str, node: &Node, value: &Value) -> Option<Plan> {
    let mut plan = Plan {
        changes: Vec::new(),
        append: Vec::new(),
        add: Vec::new(),
    };
    match (&node.kind, value) {
        (NodeKind::Sequence(items), Value::Array(new)) if new.len() >= items.len() => {
            for (i, (old, new)) in items.iter().zip(new).enumerate() {
                if !identical(&node_to_value(old), new) {
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
                    if !identical(&node_to_value(&e.value), v) {
                        plan.changes.push((Step::Field(k.clone()), v.clone()));
                    }
                    pos += 1;
                } else if phys.iter().any(|e| &e.key == k) {
                    return None; // reordered
                } else if merged.iter().any(|(mk, mv)| mk == k && identical(mv, v)) {
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

/// The newline style the document uses, so inserted lines match it (a lone `\n`
/// spliced into a CRLF file would leave observably mixed line endings).
pub(crate) fn newline(source: &str) -> &'static str {
    if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Whether the source already ends with a line break (`\n`, hence also `\r\n`).
pub(crate) fn ends_with_newline(source: &str) -> bool {
    source.ends_with('\n')
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

/// `end` with the line break just before it (`\n` or `\r\n`) excluded.
pub(crate) fn trim_newline(source: &str, end: usize) -> usize {
    let s = &source[..end];
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.strip_suffix('\r').unwrap_or(s).len()
}

/// The byte offset just after the last physical line of `node`.
///
/// A collection's own end-mark is unreliable for line math; libyaml lands it on
/// the *next sibling's* text (past that sibling's indent), which would overshoot.
/// So we drill to the node's deepest last scalar and take the line after *it*.
pub(crate) fn block_end(source: &str, node: &Node) -> usize {
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

/// Where a key or item added after `node` goes: [`block_end`], except that a
/// block scalar ending the node leaves its trailing blank lines after the
/// insertion, where they keep separating it from what follows (#90). Only a
/// keep-chomped (`+`) scalar owns those lines, so an insertion goes past them.
pub(crate) fn insert_end(source: &str, node: &Node) -> usize {
    match &node.kind {
        NodeKind::Scalar(_) => {
            BlockScalar::of(source, node).map_or_else(|| block_end(source, node), |b| b.insert_at())
        }
        NodeKind::Sequence(items) => items
            .last()
            .map_or_else(|| block_end(source, node), |n| insert_end(source, n)),
        NodeKind::Mapping(entries) => entries
            .last()
            .map_or_else(|| block_end(source, node), |e| insert_end(source, &e.value)),
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
                // A comment addresses no value node; source slices never
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
