//! Driving comment mutation (`.foo.# = ...`, `|=`, `+=`, `del(.foo.#)`) through
//! the format-agnostic [`Document::set_comment`] / [`Document::delete_comment`]
//! write methods. The evaluator computes the new text in the value calculus
//! (so `.foo.# |= gsub("a"; "b")` works); the format splices it in place.

use crate::{CommentKind, Document, EditError, Expr, Step, Value, eval};

/// Apply a comment-mutation expression via the document's comment write
/// methods, returning any warnings (layout expansion, kind remap). Comment-free
/// mutations never reach here - the CLI routes on [`Expr::has_comment`].
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
        // Bulk: transform / clear every comment in the document.
        Expr::UpdateAssign(lhs, rhs) | Expr::AddAssign(lhs, rhs) if is_comments_stream(lhs) => {
            let append = matches!(expr, Expr::AddAssign(..));
            bulk_edit(doc, rhs, append, warnings)
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
                return Err(EditError::new("del(...) takes one argument"));
            }
            if is_comments_stream(&args[0]) {
                return bulk_delete(doc);
            }
            let steps = args[0]
                .as_path()
                .ok_or_else(|| EditError::new("del(...) takes a path"))?;
            let (prefix, kind) = split_comment(steps)?;
            doc.delete_comment(prefix, kind)
        }
        // Document selection with a comment edit: comment edits already apply
        // to every document, and per-document comment editing is a follow-up.
        // Name that, rather than falling through to the generic error below.
        Expr::DocSelect(..) => Err(EditError::new(
            "`^dN` document selection isn't supported with comment edits yet; \
             a comment edit already applies to every document of the stream",
        )),
        Expr::Call(name, _) if name == "select" => Err(EditError::new(
            "`select(...)` targeting isn't supported with comment edits yet; \
             a comment edit already applies to every document of the stream",
        )),
        _ => Err(EditError::new(
            "unsupported comment edit - use `.path.# = ...`, `|=`, `+=`, `del(.path.#)`, \
             or the bulk `comments |= ...` / `del(comments)`",
        )),
    }
}

/// Is `expr` the bare document-wide `comments` stream?
fn is_comments_stream(expr: &Expr) -> bool {
    matches!(expr, Expr::Call(name, args) if name == "comments" && args.is_empty())
}

/// `comments |= f` / `comments += x`: apply `f` (or append `x`) to every
/// comment's text, in document order. Targets are snapshotted first; their
/// paths stay valid as each write lands (logical, not byte-based).
fn bulk_edit(
    doc: &mut dyn Document,
    rhs: &Expr,
    append: bool,
    warnings: &mut Vec<String>,
) -> Result<(), EditError> {
    // Each new comment derives from that comment's own text, so a multi-document
    // stream needs per-document scoping: snapshot every document's comment
    // targets first (paths stay valid as writes land, logical not byte-based),
    // then transform each within its own document.
    let all = doc.to_commented_all();
    if all.is_empty() {
        return Err(EditError::new("this format has no comments"));
    }
    let mut targets = Vec::new();
    for (doc_idx, commented) in all.iter().enumerate() {
        for (steps, kind, current) in commented.comment_targets() {
            targets.push((doc_idx, steps, kind, current));
        }
    }
    for (doc_idx, steps, kind, current) in targets {
        let text = if append {
            let mut t = current;
            t.push_str(&eval_text(rhs, &doc.to_value())?);
            t
        } else {
            eval_text(rhs, &Value::Str(current))?
        };
        warnings.extend(doc.set_comment_in_doc(doc_idx, &steps, kind, &text)?);
    }
    Ok(())
}

/// `del(comments)`: remove every comment in every document. Deleting a comment
/// path that a given document lacks is a no-op there, so enumerating the union
/// of all documents' comment paths and deleting each clears the whole stream.
fn bulk_delete(doc: &mut dyn Document) -> Result<(), EditError> {
    let all = doc.to_commented_all();
    if all.is_empty() {
        return Err(EditError::new("this format has no comments"));
    }
    for commented in &all {
        for (steps, kind, _) in commented.comment_targets() {
            doc.delete_comment(&steps, kind)?;
        }
    }
    Ok(())
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

/// Place (or clear) an own-line comment block around a node's line, on the
/// source text - for the line-oriented formats (`.env`, INI). `target_line` is
/// the 0-based index of the node's own line; `is_head` puts the block above
/// (else below, for foot). Contiguous existing comment lines on that side
/// (detected by `is_comment_line`) are replaced; `wrapped == None` deletes.
/// Untouched lines are preserved verbatim, so the moat holds.
pub fn place_line_comment(
    source: &str,
    target_line: usize,
    is_head: bool,
    indent: &str,
    delim: &str,
    is_comment_line: &dyn Fn(&str) -> bool,
    wrapped: Option<&[String]>,
) -> String {
    let mut lines: Vec<String> = source.split_inclusive('\n').map(str::to_string).collect();
    if target_line >= lines.len() {
        return source.to_string();
    }
    let bare = |s: &str| s.trim_end_matches(['\n', '\r']).to_string();
    let block: Vec<String> = wrapped
        .into_iter()
        .flatten()
        .map(|l| format!("{indent}{delim}{l}\n"))
        .collect();

    if is_head {
        let mut start = target_line;
        while start > 0 && is_comment_line(&bare(&lines[start - 1])) {
            start -= 1;
        }
        lines.splice(start..target_line, block);
    } else {
        let mut end = target_line + 1;
        while end < lines.len() && is_comment_line(&bare(&lines[end])) {
            end += 1;
        }
        lines.splice(target_line + 1..end, block);
    }
    lines.concat()
}

/// The 0-based line index containing byte offset `at`.
pub fn line_index(source: &str, at: usize) -> usize {
    source[..at.min(source.len())].matches('\n').count()
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
