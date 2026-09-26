//! The query evaluator (value calculus) over an in-memory [`Value`].
//!
//! jq-style generator semantics: every expression maps one input value to a
//! *stream* of output values (0, 1, or many), collected here into a `Vec`.
//! A miss (missing key, out-of-range index) yields an **empty stream**, not
//! `null`: the CLI renders it as a silent no-op (sed-shaped), and `//`
//! supplies defaults. An explicit `null` in the document still yields `null`.
//!
//! Mutation `=`, `|=`, and `del` are handled here at the value level - this
//! defines the *semantics* (what value ends up where). The format-preserving CST
//! *write* path lives in the format modules and mirrors these rules. `+=`
//! arrives in a later slice.

use crate::ast::{BinOp, Expr, Step};
use crate::builtins::{comment_mutation_unsupported, eval_call};
use crate::comment::Commented;
use crate::value::Value;
use std::cmp::Ordering;

/// An evaluation failure (type error, unknown function, arity mismatch).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg}")]
pub struct EvalError {
    pub msg: String,
}

impl EvalError {
    pub(crate) fn new(msg: impl Into<String>) -> EvalError {
        EvalError { msg: msg.into() }
    }
}

/// Evaluate a query that may address comments (`#`) against the document's
/// commented projection. Comment-free sub-expressions fall back to the plain
/// value evaluator; a comment path resolves the comment text of each selected
/// node. Supported in v0.2 Phase 1 as a **read** surface: a comment path
/// (`.foo.#`, `.foo.#.inline`, `.items[].#`) optionally piped or defaulted
/// (`| ascii_upcase`, `// "none"`). Comment access after a value pipe, or as an
/// assignment target, is not yet served and errors clearly.
pub fn eval_with_comments(expr: &Expr, root: &Commented) -> Result<Vec<Value>, EvalError> {
    if !expr.has_comment() {
        return eval(expr, &root.to_value());
    }
    match expr {
        Expr::Path(steps) => Ok(root.resolve_comment(steps)),
        // The document-wide `comments` stream: one record per comment.
        Expr::Call(name, args) if name == "comments" && args.is_empty() => {
            Ok(comment_records(root))
        }
        Expr::Pipe(a, b) => {
            let mut out = Vec::new();
            for v in eval_with_comments(a, root)? {
                // Past the comment, the piped value is an ordinary scalar.
                out.extend(eval(b, &v)?);
            }
            Ok(out)
        }
        Expr::Alternative(a, b) => {
            let truthy: Vec<Value> = eval_with_comments(a, root)?
                .into_iter()
                .filter(Value::is_truthy)
                .collect();
            if truthy.is_empty() {
                eval_with_comments(b, root)
            } else {
                Ok(truthy)
            }
        }
        Expr::Comma(items) => {
            let mut out = Vec::new();
            for it in items {
                out.extend(eval_with_comments(it, root)?);
            }
            Ok(out)
        }
        Expr::Collect(inner) => {
            let items = match inner {
                Some(e) => eval_with_comments(e, root)?,
                None => Vec::new(),
            };
            Ok(vec![Value::Array(items)])
        }
        _ => Err(EvalError::new(
            "comment access (`#` / `comments`) here isn't supported: use a comment \
             path (`.foo.#`) or the `comments` stream, optionally piped or collected",
        )),
    }
}

/// The document-wide `comments` stream: one `{ path, kind, text }` record per
/// comment, in document order. `path` is a rendered path to the annotated node
/// (`.web.image`), so `comments | select(.text | test("TODO")) | .path` answers
/// "which keys carry a TODO?".
fn comment_records(root: &Commented) -> Vec<Value> {
    root.comment_targets()
        .into_iter()
        .map(|(steps, kind, text)| {
            Value::Object(vec![
                ("path".into(), Value::Str(crate::render_path(&steps))),
                ("kind".into(), Value::Str(kind.as_str().to_string())),
                ("text".into(), Value::Str(text)),
            ])
        })
        .collect()
}

