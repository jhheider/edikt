//! The mutation driver every format shares.
//!
//! A format provides the primitive, format-preserving edits ([`Mutable`]:
//! read the value at a path, set it, delete it); [`apply_mutation`] turns a
//! mutation expression (`=`, `|=`, `+=`, `del`, `|`-chains of them, and
//! path-expression targets through [`crate::lower_mutation`]) into calls on
//! those primitives. The format decides what a path means; the driver decides
//! what the expression means, once, for all of them. That includes what an
//! assignment of a collection over a collection means: element edits, not
//! one replacement ([`assign`]).

use crate::ast::{Expr, Step};
use crate::error::EditError;
use crate::eval::{eval, expand_iter_paths};
use crate::value::Value;

mod diff;

pub use diff::{assign, diff_grows};

/// Which mutation a target path belongs to, for [`Mutable::check_path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    /// `path = value`
    Assign,
    /// `path |= f`
    Update,
    /// `path += value`
    Add,
    /// `del(path)`
    Delete,
}

/// The primitive edits a format provides to [`apply_mutation`].
///
/// Paths handed to `value_at`, `set`, `add` and `delete` are plain paths.
/// With [`Mutable::FANS_OUT`] they are also iterate-free for everything but
/// `=`'s create check and `delete` (a format's own `del(.a[])` semantics).
pub trait Mutable {
    /// Whether `[]` in an assignment target fans out into one edit per
    /// element (`.a[] |= f`). A format without it sees the iterate path in
    /// `set`/`value_at` and answers for it itself.
    const FANS_OUT: bool = true;

    /// The document value an assignment's right side, and a path-expression
    /// target, is evaluated against.
    fn whole(&self) -> Value;

    /// The value at `path`, or `None` when it doesn't resolve.
    fn value_at(&self, path: &[Step]) -> Option<Value>;

    /// Set the value at `path`, format-preserving (creating it where the
    /// format can).
    fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError>;

    /// Delete the value at `path`.
    fn delete(&mut self, path: &[Step]) -> Result<(), EditError>;

    /// Append `items` to the array at `path` in place, format-preserving.
    /// `None` (the default) means the format has no in-place append, and
    /// `+=` writes the concatenated array through `set` instead.
    fn append(&mut self, path: &[Step], items: &[Value]) -> Option<Result<(), EditError>> {
        let _ = (path, items);
        None
    }

    /// `path += addend`, where `current` is the value there: array onto
    /// array goes through [`Mutable::append`] when the format has one;
    /// object onto object is jq's shallow merge, written as one keyed
    /// [`assign`] per right-hand entry, so the object's other entries keep
    /// their bytes;
    /// anything else writes `current + addend`.
    fn add(&mut self, path: &[Step], current: &Value, addend: &Value) -> Result<(), EditError> {
        if let (Value::Array(_), Value::Array(items)) = (current, addend)
            && let Some(done) = self.append(path, items)
        {
            return done;
        }
        if let (Value::Object(_), Value::Object(entries)) = (current, addend) {
            let mut at = path.to_vec();
            for (k, v) in entries {
                at.push(Step::Field(k.clone()));
                assign(self, &at, v)?;
                at.pop();
            }
            return Ok(());
        }
        let sum = add_values(current, addend)?;
        self.set(path, &sum)
    }

    /// What `|=`/`+=` does when `path` doesn't resolve. The default is an
    /// error; a document mapped over leniently can make it a no-op.
    fn miss(&self, path: &[Step]) -> Result<(), EditError> {
        let _ = path;
        Err(EditError::new("path not found"))
    }

    /// Whether an assignment of `value` over the collection `current` at
    /// `path` may be written as element edits ([`assign`]). A format
    /// declines where it can't make them in place, and the collection is
    /// then replaced through [`Mutable::set`]. The default accepts.
    fn diffs(&self, path: &[Step], current: &Value, value: &Value) -> bool {
        let _ = (path, current, value);
        true
    }

    /// Reject a target path this format can't edit before anything happens.
    /// The default accepts every path.
    fn check_path(&self, path: &[Step], kind: MutationKind) -> Result<(), EditError> {
        let _ = (path, kind);
        Ok(())
    }
}

/// `current + addend` with the evaluator's `+` (numbers add, strings and
/// arrays concatenate, objects merge, `null` is the identity).
pub fn add_values(current: &Value, addend: &Value) -> Result<Value, EditError> {
    Ok(crate::eval::add(current, addend)?)
}

