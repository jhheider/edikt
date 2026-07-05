//! The query evaluator (value calculus) over an in-memory [`Value`].
//!
//! jq-style generator semantics: every expression maps one input value to a
//! *stream* of output values (0, 1, or many), collected here into a `Vec`.
//! A miss (missing key, out-of-range index) yields an **empty stream**, not
//! `null` — the CLI renders it as a silent no-op (sed-shaped), and `//`
//! supplies defaults. An explicit `null` in the document still yields `null`.
//!
//! Mutation `=`, `|=`, and `del` are handled here at the value level — this
//! defines the *semantics* (what value ends up where). The format-preserving CST
//! *write* path lives in the format modules and mirrors these rules. `+=`
//! arrives in a later slice.

use crate::ast::{BinOp, Expr, Step};
use crate::comment::Commented;
use crate::strings;
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

/// Evaluate `expr` against `input`, returning the output stream.
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
        _ => Err(EvalError::new(
            "comment access (`#`) is a read in v0.2 Phase 1: use `.path.#` \
             optionally piped (`| f`) or defaulted (`// x`)",
        )),
    }
}

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
            // jq's `//`: the left side's truthy outputs; if there are none —
            // a miss, `null`, or `false` — the right side's. A type *error*
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
        // projection, not the value stream — see `eval_with_comments`. Reaching
        // it here means it was used in a spot the value evaluator can't serve.
        Step::Comment(_) => Err(EvalError::new(
            "comment access (`#`) resolves only as a whole path like `.foo.#`, \
             not after a pipe over a value",
        )),
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

fn eval_call(name: &str, args: &[Expr], input: &Value) -> Result<Vec<Value>, EvalError> {
    let arity = |n: usize| -> Result<(), EvalError> {
        if args.len() == n {
            Ok(())
        } else {
            Err(EvalError::new(format!(
                "{name} takes {n} argument(s), got {}",
                args.len()
            )))
        }
    };
    // For builtins with a trailing optional argument (regex flags).
    let arity_between = |min: usize, max: usize| -> Result<(), EvalError> {
        if (min..=max).contains(&args.len()) {
            Ok(())
        } else {
            Err(EvalError::new(format!(
                "{name} takes {min}–{max} arguments, got {}",
                args.len()
            )))
        }
    };
    // The optional flags argument, defaulting to none.
    let flags_arg = |at: usize| -> Result<String, EvalError> {
        match args.get(at) {
            Some(a) => str_arg(a, input, "flags"),
            None => Ok(String::new()),
        }
    };

    match name {
        "select" => {
            arity(1)?;
            let mut out = Vec::new();
            for cond in eval(&args[0], input)? {
                if cond.is_truthy() {
                    out.push(input.clone());
                }
            }
            Ok(out)
        }
        "length" => {
            arity(0)?;
            Ok(vec![length(input)?])
        }
        "keys" => {
            arity(0)?;
            Ok(vec![keys(input)?])
        }
        "type" => {
            arity(0)?;
            Ok(vec![Value::Str(input.type_name().to_string())])
        }
        "tostring" => {
            arity(0)?;
            Ok(vec![Value::Str(input.to_raw_string())])
        }
        "tonumber" => {
            arity(0)?;
            Ok(vec![tonumber(input)?])
        }
        "ascii_upcase" => {
            arity(0)?;
            Ok(vec![map_str(input, |s| s.to_uppercase())?])
        }
        "ascii_downcase" => {
            arity(0)?;
            Ok(vec![map_str(input, |s| s.to_lowercase())?])
        }
        "has" => {
            arity(1)?;
            let mut out = Vec::new();
            for key in eval(&args[0], input)? {
                out.push(Value::Bool(has(input, &key)?));
            }
            Ok(out)
        }
        "ltrimstr" => {
            arity(1)?;
            trim_str(input, &args[0], true)
        }
        "rtrimstr" => {
            arity(1)?;
            trim_str(input, &args[0], false)
        }
        "startswith" | "endswith" => {
            arity(1)?;
            let s = str_input(input, name)?;
            let affix = str_arg(&args[0], input, "the affix")?;
            let hit = if name == "startswith" {
                s.starts_with(&affix)
            } else {
                s.ends_with(&affix)
            };
            Ok(vec![Value::Bool(hit)])
        }
        "test" => {
            arity_between(1, 2)?;
            let re = str_arg(&args[0], input, "the regex")?;
            Ok(vec![strings::test(
                str_input(input, name)?,
                &re,
                &flags_arg(1)?,
            )?])
        }
        "match" => {
            arity_between(1, 2)?;
            let re = str_arg(&args[0], input, "the regex")?;
            strings::find(str_input(input, name)?, &re, &flags_arg(1)?)
        }
        "capture" => {
            arity_between(1, 2)?;
            let re = str_arg(&args[0], input, "the regex")?;
            strings::capture(str_input(input, name)?, &re, &flags_arg(1)?)
        }
        "sub" | "gsub" => {
            arity_between(2, 3)?;
            let re = str_arg(&args[0], input, "the regex")?;
            let repl = str_arg(&args[1], input, "the replacement")?;
            let mut flags = flags_arg(2)?;
            if name == "gsub" {
                flags.push('g');
            }
            Ok(vec![strings::sub(
                str_input(input, name)?,
                &re,
                &repl,
                &flags,
            )?])
        }
        "split" => {
            arity_between(1, 2)?;
            let sep = str_arg(&args[0], input, "the separator")?;
            // jq's shape: 1-arg splits on a literal, 2-arg on a regex.
            let regex_flags = if args.len() == 2 {
                Some(flags_arg(1)?)
            } else {
                None
            };
            Ok(vec![strings::split(
                str_input(input, name)?,
                &sep,
                regex_flags.as_deref(),
            )?])
        }
        "join" => {
            arity(1)?;
            let Value::Array(items) = input else {
                return Err(EvalError::new(format!(
                    "join requires an array input, got {}",
                    input.type_name()
                )));
            };
            let sep = str_arg(&args[0], input, "the separator")?;
            Ok(vec![strings::join(items, &sep)?])
        }
        "del" => {
            arity(1)?;
            let steps = args[0]
                .as_path()
                .ok_or_else(|| EvalError::new("del(...) takes a path"))?;
            Ok(vec![delete_path(input, steps)?])
        }
        _ => Err(EvalError::new(format!("unknown function `{name}`"))),
    }
}