/// Evaluate `expr` against `input`, returning the output stream.
pub fn eval(expr: &Expr, input: &Value) -> Result<Vec<Value>, EvalError> {
    match expr {
        Expr::Path(steps) => eval_path(steps, input),
        Expr::Literal(v) => Ok(vec![v.clone()]),
        Expr::Neg(inner) => {
            let mut out = Vec::new();
            for v in eval(inner, input)? {
                out.push(negate(&v)?);
            }
            Ok(out)
        }
        Expr::Binary(op, l, r) => {
            let lefts = eval(l, input)?;
            let rights = eval(r, input)?;
            let mut out = Vec::new();
            for a in &lefts {
                for b in &rights {
                    out.push(binary(*op, a, b)?);
                }
            }
            Ok(out)
        }
        Expr::Pipe(l, r) => {
            let mut out = Vec::new();
            for v in eval(l, input)? {
                out.extend(eval(r, &v)?);
            }
            Ok(out)
        }
        Expr::Alternative(l, r) => {
            // jq's `//`: the left side's truthy outputs; if there are none -
            // a miss, `null`, or `false`: the right side's. A type *error*
            // on the left still propagates: a miss falls back, a mistake
            // doesn't hide.
            let truthy: Vec<Value> = eval(l, input)?
                .into_iter()
                .filter(Value::is_truthy)
                .collect();
            if truthy.is_empty() {
                eval(r, input)
            } else {
                Ok(truthy)
            }
        }
        Expr::Comma(items) => {
            let mut out = Vec::new();
            for it in items {
                out.extend(eval(it, input)?);
            }
            Ok(out)
        }
        Expr::Call(name, args) => eval_call(name, args, input),
        Expr::Collect(inner) => {
            let items = match inner {
                Some(e) => eval(e, input)?,
                None => Vec::new(),
            };
            Ok(vec![Value::Array(items)])
        }
        Expr::ObjectConstruct(pairs) => {
            let mut obj = Vec::with_capacity(pairs.len());
            for (key, value_expr) in pairs {
                let v = eval(value_expr, input)?
                    .into_iter()
                    .next()
                    .unwrap_or(Value::Null);
                obj.push((key.clone(), v));
            }
            Ok(vec![Value::Object(obj)])
        }
        // A path-expression target (`(.xs[] | select(...) | .n) = v`) lowers
        // to one plain-path mutation per match; apply them in turn.
        Expr::Assign(..) | Expr::UpdateAssign(..) | Expr::AddAssign(..)
            if crate::paths::path_expr_target(expr).is_some() =>
        {
            let mut out = input.clone();
            for m in crate::paths::lower_mutation(expr, || input.clone())?.unwrap_or_default() {
                out = eval(&m, &out)?.into_iter().next().unwrap_or(out);
            }
            Ok(vec![out])
        }
        Expr::Assign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let mut out = Vec::new();
            for rv in eval(rhs, input)? {
                out.push(set_path(input, steps, &rv)?);
            }
            Ok(out)
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            Ok(vec![update_path(input, steps, rhs)?])
        }
        Expr::AddAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let mut out = Vec::new();
            for rv in eval(rhs, input)? {
                let current = eval_path(steps, input)?
                    .into_iter()
                    .next()
                    .unwrap_or(Value::Null);
                let sum = binary(BinOp::Add, &current, &rv)?;
                out.push(set_path(input, steps, &sum)?);
            }
            Ok(out)
        }
        // `^dN` addresses documents, an axis the value evaluator has no notion
        // of; the CLI/format dispatch selects the document and evaluates the
        // body. Reached only when a `^dN` expression is evaluated against a
        // lone value (e.g. a non-YAML input), where the body simply applies.
        Expr::DocSelect(_, body) => eval(body, input),
    }
}

/// The left side of an assignment must be a plain path.
fn assign_path(expr: &Expr) -> Result<&[Step], EvalError> {
    expr.as_path()
        .ok_or_else(|| EvalError::new("left side of an assignment must be a path"))
}

