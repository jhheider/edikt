//! Path expressions: resolving an expression to the **concrete paths** it
//! selects (jq's `path(f)`), and lowering an assignment whose left side is such
//! an expression into one plain-path assignment per match (#88).
//!
//! `(.items[] | select(.id == "b") | .n) = 5` resolves its left side against
//! the document to `.items[1].n`, then applies `.items[1].n = 5` through the
//! format's ordinary single-path splice. Every format gets assignment through
//! `select(...)` from this one lowering, and the edit stays exactly as surgical
//! as the hand-typed index would be.

use crate::ast::{Expr, Step};
use crate::eval::{EvalError, eval};
use crate::value::Value;
use std::cmp::Ordering;

/// A concrete path plus the value found there (`None` when the path names a
/// key or index that doesn't exist yet), borrowed from the input.
type Resolved<'v> = (Vec<Step>, Option<&'v Value>);

/// Resolve `expr`, a path expression, against `input` into the concrete,
/// iterate-free paths it selects, in document order.
///
/// A path expression is a plain path (`.a.b[0]`, `.a[]`), `select(pred)`, and
/// any `|` or `,` composition of them. The walk mirrors a query: a missing key
/// or index still resolves to its own path (so `=` can create it), `[]` over
/// something missing resolves to nothing, and stepping into the wrong type
/// errors as evaluation does. Anything else is not a path expression.
pub fn eval_paths(expr: &Expr, input: &Value) -> Result<Vec<Vec<Step>>, EvalError> {
    Ok(resolve(expr, Some(input))?
        .into_iter()
        .map(|(path, _)| path)
        .collect())
}

/// A concrete path as jq's path array: `.items[1].n` is `["items", 1, "n"]`.
pub(crate) fn path_to_value(steps: &[Step]) -> Value {
    Value::Array(
        steps
            .iter()
            .map(|s| match s {
                Step::Field(k) => Value::Str(k.clone()),
                Step::Index(i) => Value::Int(*i),
                // `resolve` never yields these in a concrete path.
                Step::Iterate | Step::Comment(_) => Value::Null,
            })
            .collect(),
    )
}

fn resolve<'v>(expr: &Expr, input: Option<&'v Value>) -> Result<Vec<Resolved<'v>>, EvalError> {
    match expr {
        Expr::Path(steps) => {
            let mut stream: Vec<Resolved> = vec![(Vec::new(), input)];
            for step in steps {
                let mut next = Vec::new();
                for (path, v) in stream {
                    step_paths(step, path, v, &mut next)?;
                }
                stream = next;
            }
            Ok(stream)
        }
        Expr::Pipe(a, b) => {
            let mut out = Vec::new();
            for (prefix, v) in resolve(a, input)? {
                for (rest, w) in resolve(b, v)? {
                    let mut path = prefix.clone();
                    path.extend(rest);
                    out.push((path, w));
                }
            }
            Ok(out)
        }
        Expr::Comma(items) => {
            let mut out = Vec::new();
            for it in items {
                out.extend(resolve(it, input)?);
            }
            Ok(out)
        }
        Expr::Call(name, args) if name == "select" && args.len() == 1 => {
            // The predicate sees `null` at a path that doesn't exist, as in jq.
            let keep = eval(&args[0], input.unwrap_or(&Value::Null))?
                .iter()
                .any(Value::is_truthy);
            Ok(if keep {
                vec![(Vec::new(), input)]
            } else {
                Vec::new()
            })
        }
        _ => Err(EvalError::new(
            "left side of an assignment must be a path (a path, `[]`, `select(...)`, \
             joined by `|` or `,`)",
        )),
    }
}

/// Apply one navigation step to a resolved path, pushing each continuation.
fn step_paths<'v>(
    step: &Step,
    path: Vec<Step>,
    v: Option<&'v Value>,
    out: &mut Vec<Resolved<'v>>,
) -> Result<(), EvalError> {
    let push = |out: &mut Vec<Resolved<'v>>, s: Step, child: Option<&'v Value>| {
        let mut p = path.clone();
        p.push(s);
        out.push((p, child));
    };
    match step {
        Step::Field(k) => match v {
            Some(Value::Object(m)) => {
                let child = m.iter().find(|(kk, _)| kk == k).map(|(_, x)| x);
                push(out, step.clone(), child);
            }
            Some(Value::Null) | None => push(out, step.clone(), None),
            Some(other) => {
                return Err(EvalError::new(format!(
                    "cannot index {} with \"{k}\"",
                    other.type_name()
                )));
            }
        },
        Step::Index(i) => match v {
            Some(Value::Array(a)) => {
                // Normalize a negative index against the array, so the path
                // names the element it resolved to (and `path(.a[-1])` is useful).
                match crate::resolve_index(*i, a.len()) {
                    Some(n) => push(out, Step::Index(n as i64), Some(&a[n])),
                    None => push(out, step.clone(), None),
                }
            }
            Some(Value::Null) | None => push(out, step.clone(), None),
            Some(other) => {
                return Err(EvalError::new(format!(
                    "cannot index {} with a number",
                    other.type_name()
                )));
            }
        },
        Step::Iterate => match v {
            Some(Value::Array(a)) => {
                for (n, x) in a.iter().enumerate() {
                    push(out, Step::Index(n as i64), Some(x));
                }
            }
            Some(Value::Object(m)) => {
                for (k, x) in m {
                    push(out, Step::Field(k.clone()), Some(x));
                }
            }
            // Nothing there: nothing to iterate, a miss like a query's.
            None => {}
            Some(other) => {
                return Err(EvalError::new(format!(
                    "cannot iterate over {}",
                    other.type_name()
                )));
            }
        },
        Step::Comment(_) => {
            return Err(EvalError::new(
                "a comment (`#`) can't be part of a path expression; \
                 a comment edit takes a plain path like `.foo.#`",
            ));
        }
    }
    Ok(())
}