/// Return a copy of `v` with the value at `steps` removed. A missing key or
/// out-of-range index is a no-op (jq semantics).
fn delete_path(v: &Value, steps: &[Step]) -> Result<Value, EvalError> {
    let Some((head, rest)) = steps.split_first() else {
        return Err(EvalError::new("del(.) is not allowed"));
    };
    if rest.is_empty() {
        return remove_step(v, head);
    }
    match head {
        Step::Field(k) => {
            let mut obj = match v {
                Value::Object(m) => m.clone(),
                Value::Null => return Ok(Value::Null),
                other => {
                    return Err(EvalError::new(format!(
                        "cannot descend into {}",
                        other.type_name()
                    )));
                }
            };
            if let Some(pair) = obj.iter_mut().find(|(kk, _)| kk == k) {
                pair.1 = delete_path(&pair.1, rest)?;
            }
            Ok(Value::Object(obj))
        }
        Step::Index(i) => {
            let mut arr = match v {
                Value::Array(a) => a.clone(),
                Value::Null => return Ok(Value::Null),
                other => {
                    return Err(EvalError::new(format!(
                        "cannot index {} with a number",
                        other.type_name()
                    )));
                }
            };
            let idx = if *i < 0 { arr.len() as i64 + i } else { *i };
            if idx >= 0 && (idx as usize) < arr.len() {
                let idx = idx as usize;
                arr[idx] = delete_path(&arr[idx], rest)?;
            }
            Ok(Value::Array(arr))
        }
        Step::Iterate => match v {
            Value::Array(a) => {
                let mut out = Vec::with_capacity(a.len());
                for e in a {
                    out.push(delete_path(e, rest)?);
                }
                Ok(Value::Array(out))
            }
            Value::Object(m) => {
                let mut out = Vec::with_capacity(m.len());
                for (k, e) in m {
                    out.push((k.clone(), delete_path(e, rest)?));
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

/// Remove `step` from the container `v` (the leaf of a `del` path).
fn remove_step(v: &Value, step: &Step) -> Result<Value, EvalError> {
    match step {
        Step::Field(k) => match v {
            Value::Object(m) => {
                let kept = m.iter().filter(|(kk, _)| kk != k).cloned().collect();
                Ok(Value::Object(kept))
            }
            Value::Null => Ok(Value::Null),
            other => Err(EvalError::new(format!(
                "cannot delete a field of {}",
                other.type_name()
            ))),
        },
        Step::Index(i) => match v {
            Value::Array(a) => {
                let mut arr = a.clone();
                let idx = if *i < 0 { arr.len() as i64 + i } else { *i };
                if idx >= 0 && (idx as usize) < arr.len() {
                    arr.remove(idx as usize);
                }
                Ok(Value::Array(arr))
            }
            Value::Null => Ok(Value::Null),
            other => Err(EvalError::new(format!(
                "cannot delete an index of {}",
                other.type_name()
            ))),
        },
        Step::Iterate => match v {
            Value::Array(_) => Ok(Value::Array(Vec::new())),
            Value::Object(_) => Ok(Value::Object(Vec::new())),
            other => Err(EvalError::new(format!(
                "cannot iterate over {}",
                other.type_name()
            ))),
        },
        Step::Comment(_) => Err(EvalError::new(comment_mutation_unsupported())),
    }
}

/// The message for a comment edit, which lands in v0.2 Phase 2.
fn comment_mutation_unsupported() -> &'static str {
    "editing comments (`#`) is not supported yet (planned for v0.2); reading works — e.g. `edikt '.foo.#' file`"
}

fn length(v: &Value) -> Result<Value, EvalError> {
    let n = match v {
        Value::Null => 0,
        Value::Str(s) => s.chars().count() as i64,
        Value::Array(a) => a.len() as i64,
        Value::Object(m) => m.len() as i64,
        other => {
            return Err(EvalError::new(format!(
                "{} has no length",
                other.type_name()
            )));
        }
    };
    Ok(Value::Int(n))
}

fn keys(v: &Value) -> Result<Value, EvalError> {
    match v {
        Value::Object(m) => {
            let mut ks: Vec<String> = m.iter().map(|(k, _)| k.clone()).collect();
            ks.sort(); // jq's `keys` is sorted; use `keys_unsorted` later for order
            Ok(Value::Array(ks.into_iter().map(Value::Str).collect()))
        }
        Value::Array(a) => Ok(Value::Array((0..a.len() as i64).map(Value::Int).collect())),
        other => Err(EvalError::new(format!("{} has no keys", other.type_name()))),
    }
}

fn tonumber(v: &Value) -> Result<Value, EvalError> {
    match v {
        Value::Int(_) | Value::Float(_) => Ok(v.clone()),
        Value::Str(s) => {
            let t = s.trim();
            if let Ok(i) = t.parse::<i64>() {
                Ok(Value::Int(i))
            } else if let Ok(f) = t.parse::<f64>() {
                Ok(Value::Float(f))
            } else {
                Err(EvalError::new(format!("cannot parse \"{s}\" as a number")))
            }
        }
        other => Err(EvalError::new(format!(
            "cannot parse {} as a number",
            other.type_name()
        ))),
    }
}

/// The input as a string, for string-only builtins.
fn str_input<'a>(input: &'a Value, name: &str) -> Result<&'a str, EvalError> {
    match input {
        Value::Str(s) => Ok(s),
        other => Err(EvalError::new(format!(
            "{name} requires a string input, got {}",
            other.type_name()
        ))),
    }
}

/// Evaluate an argument expression to a single string (its first value).
fn str_arg(arg: &Expr, input: &Value, what: &str) -> Result<String, EvalError> {
    match eval(arg, input)?.into_iter().next() {
        Some(Value::Str(s)) => Ok(s),
        Some(other) => Err(EvalError::new(format!(
            "{what} must be a string, got {}",
            other.type_name()
        ))),
        None => Err(EvalError::new(format!("{what} produced no value"))),
    }
}

fn map_str(v: &Value, f: impl Fn(&str) -> String) -> Result<Value, EvalError> {
    match v {
        Value::Str(s) => Ok(Value::Str(f(s))),
        other => Err(EvalError::new(format!(
            "expected a string, got {}",
            other.type_name()
        ))),
    }
}

fn has(v: &Value, key: &Value) -> Result<bool, EvalError> {
    match (v, key) {
        (Value::Object(m), Value::Str(k)) => Ok(m.iter().any(|(kk, _)| kk == k)),
        (Value::Array(a), Value::Int(i)) => Ok(*i >= 0 && (*i as usize) < a.len()),
        _ => Err(EvalError::new(format!(
            "cannot check membership of {} in {}",
            key.type_name(),
            v.type_name()
        ))),
    }
}

fn trim_str(input: &Value, arg: &Expr, left: bool) -> Result<Vec<Value>, EvalError> {
    let s = match input {
        Value::Str(s) => s,
        other => {
            return Err(EvalError::new(format!(
                "expected a string, got {}",
                other.type_name()
            )));
        }
    };
    let mut out = Vec::new();
    for prefix in eval(arg, input)? {
        let p = match &prefix {
            Value::Str(p) => p,
            other => {
                return Err(EvalError::new(format!(
                    "expected a string argument, got {}",
                    other.type_name()
                )));
            }
        };
        let trimmed = if left {
            s.strip_prefix(p).unwrap_or(s)
        } else {
            s.strip_suffix(p).unwrap_or(s)
        };
        out.push(Value::Str(trimmed.to_string()));
    }
    Ok(out)
}

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
        // A missing key (or OOB index) is a miss → empty stream. Indexing a
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
        // A type error on the left still propagates — a miss falls back, a
        // mistake doesn't hide.
        assert!(eval(&parse(r#".a.b // "d""#).unwrap(), &doc).is_err());
    }

    #[test]
    fn regex_test_match_capture() {
        let s = Value::Str("nginx:1.25".into());
        assert_eq!(one(r#"test("^nginx")"#, &s), Value::Bool(true));
        assert_eq!(one(r#"test("^NGINX")"#, &s), Value::Bool(false));
        // The `;`-separated flags argument, jq-style.
        assert_eq!(one(r#"test("^NGINX"; "i")"#, &s), Value::Bool(true));

        // match: no match → empty stream (a silent miss at the CLI); `g`
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
        // `1 = 2` — the left side is a literal, not a path.
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
}