/// Return a copy of `v` with `steps` set to `new`. Missing object keys and
/// array slots are created (arrays extend with nulls), matching jq.
fn set_path(v: &Value, steps: &[Step], new: &Value) -> Result<Value, EvalError> {
    let Some((head, rest)) = steps.split_first() else {
        return Ok(new.clone());
    };
    match head {
        Step::Field(k) => {
            let mut obj = match v {
                Value::Object(m) => m.clone(),
                Value::Null => Vec::new(),
                other => {
                    return Err(EvalError::new(format!(
                        "cannot set field of {}",
                        other.type_name()
                    )));
                }
            };
            match obj.iter_mut().find(|(kk, _)| kk == k) {
                Some(pair) => pair.1 = set_path(&pair.1, rest, new)?,
                None => obj.push((k.clone(), set_path(&Value::Null, rest, new)?)),
            }
            Ok(Value::Object(obj))
        }
        Step::Index(i) => {
            let mut arr = match v {
                Value::Array(a) => a.clone(),
                Value::Null => Vec::new(),
                other => {
                    return Err(EvalError::new(format!(
                        "cannot index {} with a number",
                        other.type_name()
                    )));
                }
            };
            let idx = crate::normalize_index(*i, arr.len())
                .ok_or_else(|| EvalError::new("array index out of range"))?;
            if idx >= arr.len() {
                arr.resize(idx + 1, Value::Null);
            }
            arr[idx] = set_path(&arr[idx], rest, new)?;
            Ok(Value::Array(arr))
        }
        Step::Iterate => match v {
            Value::Array(a) => {
                let mut out = Vec::with_capacity(a.len());
                for e in a {
                    out.push(set_path(e, rest, new)?);
                }
                Ok(Value::Array(out))
            }
            Value::Object(m) => {
                let mut out = Vec::with_capacity(m.len());
                for (k, e) in m {
                    out.push((k.clone(), set_path(e, rest, new)?));
                }
                Ok(Value::Object(out))
            }
            other => Err(EvalError::new(format!(
                "cannot iterate over {}",
                other.type_name()
            ))),
        },
        Step::Comment(_) => Err(EvalError::new(comment_mutation_unsupported())),
    }
}

/// Return a copy of `v` with the value at `steps` replaced by `f` applied to it.
fn update_path(v: &Value, steps: &[Step], f: &Expr) -> Result<Value, EvalError> {
    let Some((head, rest)) = steps.split_first() else {
        return Ok(eval(f, v)?.into_iter().next().unwrap_or(Value::Null));
    };
    match head {
        Step::Field(k) => {
            let mut obj = match v {
                Value::Object(m) => m.clone(),
                other => {
                    return Err(EvalError::new(format!(
                        "cannot update field of {}",
                        other.type_name()
                    )));
                }
            };
            match obj.iter_mut().find(|(kk, _)| kk == k) {
                Some(pair) => pair.1 = update_path(&pair.1, rest, f)?,
                None => return Err(EvalError::new(format!("no such key: \"{k}\""))),
            }
            Ok(Value::Object(obj))
        }
        Step::Index(i) => {
            let mut arr = match v {
                Value::Array(a) => a.clone(),
                other => {
                    return Err(EvalError::new(format!(
                        "cannot index {} with a number",
                        other.type_name()
                    )));
                }
            };
            let idx = crate::resolve_index(*i, arr.len())
                .ok_or_else(|| EvalError::new("array index out of range"))?;
            arr[idx] = update_path(&arr[idx], rest, f)?;
            Ok(Value::Array(arr))
        }
        Step::Iterate => match v {
            Value::Array(a) => {
                let mut out = Vec::with_capacity(a.len());
                for e in a {
                    out.push(update_path(e, rest, f)?);
                }
                Ok(Value::Array(out))
            }
            Value::Object(m) => {
                let mut out = Vec::with_capacity(m.len());
                for (k, e) in m {
                    out.push((k.clone(), update_path(e, rest, f)?));
                }
                Ok(Value::Object(out))
            }
            other => Err(EvalError::new(format!(
                "cannot iterate over {}",
                other.type_name()
            ))),
        },
        Step::Comment(_) => Err(EvalError::new(comment_mutation_unsupported())),
    }
}

fn eval_path(steps: &[Step], input: &Value) -> Result<Vec<Value>, EvalError> {
    let mut stream = vec![input.clone()];
    for step in steps {
        let mut next = Vec::new();
        for v in &stream {
            next.extend(apply_step(step, v)?);
        }
        stream = next;
    }
    Ok(stream)
}

