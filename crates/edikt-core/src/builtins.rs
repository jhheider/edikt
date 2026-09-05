//! The builtin function registry (the value calculus' jq-named functions).
//!
//! One `eval_call` dispatches over the whole registry - `length`/`keys`/`has`,
//! the string family (`split`/`join`, affix predicates, case mapping), the
//! regex family (`test`/`match`/`capture`/`sub`/`gsub`), `tonumber`/`tostring`,
//! and `select`. Grows deliberately, never speculatively: a new builtin lands
//! here as one `match` arm plus (if non-trivial) a helper in this file.
//!
//! `del` is handled at the value level right here too (it is a function in the
//! language): it defines what value `del(path)` produces.

use crate::ast::{Expr, Step};
use crate::eval::{EvalError, eval};
use crate::strings;
use crate::value::Value;
pub(crate) fn eval_call(name: &str, args: &[Expr], input: &Value) -> Result<Vec<Value>, EvalError> {
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
                "{name} takes {min}-{max} arguments, got {}",
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
pub(crate) fn comment_mutation_unsupported() -> &'static str {
    "editing comments (`#`) is not supported yet (planned for v0.2); reading works, e.g. `edikt '.foo.#' file`"
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
