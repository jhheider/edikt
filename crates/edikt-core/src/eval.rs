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
            let idx = if *i < 0 { arr.len() as i64 + i } else { *i };
            if idx < 0 {
                return Err(EvalError::new("array index out of range"));
            }
            let idx = idx as usize;
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
            let idx = if *i < 0 { arr.len() as i64 + i } else { *i };
            if idx < 0 || idx as usize >= arr.len() {
                return Err(EvalError::new("array index out of range"));
            }
            let idx = idx as usize;
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
            Value::Array(a) => {
                let idx = if *i < 0 { a.len() as i64 + i } else { *i };
                if idx >= 0 && (idx as usize) < a.len() {
                    Ok(vec![a[idx as usize].clone()])
                } else {
                    Ok(vec![])
                }
            }
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
fn add(a: &Value, b: &Value) -> Result<Value, EvalError> {
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
mod tests {
    use super::*;
    use crate::parser::parse;

    fn obj(pairs: &[(&str, Value)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn run(expr: &str, input: &Value) -> Vec<Value> {
        eval(&parse(expr).unwrap(), input).unwrap_or_else(|e| panic!("eval `{expr}`: {e}"))
    }

    fn one(expr: &str, input: &Value) -> Value {
        let r = run(expr, input);
        assert_eq!(r.len(), 1, "`{expr}` should yield one value, got {r:?}");
        r.into_iter().next().unwrap()
    }

    #[test]
    fn navigation() {
        let doc = obj(&[(
            "compilerOptions",
            obj(&[
                ("strict", Value::Bool(true)),
                ("target", Value::Str("ES2020".into())),
            ]),
        )]);
        assert_eq!(one(".compilerOptions.strict", &doc), Value::Bool(true));
        assert_eq!(
            one(".compilerOptions.target", &doc),
            Value::Str("ES2020".into())
        );
    }

    #[test]
    fn missing_is_empty_stream() {
        // A missing key (or OOB index) is a miss -> empty stream. Indexing a
        // scalar is a *type error*, tested separately in `type_errors`.
        let doc = obj(&[("a", obj(&[("x", Value::Int(1))]))]);
        assert!(run(".nope", &doc).is_empty());
        assert!(run(".a.nope", &doc).is_empty());
        assert!(run(".a.nope.deeper", &doc).is_empty());
    }

    #[test]
    fn explicit_null_is_a_value() {
        let doc = obj(&[("a", Value::Null)]);
        assert_eq!(run(".a", &doc), vec![Value::Null]);
    }

    #[test]
    fn iterate_and_index() {
        let doc = obj(&[(
            "lib",
            Value::Array(vec![Value::Str("ES2020".into()), Value::Str("DOM".into())]),
        )]);
        assert_eq!(
            run(".lib[]", &doc),
            vec![Value::Str("ES2020".into()), Value::Str("DOM".into())]
        );
        assert_eq!(one(".lib[0]", &doc), Value::Str("ES2020".into()));
        assert_eq!(one(".lib[-1]", &doc), Value::Str("DOM".into()));
        assert!(run(".lib[9]", &doc).is_empty());
    }

    #[test]
    fn select_filter() {
        let doc = obj(&[(
            "items",
            Value::Array(vec![
                obj(&[
                    ("name", Value::Str("keep".into())),
                    ("on", Value::Bool(true)),
                ]),
                obj(&[
                    ("name", Value::Str("drop".into())),
                    ("on", Value::Bool(false)),
                ]),
            ]),
        )]);
        let r = run(".items[] | select(.on == true)", &doc);
        assert_eq!(r.len(), 1);
        assert_eq!(one(".name", &r[0]), Value::Str("keep".into()));
    }

    #[test]
    fn arithmetic_and_strings() {
        let doc = obj(&[
            ("count", Value::Int(5)),
            ("name", Value::Str("edikt".into())),
        ]);
        assert_eq!(one(".count + 1", &doc), Value::Int(6));
        assert_eq!(one(".count * 2 - 3", &doc), Value::Int(7));
        assert_eq!(one(".name + \"!\"", &doc), Value::Str("edikt!".into()));
        assert_eq!(
            one(".name | ascii_upcase", &doc),
            Value::Str("EDIKT".into())
        );
        assert_eq!(one(".name | length", &doc), Value::Int(5));
    }

    #[test]
    fn multi_output_comma() {
        let doc = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        assert_eq!(run(".a, .b", &doc), vec![Value::Int(1), Value::Int(2)]);
    }

    #[test]
    fn object_construction() {
        let doc = obj(&[("x", Value::Int(5))]);
        assert_eq!(
            one("{ a: 1, b: .x }", &doc),
            obj(&[("a", Value::Int(1)), ("b", Value::Int(5))])
        );
        assert_eq!(one("{}", &doc), Value::Object(vec![]));
    }

    #[test]
    fn bracket_string_keys() {
        let doc = obj(&[("weird.key", Value::Str("w".into()))]);
        assert_eq!(one(r#".["weird.key"]"#, &doc), Value::Str("w".into()));
    }

    #[test]
    fn builtins() {
        let doc = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        assert_eq!(
            one("keys", &doc),
            Value::Array(vec![Value::Str("a".into()), Value::Str("b".into())])
        );
        assert_eq!(one("has(\"a\")", &doc), Value::Bool(true));
        assert_eq!(one("type", &doc), Value::Str("object".into()));
        assert_eq!(one("length", &doc), Value::Int(2));
        assert_eq!(one("\"12\" | tonumber", &Value::Null), Value::Int(12));
        assert_eq!(
            one("\"pre-x\" | ltrimstr(\"pre-\")", &Value::Null),
            Value::Str("x".into())
        );
    }

    #[test]
    fn type_errors() {
        assert!(eval(&parse(".a").unwrap(), &Value::Int(3)).is_err());
        assert!(eval(&parse(".[]").unwrap(), &Value::Int(3)).is_err());
        assert!(eval(&parse("length").unwrap(), &Value::Int(3)).is_err());
    }

    #[test]
    fn value_level_set_paths() {
        // Create nested keys through null / missing; extend arrays with nulls.
        let r = run(".a.b.c = 1", &Value::Null);
        assert_eq!(one(".a.b.c", &r[0]), Value::Int(1));
        let arr = run(
            ".xs[2] = 9",
            &obj(&[("xs", Value::Array(vec![Value::Int(0)]))]),
        );
        assert_eq!(
            one(".xs", &arr[0]),
            Value::Array(vec![Value::Int(0), Value::Null, Value::Int(9)])
        );
        // Iterate-assignment sets every element / value.
        let it = run(".[] = 0", &Value::Array(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(it[0], Value::Array(vec![Value::Int(0), Value::Int(0)]));
        let ito = run(
            ".[] = 0",
            &obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]),
        );
        assert_eq!(ito[0], obj(&[("a", Value::Int(0)), ("b", Value::Int(0))]));
        // Negative index out of range, and setting through the wrong type, error.
        assert!(
            eval(
                &parse(".xs[-9] = 1").unwrap(),
                &obj(&[("xs", Value::Array(vec![]))])
            )
            .is_err()
        );
        assert!(eval(&parse(".a = 1").unwrap(), &Value::Int(3)).is_err());
        assert!(eval(&parse(".[0] = 1").unwrap(), &Value::Str("x".into())).is_err());
    }

    #[test]
    fn value_level_update_and_delete() {
        // |= over a field, an index, and an iterate.
        assert_eq!(
            one(".a |= . + 1", &obj(&[("a", Value::Int(1))])),
            obj(&[("a", Value::Int(2))])
        );
        let xs = obj(&[("xs", Value::Array(vec![Value::Int(1), Value::Int(2)]))]);
        assert_eq!(
            one(".xs[0] |= . * 10", &xs),
            obj(&[("xs", Value::Array(vec![Value::Int(10), Value::Int(2)]))])
        );
        assert_eq!(
            one(".xs[] |= . + 1", &xs),
            obj(&[("xs", Value::Array(vec![Value::Int(2), Value::Int(3)]))])
        );
        // del of a nested key, an index, an iterate, and a miss (no-op).
        assert_eq!(
            one(
                "del(.a.b)",
                &obj(&[("a", obj(&[("b", Value::Int(1)), ("c", Value::Int(2))]))])
            ),
            obj(&[("a", obj(&[("c", Value::Int(2))]))])
        );
        assert_eq!(
            one("del(.xs[0])", &xs),
            obj(&[("xs", Value::Array(vec![Value::Int(2)]))])
        );
        assert_eq!(one("del(.xs[])", &xs), obj(&[("xs", Value::Array(vec![]))]));
        assert_eq!(
            one("del(.nope)", &obj(&[("a", Value::Int(1))])),
            obj(&[("a", Value::Int(1))])
        );
        // Update through the wrong type errors.
        assert!(eval(&parse(".a |= .").unwrap(), &Value::Int(3)).is_err());
    }

    #[test]
    fn arithmetic_and_its_errors() {
        assert_eq!(one("3 - 1", &Value::Null), Value::Int(2));
        assert_eq!(one("3 * 4", &Value::Null), Value::Int(12));
        assert_eq!(one("7 % 3", &Value::Null), Value::Int(1));
        assert_eq!(one("6 / 2", &Value::Null), Value::Int(3)); // even -> int
        assert_eq!(one("7 / 2", &Value::Null), Value::Float(3.5)); // uneven -> float
        assert_eq!(one("2.5 + 0.5", &Value::Null), Value::Int(3)); // 3.0 prints as int
        // Comparisons.
        assert_eq!(one("1 < 2", &Value::Null), Value::Bool(true));
        assert_eq!(one("2 <= 2", &Value::Null), Value::Bool(true));
        assert_eq!(one("3 >= 4", &Value::Null), Value::Bool(false));
        assert_eq!(one("1 != 2", &Value::Null), Value::Bool(true));
        // Division / modulo by zero, and non-numeric arithmetic, error.
        assert!(eval(&parse("1 / 0").unwrap(), &Value::Null).is_err());
        assert!(eval(&parse("1 % 0").unwrap(), &Value::Null).is_err());
        assert!(eval(&parse("\"a\" - 1").unwrap(), &Value::Null).is_err());
        assert!(eval(&parse("-\"a\"").unwrap(), &Value::Null).is_err());
        // Overflow promotes to float rather than panicking.
        assert!(matches!(
            one("9223372036854775807 + 1", &Value::Null),
            Value::Float(_)
        ));
    }

    #[test]
    fn add_is_overloaded() {
        assert_eq!(one("null + 5", &Value::Null), Value::Int(5));
        assert_eq!(one("5 + null", &Value::Null), Value::Int(5));
        assert_eq!(one("\"a\" + \"b\"", &Value::Null), Value::Str("ab".into()));
        assert_eq!(
            one("[1] + [2]", &Value::Null),
            Value::Array(vec![Value::Int(1), Value::Int(2)])
        );
    }

    #[test]
    fn builtin_error_and_edge_paths() {
        // has / keys / length on the wrong type, and ltrimstr/rtrimstr edges.
        assert!(eval(&parse("has(\"a\")").unwrap(), &Value::Int(1)).is_err());
        assert!(eval(&parse("keys").unwrap(), &Value::Int(1)).is_err());
        assert_eq!(
            one("has(1)", &Value::Array(vec![Value::Int(0), Value::Int(0)])),
            Value::Bool(true)
        );
        assert_eq!(
            one("\"abc\" | ltrimstr(\"x\")", &Value::Null),
            Value::Str("abc".into())
        );
        assert_eq!(
            one("\"abc\" | rtrimstr(\"bc\")", &Value::Null),
            Value::Str("a".into())
        );
        assert_eq!(one("\"42\" | tonumber", &Value::Null), Value::Int(42));
        assert!(eval(&parse("\"x\" | tonumber").unwrap(), &Value::Null).is_err());
        assert_eq!(one("length", &Value::Str("héllo".into())), Value::Int(5));
        assert_eq!(one("length", &Value::Null), Value::Int(0));
        // Unknown function and wrong arity.
        assert!(eval(&parse("nope").unwrap(), &Value::Null).is_err());
        assert!(eval(&parse("length(1)").unwrap(), &Value::Null).is_err());
    }

    #[test]
    fn alternative_operator() {
        let doc = obj(&[
            ("a", Value::Int(1)),
            ("z", Value::Null),
            ("f", Value::Bool(false)),
        ]);
        // A present, truthy value wins.
        assert_eq!(one(r#".a // "d""#, &doc), Value::Int(1));
        // A miss, null, and false all fall back.
        assert_eq!(one(r#".nope // "d""#, &doc), Value::Str("d".into()));
        assert_eq!(one(r#".z // "d""#, &doc), Value::Str("d".into()));
        assert_eq!(one(r#".f // "d""#, &doc), Value::Str("d".into()));
        // Right-associative chain.
        assert_eq!(
            one(r#".x // .y // "last""#, &doc),
            Value::Str("last".into())
        );
        // Binds tighter than `=`: the RHS gets the default.
        let r = run(r#".k = .nope // "d""#, &doc);
        assert_eq!(one(".k", &r[0]), Value::Str("d".into()));
        // Binds looser than comparison: `.a == 2 // "d"` is ((.a == 2)) // "d".
        assert_eq!(one(r#".a == 2 // "d""#, &doc), Value::Str("d".into()));
        // Filters a stream to its truthy members before falling back.
        let items = obj(&[(
            "xs",
            Value::Array(vec![Value::Bool(false), Value::Int(7), Value::Null]),
        )]);
        assert_eq!(run(r#".xs[] // "d""#, &items), vec![Value::Int(7)]);
        // A type error on the left still propagates: a miss falls back, a
        // mistake doesn't hide.
        assert!(eval(&parse(r#".a.b // "d""#).unwrap(), &doc).is_err());
    }

    #[test]
    fn comments_stream_records_and_paths() {
        use crate::comment::{Commented, CommentedNode, Comments};
        // A little commented tree: web (head), web.image (inline), debug (inline).
        let img = Commented {
            comments: Comments {
                head: vec![],
                inline: Some("pinned".into()),
                foot: vec![],
            },
            node: CommentedNode::Scalar(Value::Str("nginx".into())),
        };
        let web = Commented {
            comments: Comments {
                head: vec!["the service".into()],
                inline: None,
                foot: vec![],
            },
            node: CommentedNode::Object(vec![("image".into(), img)]),
        };
        let debug = Commented {
            comments: Comments {
                head: vec![],
                inline: Some("TODO remove".into()),
                foot: vec![],
            },
            node: CommentedNode::Scalar(Value::Bool(false)),
        };
        let root = Commented {
            comments: Comments::default(),
            node: CommentedNode::Object(vec![("web".into(), web), ("debug".into(), debug)]),
        };

        // The stream yields one record per comment, in document order.
        let recs = comment_records(&root);
        assert_eq!(recs.len(), 3);
        // comment -> key: which paths carry a TODO?
        let todos = eval_with_comments(
            &parse(r#"comments | select(.text | test("TODO")) | .path"#).unwrap(),
            &root,
        )
        .unwrap();
        assert_eq!(todos, vec![Value::Str(".debug".into())]);
        // paths render as re-usable expressions.
        let paths = eval_with_comments(&parse("comments | .path").unwrap(), &root).unwrap();
        assert_eq!(
            paths,
            vec![
                Value::Str(".web".into()),
                Value::Str(".web.image".into()),
                Value::Str(".debug".into()),
            ]
        );
        // collectable.
        assert_eq!(
            eval_with_comments(&parse("[comments] | length").unwrap(), &root).unwrap(),
            vec![Value::Int(3)]
        );
    }

    #[test]
    fn regex_test_match_capture() {
        let s = Value::Str("nginx:1.25".into());
        assert_eq!(one(r#"test("^nginx")"#, &s), Value::Bool(true));
        assert_eq!(one(r#"test("^NGINX")"#, &s), Value::Bool(false));
        // The `;`-separated flags argument, jq-style.
        assert_eq!(one(r#"test("^NGINX"; "i")"#, &s), Value::Bool(true));

        // match: no match -> empty stream (a silent miss at the CLI); `g`
        // streams every match.
        assert!(run(r#"match("\\d+"; "g")"#, &Value::Str("a1b22".into())).len() == 2);
        assert!(run(r#"match("z")"#, &s).is_empty());
        let m = one(r#"match(":(\\d+)")"#, &s);
        assert_eq!(one(".offset", &m), Value::Int(5));
        assert_eq!(one(".string", &m), Value::Str(":1".into()));
        assert_eq!(one(".captures[0].string", &m), Value::Str("1".into()));

        // capture: named groups as an object.
        assert_eq!(
            one(r#"capture("(?<img>\\w+):(?<tag>.+)")"#, &s),
            obj(&[
                ("img", Value::Str("nginx".into())),
                ("tag", Value::Str("1.25".into())),
            ])
        );

        // errors: bad regex, bad flag, non-string input
        assert!(eval(&parse(r#"test("(")"#).unwrap(), &s).is_err());
        assert!(eval(&parse(r#"test("a"; "q")"#).unwrap(), &s).is_err());
        assert!(eval(&parse(r#"test("a")"#).unwrap(), &Value::Int(1)).is_err());
    }

    #[test]
    fn regex_sub_and_gsub() {
        let v = Value::Str("v1.2.3".into());
        assert_eq!(one(r#"sub("^v"; "")"#, &v), Value::Str("1.2.3".into()));
        // sub replaces the first; gsub all; `$name` references captures.
        let s = Value::Str("a-b-c".into());
        assert_eq!(one(r#"sub("-"; "_")"#, &s), Value::Str("a_b-c".into()));
        assert_eq!(one(r#"gsub("-"; "_")"#, &s), Value::Str("a_b_c".into()));
        assert_eq!(
            one(
                r#"sub("(?<k>\\w+)=(?<v>\\w+)"; "${v}:${k}")"#,
                &Value::Str("port=80".into())
            ),
            Value::Str("80:port".into())
        );
    }

    #[test]
    fn split_join_and_affixes() {
        let path = Value::Str("/usr/bin:/bin".into());
        assert_eq!(
            one(r#"split(":")"#, &path),
            Value::Array(vec![
                Value::Str("/usr/bin".into()),
                Value::Str("/bin".into()),
            ])
        );
        // The round trip real configs want: split, extend, join.
        assert_eq!(
            one(r#"split(":") + ["/sbin"] | join(":")"#, &path),
            Value::Str("/usr/bin:/bin:/sbin".into())
        );
        // 2-arg split is regex (jq's shape).
        assert_eq!(
            one(r#""a1b22c" | split("\\d+"; "")"#, &Value::Null),
            Value::Array(vec![
                Value::Str("a".into()),
                Value::Str("b".into()),
                Value::Str("c".into()),
            ])
        );
        assert_eq!(
            one(r#""VITE_PORT" | startswith("VITE_")"#, &Value::Null),
            Value::Bool(true)
        );
        assert_eq!(
            one(r#""app.log" | endswith(".log")"#, &Value::Null),
            Value::Bool(true)
        );
        // join stringifies scalars and rejects containers.
        assert!(eval(&parse(r#"join(",")"#).unwrap(), &Value::Str("x".into())).is_err());
    }

    // --- mutation (Value-level semantics) ---------------------------------

    #[test]
    fn assign_sets_and_leaves_siblings() {
        let doc = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        let r = run(".a = 5", &doc);
        assert_eq!(one(".a", &r[0]), Value::Int(5));
        assert_eq!(one(".b", &r[0]), Value::Int(2));
    }

    #[test]
    fn assign_creates_missing_key() {
        let doc = obj(&[("a", Value::Int(1))]);
        let r = run(".c = 9", &doc);
        assert_eq!(one(".c", &r[0]), Value::Int(9));
    }

    #[test]
    fn assign_rhs_evaluated_against_input() {
        let doc = obj(&[("a", Value::Int(1)), ("b", Value::Int(7))]);
        let r = run(".a = .b", &doc);
        assert_eq!(one(".a", &r[0]), Value::Int(7));
    }

    #[test]
    fn assign_into_array_index() {
        let doc = obj(&[("a", Value::Array(vec![Value::Int(1), Value::Int(2)]))]);
        let r = run(".a[0] = 9", &doc);
        assert_eq!(one(".a[0]", &r[0]), Value::Int(9));
        assert_eq!(one(".a[1]", &r[0]), Value::Int(2));
    }

    #[test]
    fn update_assign_computes_and_maps() {
        let doc = obj(&[
            ("count", Value::Int(5)),
            ("name", Value::Str("edikt".into())),
        ]);
        let r = run(".count |= . + 1", &doc);
        assert_eq!(one(".count", &r[0]), Value::Int(6));
        let r2 = run(".name |= ascii_upcase", &doc);
        assert_eq!(one(".name", &r2[0]), Value::Str("EDIKT".into()));
    }

    #[test]
    fn mutation_detection() {
        assert!(parse(".a = 1").unwrap().is_mutation());
        assert!(parse(".a |= . + 1").unwrap().is_mutation());
        assert!(parse("del(.a)").unwrap().is_mutation());
        assert!(!parse(".a.b").unwrap().is_mutation());
        assert!(!parse(".items[] | select(. == 1)").unwrap().is_mutation());
    }

    #[test]
    fn assign_lhs_must_be_path() {
        // `1 = 2`: the left side is a literal, not a path.
        assert!(eval(&parse("1 = 2").unwrap(), &Value::Null).is_err());
    }

    #[test]
    fn del_removes_key_and_index() {
        let doc = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        let r = run("del(.a)", &doc);
        assert!(run(".a", &r[0]).is_empty());
        assert_eq!(one(".b", &r[0]), Value::Int(2));

        let arr = obj(&[(
            "x",
            Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)]),
        )]);
        let r2 = run("del(.x[1])", &arr);
        assert_eq!(run(".x[]", &r2[0]), vec![Value::Int(10), Value::Int(30)]);
    }

    #[test]
    fn del_missing_is_noop() {
        let doc = obj(&[("a", Value::Int(1))]);
        let r = run("del(.nope)", &doc);
        assert_eq!(one(".a", &r[0]), Value::Int(1));
    }

    #[test]
    fn del_nested() {
        let doc = obj(&[("a", obj(&[("b", Value::Int(1)), ("c", Value::Int(2))]))]);
        let r = run("del(.a.b)", &doc);
        assert!(run(".a.b", &r[0]).is_empty());
        assert_eq!(one(".a.c", &r[0]), Value::Int(2));
    }

    #[test]
    fn add_assign_number_string_array() {
        let doc = obj(&[
            ("count", Value::Int(5)),
            ("name", Value::Str("edikt".into())),
            ("list", Value::Array(vec![Value::Int(1)])),
        ]);
        assert_eq!(one(".count", &run(".count += 3", &doc)[0]), Value::Int(8));
        assert_eq!(
            one(".name", &run(".name += \"!\"", &doc)[0]),
            Value::Str("edikt!".into())
        );
        let appended = run(".list += [2, 3]", &doc);
        assert_eq!(
            run(".list[]", &appended[0]),
            vec![Value::Int(1), Value::Int(2), Value::Int(3)]
        );
    }

    #[test]
    fn add_assign_null_identity() {
        // A missing key is `null`; `null + [x] == [x]`, so `+=` creates it.
        let doc = obj(&[("a", Value::Int(1))]);
        let r = run(".tags += [\"x\"]", &doc);
        assert_eq!(run(".tags[]", &r[0]), vec![Value::Str("x".into())]);
    }

    // --- expand_iter_paths ------------------------------------------------

    fn paths(steps: &[Step], value: &Value) -> Vec<Vec<Step>> {
        expand_iter_paths(steps, value).unwrap_or_default()
    }

    fn field(k: &str) -> Step {
        Step::Field(k.into())
    }

    #[test]
    fn expand_iter_paths_fans_out_arrays_objects_and_nests() {
        let doc = obj(&[("a", Value::Array(vec![Value::Int(1), Value::Int(2)]))]);
        assert_eq!(
            paths(&[field("a"), Step::Iterate], &doc),
            vec![
                vec![field("a"), Step::Index(0)],
                vec![field("a"), Step::Index(1)],
            ]
        );

        let objdoc = obj(&[(
            "o",
            Value::Object(vec![
                ("x".into(), Value::Int(1)),
                ("y".into(), Value::Int(2)),
            ]),
        )]);
        assert_eq!(
            paths(&[field("o"), Step::Iterate], &objdoc),
            vec![vec![field("o"), field("x")], vec![field("o"), field("y")],]
        );

        // A nested iterate expands depth-first, preserving element order.
        let nested = obj(&[(
            "a",
            Value::Array(vec![
                Value::Object(vec![("b".into(), Value::Int(1))]),
                Value::Object(vec![("b".into(), Value::Int(2))]),
            ]),
        )]);
        assert_eq!(
            paths(&[field("a"), Step::Iterate, field("b")], &nested),
            vec![
                vec![field("a"), Step::Index(0), field("b")],
                vec![field("a"), Step::Index(1), field("b")],
            ]
        );

        // Resolves to nothing when a pre-iterate key is absent (a miss).
        assert!(paths(&[field("nope"), Step::Iterate], &doc).is_empty());
        // Iterating a scalar errors, matching evaluation.
        assert!(
            expand_iter_paths(&[field("a"), Step::Iterate], &obj(&[("a", Value::Int(1))])).is_err()
        );

        // The delete fan-out is the same expansion, **reversed** (so delete
        // goes back-to-front and earlier indices stay valid as the collection
        // shrinks).
        assert_eq!(
            expand_delete_paths(&[field("a"), Step::Iterate], &doc).unwrap(),
            vec![
                vec![field("a"), Step::Index(1)],
                vec![field("a"), Step::Index(0)],
            ]
        );
    }

    // --- non-finite number literals (JSON5) -------------------------------

    #[test]
    fn nonfinite_number_literals() {
        // `Infinity`/`-Infinity`/`NaN` are JSON5 number literals in the value
        // calculus (lexed as `Num`).
        assert_eq!(one("Infinity", &Value::Null), Value::Float(f64::INFINITY));
        assert_eq!(
            one("-Infinity", &Value::Null),
            Value::Float(f64::NEG_INFINITY)
        );
        assert_eq!(one("NaN", &Value::Null), Value::Float(f64::NAN));
        // They compose: arithmetic, arrays, object constructs, assignment.
        assert_eq!(
            one("Infinity + 1", &Value::Null),
            Value::Float(f64::INFINITY)
        );
        assert_eq!(
            one("[Infinity, NaN]", &Value::Null),
            Value::Array(vec![Value::Float(f64::INFINITY), Value::Float(f64::NAN)])
        );
        let doc = obj(&[("a", Value::Int(1))]);
        assert_eq!(
            one(".b = {n: -Infinity}", &doc),
            Value::Object(vec![
                ("a".into(), Value::Int(1)),
                (
                    "b".into(),
                    Value::Object(vec![("n".into(), Value::Float(f64::NEG_INFINITY))]),
                ),
            ])
        );
        // `Infinity` is a number, not an identifier: a bare `.Infinity` cannot
        // be a path (the field needs quoting, as in the JSON5 reader).
        assert!(parse(".Infinity").is_err());
        assert_eq!(
            one(".\"Infinity\"", &obj(&[("Infinity", Value::Int(7))])),
            Value::Int(7)
        );
    }
}