/// The left side of a mutation when it is a path expression rather than a
/// plain path: `Some(lhs)` for `lhs = ..`, `lhs |= ..`, `lhs += ..`, `del(lhs)`.
/// A plain path (including `[]` fan-out) is `None`: formats handle it directly.
pub fn path_expr_target(expr: &Expr) -> Option<&Expr> {
    let lhs: &Expr = match expr {
        Expr::Assign(lhs, _) | Expr::UpdateAssign(lhs, _) | Expr::AddAssign(lhs, _) => lhs,
        Expr::Call(name, args) if name == "del" && args.len() == 1 => &args[0],
        _ => return None,
    };
    lhs.as_path().is_none().then_some(lhs)
}

/// Lower a mutation whose target is a path expression into one plain-path
/// mutation per concrete path it resolves to against `whole()` (the document
/// before this mutation). `None` means `expr` needs no lowering (not a
/// mutation, or its target is already a plain path); `Some(vec![])` means the
/// target matched nothing, a no-op.
///
/// `=` and `+=` evaluate their right side once against the whole document and
/// write the result to each path; `|=` keeps its right side, so each path's
/// update sees that path's own value. `del` targets come back deepest-last
/// first (reverse document order, duplicates dropped), so deleting one never
/// shifts the index of another.
pub fn lower_mutation(
    expr: &Expr,
    whole: impl FnOnce() -> Value,
) -> Result<Option<Vec<Expr>>, EvalError> {
    let Some(lhs) = path_expr_target(expr) else {
        return Ok(None);
    };
    let doc = whole();
    let mut paths = eval_paths(lhs, &doc)?;
    let at = |p: Vec<Step>| Box::new(Expr::Path(p));
    let lowered = match expr {
        Expr::Assign(_, rhs) | Expr::AddAssign(_, rhs) => {
            if paths.is_empty() {
                return Ok(Some(Vec::new()));
            }
            let value = eval(rhs, &doc)?
                .into_iter()
                .next()
                .ok_or_else(|| EvalError::new("right side of the assignment produced no value"))?;
            let lit = || Box::new(Expr::Literal(value.clone()));
            let assign = matches!(expr, Expr::Assign(..));
            paths
                .into_iter()
                .map(|p| {
                    if assign {
                        Expr::Assign(at(p), lit())
                    } else {
                        Expr::AddAssign(at(p), lit())
                    }
                })
                .collect()
        }
        Expr::UpdateAssign(_, rhs) => paths
            .into_iter()
            .map(|p| Expr::UpdateAssign(at(p), rhs.clone()))
            .collect(),
        _ => {
            // del: back to front, so earlier indices stay valid.
            paths.sort_by(|a, b| cmp_paths(b, a));
            paths.dedup();
            paths
                .into_iter()
                .map(|p| Expr::Call("del".into(), vec![Expr::Path(p)]))
                .collect()
        }
    };
    Ok(Some(lowered))
}