fn apply_step(step: &Step, v: &Value) -> Result<Vec<Value>, EvalError> {
    match step {
        Step::Field(k) => match v {
            Value::Object(m) => Ok(m
                .iter()
                .find(|(kk, _)| kk == k)
                .map(|(_, val)| vec![val.clone()])
                .unwrap_or_default()),
            Value::Null => Ok(vec![]),
            other => Err(EvalError::new(format!(
                "cannot index {} with \"{k}\"",
                other.type_name()
            ))),
        },
        Step::Index(i) => match v {
            Value::Array(a) => Ok(crate::resolve_index(*i, a.len())
                .map(|idx| a[idx].clone())
                .into_iter()
                .collect()),
            Value::Null => Ok(vec![]),
            other => Err(EvalError::new(format!(
                "cannot index {} with a number",
                other.type_name()
            ))),
        },
        Step::Iterate => match v {
            Value::Array(a) => Ok(a.clone()),
            Value::Object(m) => Ok(m.iter().map(|(_, val)| val.clone()).collect()),
            other => Err(EvalError::new(format!(
                "cannot iterate over {}",
                other.type_name()
            ))),
        },
        // A comment step is resolved against the document's commented
        // projection, not the value stream; see `eval_with_comments`. Reaching
        // it here means it was used in a spot the value evaluator can't serve.
        Step::Comment(_) => Err(EvalError::new(
            "comment access (`#`) resolves only as a whole path like `.foo.#`, \
             not after a pipe over a value",
        )),
    }
}

/// Expand a path containing `Step::Iterate` into **concrete** iterate-free
/// paths, one per iterated element, against `value`. This is the edit-path
/// analogue of `Step::Iterate` evaluation: an array iterate becomes `.a[0]`,
/// `.a[1]`, ...; an object iterate becomes `.a."key"`, ..., so a caller can
/// apply a set/update per element with machinery that only handles index-keyed
/// targets. A non-iterate step that misses (absent key, out-of-range index)
/// yields no paths, like a query miss; stepping *into* the wrong type errors,
/// matching evaluation.
pub fn expand_iter_paths(steps: &[Step], value: &Value) -> Result<Vec<Vec<Step>>, EvalError> {
    let mut out = Vec::new();
    let mut acc = Vec::new();
    expand_iter_walk(steps, value, &mut acc, &mut out)?;
    Ok(out)
}

/// The concrete paths for a **delete** fan-out: [`expand_iter_paths`] reversed,
/// so a caller deletes from the back forward and earlier concrete paths'
/// indices (or keys) stay valid as the collection shrinks under the splices.
pub fn expand_delete_paths(steps: &[Step], value: &Value) -> Result<Vec<Vec<Step>>, EvalError> {
    let mut paths = expand_iter_paths(steps, value)?;
    paths.reverse();
    Ok(paths)
}

/// Depth-first walk appending each complete concrete path to `out`. Mutating
/// `acc` on entry/exit (push/pop) keeps the per-branch allocation to one Vec.
fn expand_iter_walk(
    steps: &[Step],
    v: &Value,
    acc: &mut Vec<Step>,
    out: &mut Vec<Vec<Step>>,
) -> Result<(), EvalError> {
    match steps.split_first() {
        None => {
            out.push(acc.to_vec());
            Ok(())
        }
        Some((step, rest)) => match step {
            Step::Iterate => match v {
                Value::Array(a) => {
                    for (i, elem) in a.iter().enumerate() {
                        acc.push(Step::Index(i as i64));
                        expand_iter_walk(rest, elem, acc, out)?;
                        acc.pop();
                    }
                    Ok(())
                }
                Value::Object(m) => {
                    for (k, val) in m {
                        acc.push(Step::Field(k.clone()));
                        expand_iter_walk(rest, val, acc, out)?;
                        acc.pop();
                    }
                    Ok(())
                }
                other => Err(EvalError::new(format!(
                    "cannot iterate over {}",
                    other.type_name()
                ))),
            },
            // A field/index step: descend the single value it resolves to. A
            // miss (absent key, out-of-range index) is a dead branch: this
            // entire path list contributes nothing, like a query miss. A type
            // error (indexing a scalar) still propagates.
            _ => {
                let next = apply_step(step, v)?.into_iter().next();
                match next {
                    None => Ok(()),
                    Some(next) => {
                        acc.push(step.clone());
                        expand_iter_walk(rest, &next, acc, out)?;
                        acc.pop();
                        Ok(())
                    }
                }
            }
        },
    }
}

