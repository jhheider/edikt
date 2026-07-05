//! Driving comment mutation (`.foo.# = …`, `|=`, `+=`, `del(.foo.#)`) through
//! the format-agnostic [`Document::set_comment`] / [`Document::delete_comment`]
//! write methods. The evaluator computes the new text in the value calculus
//! (so `.foo.# |= gsub("a"; "b")` works); the format splices it in place.

use crate::{CommentKind, Document, EditError, Expr, Step, Value, eval};

/// Apply a comment-mutation expression via the document's comment write
/// methods, returning any warnings (layout expansion, kind remap). Comment-free
/// mutations never reach here — the CLI routes on [`Expr::has_comment`].
pub fn apply_comment_mutation(
    doc: &mut dyn Document,
    expr: &Expr,
) -> Result<Vec<String>, EditError> {
    let mut warnings = Vec::new();
    apply_inner(doc, expr, &mut warnings)?;
    Ok(warnings)
}

fn apply_inner(
    doc: &mut dyn Document,
    expr: &Expr,
    warnings: &mut Vec<String>,
) -> Result<(), EditError> {
    match expr {
        Expr::Pipe(a, b) => {
            apply_inner(doc, a, warnings)?;
            apply_inner(doc, b, warnings)
        }
        Expr::Assign(lhs, rhs) => {
            let (prefix, kind) = comment_target(lhs)?;
            let text = eval_text(rhs, &doc.to_value())?;
            warnings.extend(doc.set_comment(prefix, kind, &text)?);
            Ok(())
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let (prefix, kind) = comment_target(lhs)?;
            // `|=` sees the current comment (or "" if absent) as `.`.
            let current = current_comment(doc, prefix, kind).unwrap_or_default();
            let text = eval_text(rhs, &Value::Str(current))?;
            warnings.extend(doc.set_comment(prefix, kind, &text)?);
            Ok(())
        }
        Expr::AddAssign(lhs, rhs) => {
            let (prefix, kind) = comment_target(lhs)?;
            let mut text = current_comment(doc, prefix, kind).unwrap_or_default();
            text.push_str(&eval_text(rhs, &doc.to_value())?);
            warnings.extend(doc.set_comment(prefix, kind, &text)?);
            Ok(())
        }
        Expr::Call(name, args) if name == "del" => {
            if args.len() != 1 {
                return Err(EditError::new("del(...) takes one path argument"));
            }
            let steps = args[0]
                .as_path()
                .ok_or_else(|| EditError::new("del(...) takes a path"))?;
            let (prefix, kind) = split_comment(steps)?;
            doc.delete_comment(prefix, kind)
        }
        _ => Err(EditError::new(
            "unsupported comment edit — use `.path.# = …`, `|=`, `+=`, or `del(.path.#)`",
        )),
    }
}

/// The (value-prefix, kind) of a comment-assignment left side.
fn comment_target(lhs: &Expr) -> Result<(&[Step], CommentKind), EditError> {
    let steps = lhs
        .as_path()
        .ok_or_else(|| EditError::new("left side of a comment assignment must be a path"))?;
    split_comment(steps)
}

/// Split a `#`-terminated path into its value prefix and the comment kind.
fn split_comment(steps: &[Step]) -> Result<(&[Step], CommentKind), EditError> {
    match steps.split_last() {
        Some((Step::Comment(kind), prefix)) => Ok((prefix, *kind)),
        _ => Err(EditError::new("expected a comment path ending in `#`")),
    }
}

/// Evaluate an RHS to comment text (a scalar rendered as its raw string).
fn eval_text(rhs: &Expr, input: &Value) -> Result<String, EditError> {
    let v = eval(rhs, input)
        .map_err(|e| EditError::new(e.to_string()))?
        .into_iter()
        .next()
        .ok_or_else(|| EditError::new("the comment text expression produced no value"))?;
    match v {
        Value::Array(_) | Value::Object(_) => {
            Err(EditError::new("a comment is text, not a container"))
        }
        scalar => Ok(scalar.to_raw_string()),
    }
}

/// The current text of the comment at `prefix`/`kind`, if any.
fn current_comment(doc: &dyn Document, prefix: &[Step], kind: CommentKind) -> Option<String> {
    let mut path = prefix.to_vec();
    path.push(Step::Comment(kind));
    let commented = doc.to_commented()?;
    match commented.resolve_comment(&path).into_iter().next() {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}
