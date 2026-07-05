//! The query evaluator (value calculus) over an in-memory [`Value`].
//!
//! jq-style generator semantics: every expression maps one input value to a
//! *stream* of output values (0, 1, or many), collected here into a `Vec`.
//! Miss semantics are grep-shaped, not jq-shaped: a path that resolves to
//! nothing (missing key, out-of-range index) yields an **empty stream** (→ the
//! CLI's exit-1 "miss"), not `null`. An explicit `null` in the document still
//! yields `null`.
//!
//! Mutation (`=`, `|=`, `+=`, `del`) is not handled here yet — that arrives with
//! M2, alongside the CST write path.

use crate::ast::{BinOp, Expr, Step};
use crate::value::Value;
use std::cmp::Ordering;

/// An evaluation failure (type error, unknown function, arity mismatch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    pub msg: String,
}

impl EvalError {
    fn new(msg: impl Into<String>) -> EvalError {
        EvalError { msg: msg.into() }
    }
}
impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}
impl std::error::Error for EvalError {}

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
        Expr::Comma(items) => {
            let mut out = Vec::new();
            for it in items {
                out.extend(eval(it, input)?);
            }
            Ok(out)
        }
        Expr::Call(name, args) => eval_call(name, args, input),
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
    }
}

fn negate(v: &Value) -> Result<Value, EvalError> {
    match v {
        Value::Int(i) => Ok(Value::Int(-i)),
        Value::Float(f) => Ok(Value::Float(-f)),
        other => Err(EvalError::new(format!("cannot negate {}", other.type_name()))),
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

/// `+` is overloaded: numeric addition, string concat, and array concat.
fn add(a: &Value, b: &Value) -> Result<Value, EvalError> {
    match (a, b) {
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
            if let (Value::Int(xi), Value::Int(yi)) = (a, b)
                && *xi % *yi == 0
            {
                return Ok(Value::Int(*xi / *yi));
            }
            Ok(Value::Float(x / y))
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
        _ => Err(EvalError::new(format!("unknown function `{name}`"))),
    }
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
        other => Err(EvalError::new(format!(
            "{} has no keys",
            other.type_name()
        ))),
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
        Value::Object(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
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
            obj(&[("strict", Value::Bool(true)), ("target", Value::Str("ES2020".into()))]),
        )]);
        assert_eq!(one(".compilerOptions.strict", &doc), Value::Bool(true));
        assert_eq!(one(".compilerOptions.target", &doc), Value::Str("ES2020".into()));
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
                obj(&[("name", Value::Str("keep".into())), ("on", Value::Bool(true))]),
                obj(&[("name", Value::Str("drop".into())), ("on", Value::Bool(false))]),
            ]),
        )]);
        let r = run(".items[] | select(.on == true)", &doc);
        assert_eq!(r.len(), 1);
        assert_eq!(one(".name", &r[0]), Value::Str("keep".into()));
    }

    #[test]
    fn arithmetic_and_strings() {
        let doc = obj(&[("count", Value::Int(5)), ("name", Value::Str("edikt".into()))]);
        assert_eq!(one(".count + 1", &doc), Value::Int(6));
        assert_eq!(one(".count * 2 - 3", &doc), Value::Int(7));
        assert_eq!(one(".name + \"!\"", &doc), Value::Str("edikt!".into()));
        assert_eq!(one(".name | ascii_upcase", &doc), Value::Str("EDIKT".into()));
        assert_eq!(one(".name | length", &doc), Value::Int(5));
    }

    #[test]
    fn multi_output_comma() {
        let doc = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        assert_eq!(run(".a, .b", &doc), vec![Value::Int(1), Value::Int(2)]);
    }

    #[test]
    fn builtins() {
        let doc = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        assert_eq!(one("keys", &doc), Value::Array(vec![Value::Str("a".into()), Value::Str("b".into())]));
        assert_eq!(one("has(\"a\")", &doc), Value::Bool(true));
        assert_eq!(one("type", &doc), Value::Str("object".into()));
        assert_eq!(one("length", &doc), Value::Int(2));
        assert_eq!(one("\"12\" | tonumber", &Value::Null), Value::Int(12));
        assert_eq!(one("\"pre-x\" | ltrimstr(\"pre-\")", &Value::Null), Value::Str("x".into()));
    }

    #[test]
    fn type_errors() {
        assert!(eval(&parse(".a").unwrap(), &Value::Int(3)).is_err());
        assert!(eval(&parse(".[]").unwrap(), &Value::Int(3)).is_err());
        assert!(eval(&parse("length").unwrap(), &Value::Int(3)).is_err());
    }
}