/// Apply a mutation expression to `doc` through its [`Mutable`] primitives.
///
/// `=` and `+=` evaluate their right side against the whole document; `|=`
/// against the value it replaces. A path-expression target is lowered to one
/// plain-path edit per match first, and `a | b` applies `a` then `b`.
pub fn apply_mutation<M: Mutable + ?Sized>(doc: &mut M, expr: &Expr) -> Result<(), EditError> {
    // A path-expression target (`(.xs[] | select(...) | .n) = v`, #88)
    // resolves to concrete paths first; each is then an ordinary edit.
    if let Some(each) = crate::lower_mutation(expr, || doc.whole())? {
        for e in &each {
            apply_mutation(doc, e)?;
        }
        return Ok(());
    }
    match expr {
        Expr::Assign(lhs, rhs) => {
            let steps = target(doc, lhs, MutationKind::Assign)?;
            let whole = doc.whole();
            let value = eval_one(rhs, &whole)?;
            if fans_out::<M>(steps) {
                // `.a[] = x`: every element gets the same value.
                return set_each(doc, steps, &whole, true, |_| Ok(value.clone()));
            }
            assign(doc, steps, &value)
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let steps = target(doc, lhs, MutationKind::Update)?;
            if fans_out::<M>(steps) {
                // `.a[] |= f`: map `f` over each element.
                let whole = doc.whole();
                return set_each(doc, steps, &whole, false, |current| eval_one(rhs, current));
            }
            let Some(current) = doc.value_at(steps) else {
                return doc.miss(steps);
            };
            let value = eval_one(rhs, &current)?;
            assign(doc, steps, &value)
        }
        Expr::AddAssign(lhs, rhs) => {
            let steps = target(doc, lhs, MutationKind::Add)?;
            let whole = doc.whole();
            let addend = eval_one(rhs, &whole)?;
            if fans_out::<M>(steps) {
                // `.a[] += x`: jq's `.a[] |= . + x`, per element, each
                // through the format's own `add` (in-place append, keyed
                // merge) like a hand-typed `.a[i] += x`.
                for path in expand_iter_paths(steps, &whole)? {
                    let Some(current) = doc.value_at(&path) else {
                        return doc.miss(&path);
                    };
                    doc.add(&path, &current, &addend)?;
                }
                return Ok(());
            }
            let Some(current) = doc.value_at(steps) else {
                return doc.miss(steps);
            };
            doc.add(steps, &current, &addend)
        }
        Expr::Pipe(a, b) => {
            apply_mutation(doc, a)?;
            apply_mutation(doc, b)
        }
        Expr::Call(name, args) if name == "del" => {
            if args.len() != 1 {
                return Err(EditError::new("del(...) takes one path argument"));
            }
            let steps = args[0]
                .as_path()
                .ok_or_else(|| EditError::new("del(...) takes a path"))?;
            doc.check_path(steps, MutationKind::Delete)?;
            doc.delete(steps)
        }
        _ => Err(EditError::new(
            "expected an assignment (`path = value`) or `del(path)`",
        )),
    }
}

/// An assignment's left side as a plain path the format accepts.
fn target<'e, M: Mutable + ?Sized>(
    doc: &M,
    lhs: &'e Expr,
    kind: MutationKind,
) -> Result<&'e [Step], EditError> {
    let steps = lhs
        .as_path()
        .ok_or_else(|| EditError::new("left side of an assignment must be a path"))?;
    doc.check_path(steps, kind)?;
    Ok(steps)
}

fn fans_out<M: Mutable + ?Sized>(steps: &[Step]) -> bool {
    M::FANS_OUT && steps.contains(&Step::Iterate)
}

/// Apply `f` to each element selected by `steps` (which contains at least one
/// `Step::Iterate`), assigning each through the format's ordinary
/// index-keyed edits, so every replacement is as surgical as a hand-typed
/// `.a[i] = ..`.
/// `create` is true for plain assignment (`=`), where an expansion resolving
/// to nothing means "you asked to create elements through `[]`" and errors;
/// the update forms (`|=`, `+=`) treat an empty expansion as a miss (a no-op),
/// jq-shaped.
fn set_each<M: Mutable + ?Sized>(
    doc: &mut M,
    steps: &[Step],
    whole: &Value,
    create: bool,
    f: impl Fn(&Value) -> Result<Value, EditError>,
) -> Result<(), EditError> {
    let paths = expand_iter_paths(steps, whole)?;
    if paths.is_empty() && create {
        return Err(EditError::new("cannot create through `[]`"));
    }
    for path in &paths {
        let Some(current) = doc.value_at(path) else {
            return doc.miss(path);
        };
        let value = f(&current)?;
        assign(doc, path, &value)?;
    }
    Ok(())
}

/// The first result of `expr` over `input`: an assignment's right side.
fn eval_one(expr: &Expr, input: &Value) -> Result<Value, EditError> {
    eval(expr, input)?
        .into_iter()
        .next()
        .ok_or_else(|| EditError::new("right side of the assignment produced no value"))
}

#[cfg(test)]
mod tests;