/// Document order over concrete paths: indices numerically, keys by name
/// (sibling keys never shift each other, so any consistent order will do).
fn cmp_paths(a: &[Step], b: &[Step]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let o = match (x, y) {
            (Step::Index(i), Step::Index(j)) => i.cmp(j),
            (Step::Field(k), Step::Field(l)) => k.cmp(l),
            (Step::Index(_), _) => Ordering::Less,
            (_, Step::Index(_)) => Ordering::Greater,
            _ => Ordering::Equal,
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn paths(expr: &str, doc: &Value) -> Vec<String> {
        eval_paths(&parse(expr).unwrap(), doc)
            .unwrap()
            .iter()
            .map(|p| crate::render_path(p))
            .collect()
    }

    fn items() -> Value {
        json!({
            "items": [
                {"id": "a", "n": 1, "tags": ["x"]},
                {"id": "b", "n": 2, "tags": ["x", "y"]},
                {"id": "c", "n": 3, "tags": []}
            ]
        })
    }

    #[test]
    fn select_resolves_to_matching_indices() {
        let doc = items();
        assert_eq!(
            paths(r#".items[] | select(.id == "b") | .n"#, &doc),
            vec![".items[1].n"]
        );
        // Multiple matches, in document order.
        assert_eq!(
            paths(".items[] | select(.n >= 2)", &doc),
            vec![".items[1]", ".items[2]"]
        );
        // Zero matches.
        assert!(paths(r#".items[] | select(.id == "zz")"#, &doc).is_empty());
        // Nested select: a select inside a select's sub-path.
        assert_eq!(
            paths(
                r#".items[] | select(.n > 1) | .tags[] | select(. == "y")"#,
                &doc
            ),
            vec![".items[1].tags[1]"]
        );
        // Comma composes streams.
        assert_eq!(
            paths(".items[0].n, .items[2].id", &doc),
            vec![".items[0].n", ".items[2].id"]
        );
    }

    #[test]
    fn misses_resolve_like_a_query() {
        let doc = items();
        // A missing key still has a path (so `=` can create it) ...
        assert_eq!(
            paths(r#".items[] | select(.id == "a") | .new"#, &doc),
            vec![".items[0].new"]
        );
        assert_eq!(paths(".nope.deeper", &doc), vec![".nope.deeper"]);
        // ... but iterating something missing is nothing.
        assert!(paths(".nope[] | select(.id == 1)", &doc).is_empty());
        // Negative indices normalize to the element they name.
        assert_eq!(paths(".items[-1]", &doc), vec![".items[2]"]);
        // Type errors propagate, as in a query.
        assert!(eval_paths(&parse(".items[0].id[]").unwrap(), &doc).is_err());
        assert!(eval_paths(&parse(".items.x").unwrap(), &doc).is_err());
        // Not a path expression.
        assert!(eval_paths(&parse(".items | length").unwrap(), &doc).is_err());
        assert!(eval_paths(&parse("1").unwrap(), &doc).is_err());
    }

    #[test]
    fn lowering_assign_update_add_and_del() {
        let doc = items();
        let low = |e: &str| lower_mutation(&parse(e).unwrap(), || doc.clone()).unwrap();
        // A plain path is not lowered.
        assert!(low(".items[1].n = 5").is_none());
        assert!(low(".items[].n = 5").is_none());
        assert!(low(".items").is_none());
        // `=`: RHS evaluated once, against the whole document.
        assert_eq!(
            low(r#"(.items[] | select(.id == "b") | .n) = .items[0].n"#),
            Some(vec![parse(".items[1].n = 1").unwrap()])
        );
        // `+=`: likewise, per match.
        assert_eq!(
            low("(.items[] | select(.n > 1) | .n) += 10"),
            Some(vec![
                parse(".items[1].n += 10").unwrap(),
                parse(".items[2].n += 10").unwrap(),
            ])
        );
        // `|=`: RHS kept, so each update sees its own value.
        assert_eq!(
            low(r#"(.items[] | select(.id == "c") | .n) |= . * 2"#),
            Some(vec![parse(".items[2].n |= . * 2").unwrap()])
        );
        // `del`: back to front.
        assert_eq!(
            low("del(.items[] | select(.n != 2))"),
            Some(vec![
                parse("del(.items[2])").unwrap(),
                parse("del(.items[0])").unwrap(),
            ])
        );
        // Zero matches lowers to nothing (and doesn't need the RHS).
        assert_eq!(
            low(r#"(.items[] | select(.id == "zz") | .n) = .missing"#),
            Some(vec![])
        );
    }

    #[test]
    fn value_level_assignment_through_select() {
        let doc = items();
        let one = |e: &str| {
            let mut r = eval(&parse(e).unwrap(), &doc).unwrap();
            assert_eq!(r.len(), 1, "{e}");
            r.remove(0)
        };
        let ns = |v: &Value| eval(&parse("[.items[].n]").unwrap(), v).unwrap();
        assert_eq!(
            ns(&one(r#"(.items[] | select(.id == "b") | .n) = 5"#)),
            vec![json!([1, 5, 3])]
        );
        assert_eq!(
            ns(&one("(.items[] | select(.n > 1) | .n) |= . * 10")),
            vec![json!([1, 20, 30])]
        );
        assert_eq!(
            ns(&one("(.items[] | select(.n < 3) | .n) += 1")),
            vec![json!([2, 3, 3])]
        );
        assert_eq!(
            ns(&one("del(.items[] | select(.n != 2))")),
            vec![json!([2])]
        );
        // Zero matches: the document is unchanged.
        assert_eq!(one(r#"(.items[] | select(.id == "zz") | .n) = 5"#), doc);
        // Still not a path.
        assert!(eval(&parse("(.items | length) = 1").unwrap(), &doc).is_err());
    }

    #[test]
    fn path_builtin_outputs_jq_path_arrays() {
        let doc = items();
        let r = eval(
            &parse(r#"path(.items[] | select(.id == "b"))"#).unwrap(),
            &doc,
        )
        .unwrap();
        assert_eq!(r, vec![json!(["items", 1])]);
        let r = eval(&parse("[path(.items[].id)] | length").unwrap(), &doc).unwrap();
        assert_eq!(r, vec![json!(3)]);
        assert_eq!(
            eval(&parse("path(.)").unwrap(), &doc).unwrap(),
            vec![json!([])]
        );
    }
}
