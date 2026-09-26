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

use edikt_core::{EditError, Expr, Mutable, Step, Value, add_values, eval, render_path};

use crate::Yaml;

mod extent;
mod resolve;
mod write;

pub(crate) use extent::{
    block_end, ends_with_newline, insert_end, newline, source_slices, trim_newline,
};

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
        edikt_core::check_doc_index(*idx, n)?;
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
    edikt_core::apply_mutation(&mut InDoc { doc, idx, strict }, expr)
}

/// One document of the stream, as the shared mutation driver sees it: its own
/// value for the right side, and a miss that is an error or a no-op
/// depending on how the document was addressed.
struct InDoc<'a> {
    doc: &'a mut Yaml,
    idx: usize,
    strict: Strictness,
}

impl Mutable for InDoc<'_> {
    fn whole(&self) -> Value {
        self.doc.doc_value(self.idx)
    }
    fn value_at(&self, path: &[Step]) -> Option<Value> {
        self.doc.value_at(self.idx, path)
    }
    fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        self.doc.set(self.idx, path, value, self.strict)
    }
    fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        self.doc.delete(self.idx, path)
    }
    fn add(&mut self, path: &[Step], current: &Value, addend: &Value) -> Result<(), EditError> {
        match (current, addend) {
            (Value::Array(_), Value::Array(items)) => {
                self.doc.append(self.idx, path, items, self.strict)
            }
            _ => self.set(path, &add_values(current, addend)?),
        }
    }
    fn miss(&self, path: &[Step]) -> Result<(), EditError> {
        miss(self.strict, path)
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
    let kept = eval(&filter, val)?;
    Ok(!kept.is_empty())
}