fn negate(v: &Value) -> Result<Value, EvalError> {
    match v {
        Value::Int(i) => Ok(Value::Int(-i)),
        Value::Float(f) => Ok(Value::Float(-f)),
        other => Err(EvalError::new(format!(
            "cannot negate {}",
            other.type_name()
        ))),
    }
}

fn binary(op: BinOp, a: &Value, b: &Value) -> Result<Value, EvalError> {
    match op {
        BinOp::Eq => Ok(Value::Bool(a.value_eq(b))),
        BinOp::Ne => Ok(Value::Bool(!a.value_eq(b))),
        BinOp::Lt => Ok(Value::Bool(a.order(b) == Ordering::Less)),
        BinOp::Gt => Ok(Value::Bool(a.order(b) == Ordering::Greater)),
        BinOp::Le => Ok(Value::Bool(a.order(b) != Ordering::Greater)),
        BinOp::Ge => Ok(Value::Bool(a.order(b) != Ordering::Less)),
        BinOp::Add => add(a, b),
        BinOp::Sub => arith(a, b, |x, y| x - y, i64::checked_sub, "subtract"),
        BinOp::Mul => arith(a, b, |x, y| x * y, i64::checked_mul, "multiply"),
        BinOp::Div => divide(a, b),
        BinOp::Mod => modulo(a, b),
    }
}

/// `+` is overloaded: `null` is the identity, plus numeric addition, string
/// concat, and array concat.
pub(crate) fn add(a: &Value, b: &Value) -> Result<Value, EvalError> {
    match (a, b) {
        (Value::Null, _) => Ok(b.clone()),
        (_, Value::Null) => Ok(a.clone()),
        (Value::Str(x), Value::Str(y)) => Ok(Value::Str(format!("{x}{y}"))),
        (Value::Array(x), Value::Array(y)) => {
            let mut v = x.clone();
            v.extend(y.clone());
            Ok(Value::Array(v))
        }
        _ => arith(a, b, |x, y| x + y, i64::checked_add, "add"),
    }
}

fn arith(
    a: &Value,
    b: &Value,
    f: impl Fn(f64, f64) -> f64,
    checked: impl Fn(i64, i64) -> Option<i64>,
    verb: &str,
) -> Result<Value, EvalError> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => match checked(*x, *y) {
            Some(r) => Ok(Value::Int(r)),
            None => Ok(Value::Float(f(*x as f64, *y as f64))),
        },
        _ => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => Ok(Value::Float(f(x, y))),
            _ => Err(EvalError::new(format!(
                "cannot {verb} {} and {}",
                a.type_name(),
                b.type_name()
            ))),
        },
    }
}

fn divide(a: &Value, b: &Value) -> Result<Value, EvalError> {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => {
            if y == 0.0 {
                return Err(EvalError::new("division by zero"));
            }
            // Keep an integer result when both sides are integers and it divides
            // evenly; otherwise a float, like most calculators.
            match (a, b) {
                (Value::Int(xi), Value::Int(yi)) if *xi % *yi == 0 => Ok(Value::Int(*xi / *yi)),
                _ => Ok(Value::Float(x / y)),
            }
        }
        _ => Err(EvalError::new(format!(
            "cannot divide {} and {}",
            a.type_name(),
            b.type_name()
        ))),
    }
}

fn modulo(a: &Value, b: &Value) -> Result<Value, EvalError> {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => {
            if y == 0.0 {
                return Err(EvalError::new("division by zero"));
            }
            if let (Value::Int(xi), Value::Int(yi)) = (a, b) {
                return Ok(Value::Int(*xi % *yi));
            }
            Ok(Value::Float(x % y))
        }
        _ => Err(EvalError::new(format!(
            "cannot mod {} and {}",
            a.type_name(),
            b.type_name()
        ))),
    }
}

// The builtin function registry lives in `crate::builtins` (eval_call and its
// helpers); this file holds the core evaluator, path application, and the
// value-model mutation semantics. Split along that seam in the audit pass.

#[cfg(test)]
mod tests;
