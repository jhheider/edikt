//! Driving comment mutation (`.foo.# = ...`, `|=`, `+=`, `del(.foo.#)`) through
//! the format-agnostic [`Document::set_comment`] / [`Document::delete_comment`]
//! write methods. The evaluator computes the new text in the value calculus
//! (so `.foo.# |= gsub("a"; "b")` works); the format splices it in place.

use crate::{CommentKind, Document, EditError, Expr, Step, Value, eval, eval_paths};

/// Apply a comment-mutation expression via the document's comment write
/// methods, returning any warnings (layout expansion, kind remap). Comment-free
/// mutations never reach here; the CLI routes on [`Expr::has_comment`].
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
        Expr::Assign(lhs, rhs) | Expr::UpdateAssign(lhs, rhs) | Expr::AddAssign(lhs, rhs) => {
            let (targets, kind) = comment_targets(doc, lhs)?;
            // `=` and `+=` evaluate their right side once, against the whole
            // document, as a value edit does.
            let whole = match expr {
                Expr::UpdateAssign(..) => None,
                _ => Some(eval_text(rhs, &doc.to_value())?),
            };
            for (di, prefix) in targets {
                // `|=` sees the current comment (or "" if absent) as `.`.
                let current = || current_comment(doc, di, &prefix, kind).unwrap_or_default();
                let text = match (expr, &whole) {
                    (Expr::AddAssign(..), Some(tail)) => current() + tail,
                    (_, Some(text)) => text.clone(),
                    _ => eval_text(rhs, &Value::Str(current()))?,
                };
                warnings.extend(match di {
                    Some(di) => doc.set_comment_in_doc(di, &prefix, kind, &text)?,
                    None => doc.set_comment(&prefix, kind, &text)?,
                });
            }
            Ok(())
        }
        Expr::Call(name, args) if name == "del" => {
            if args.len() != 1 {
                return Err(EditError::new("del(...) takes one argument"));
            }
            if is_comments_stream(&args[0]) {
                return bulk_delete(doc);
            }
            let (targets, kind) = comment_targets(doc, &args[0])?;
            if targets.iter().any(|(di, _)| di.is_some()) && doc.to_values().len() > 1 {
                // `delete_comment` has no per-document form, and a concrete
                // path from one document may name a comment in another.
                return Err(EditError::new(
                    "deleting a comment through a path expression isn't supported \
                     on a multi-document stream yet; use a plain path like `del(.a.#)`",
                ));
            }
            for (_, prefix) in targets {
                doc.delete_comment(&prefix, kind)?;
            }
            Ok(())
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

/// One node a comment edit targets: the document it was resolved in (`None`
/// for a plain path, which the format applies across a stream itself), and
/// its path.
type Target = (Option<usize>, Vec<Step>);

/// A comment edit's targets: the nodes whose `kind` comment it edits, each
/// with the document it was resolved in.
///
/// A plain path (`.a.b.#`) is one target, `None`: the format applies it as it
/// always has. A path expression ending in a comment step
/// (`(.xs[] | select(.id == "b") | .n.#)`, #109) resolves its value part
/// against each document to concrete paths, exactly as a value edit through
/// a path expression does (#88), and each becomes a target in its own
/// document. Resolving to nothing is a no-op.
fn comment_targets(
    doc: &dyn Document,
    lhs: &Expr,
) -> Result<(Vec<Target>, CommentKind), EditError> {
    if let Some(steps) = lhs.as_path() {
        let (prefix, kind) = split_comment(steps)?;
        return Ok((vec![(None, prefix.to_vec())], kind));
    }
    let (value_part, kind) = split_comment_target(lhs).ok_or_else(|| {
        EditError::new(
            "left side of a comment assignment must be a path ending in `.#` \
             (a plain path, or a path expression like `(.xs[] | select(...) | .n.#)`)",
        )
    })?;
    let mut targets = Vec::new();
    for (di, value) in doc.to_values().iter().enumerate() {
        for path in eval_paths(&value_part, value)? {
            targets.push((Some(di), path));
        }
    }
    Ok((targets, kind))
}

/// Split a comment-edit target that is a path expression into its value part
/// and the comment kind: `(f | .n.#)` and `(f).n.#` are `(f | .n, head)`.
/// `None` when the expression doesn't end in a comment step (or is a plain
/// path, which needs no splitting).
pub fn split_comment_target(lhs: &Expr) -> Option<(Expr, CommentKind)> {
    match lhs {
        Expr::Path(steps) => match steps.split_last() {
            Some((Step::Comment(kind), prefix)) => Some((Expr::Path(prefix.to_vec()), *kind)),
            _ => None,
        },
        Expr::Pipe(a, b) => {
            let (rest, kind) = split_comment_target(b)?;
            // `f | .#`: the comment of `f`'s own nodes.
            let value = if rest.as_path().is_some_and(<[Step]>::is_empty) {
                (**a).clone()
            } else {
                Expr::Pipe(a.clone(), Box::new(rest))
            };
            Some((value, kind))
        }
        _ => None,
    }
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
    let v = eval(rhs, input)?
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

/// How a format spells an own-line comment, for [`place_line_comment`] and
/// the extractors that read one back.
#[derive(Debug, Clone, Copy)]
pub struct LineComment {
    /// What opens a comment line after its indentation (`#`, `;`, `//`, ...).
    pub markers: &'static [&'static str],
    /// The prefix a new comment line is written with (`"# "`); its width is
    /// what wrapping budgets for.
    pub delim: &'static str,
}

impl LineComment {
    /// Is `line` a comment line (a marker after any indentation)?
    pub fn is_comment_line(&self, line: &str) -> bool {
        let t = line.trim_start();
        self.markers.iter().any(|m| t.starts_with(m))
    }

    /// A comment's text without its markers (`## x` -> `x`) and the space
    /// around it.
    pub fn strip_marker(&self, text: &str) -> String {
        let mut t = text;
        while let Some(rest) = self.markers.iter().find_map(|m| t.strip_prefix(m)) {
            t = rest;
        }
        t.trim().to_string()
    }

    /// The text of `line` if it is a comment line, markers stripped.
    pub fn comment_text(&self, line: &str) -> Option<String> {
        let t = line.trim();
        self.is_comment_line(t).then(|| self.strip_marker(t))
    }

    /// Write one own-line comment, `{indent}{delim}{text}\n`, with the text
    /// kept to one line.
    pub fn push_line(&self, out: &mut String, indent: &str, text: &str) {
        out.push_str(indent);
        out.push_str(self.delim);
        out.push_str(&sanitize_comment_line(text));
        out.push('\n');
    }

    /// Render comment lines as an own-line block at `indent`.
    pub fn block(&self, lines: &[String], indent: &str) -> String {
        let mut out = String::new();
        for l in lines {
            self.push_line(&mut out, indent, l);
        }
        out
    }
}

/// A comment's text as one line: a line break inside it would end the comment
/// and spill the rest into the document, so each becomes a space.
pub fn sanitize_comment_line(line: &str) -> String {
    line.replace(['\n', '\r'], " ")
}

/// Place (or clear) an own-line comment block around a node's line, on the
/// source text, for the formats whose comments are whole lines. `target_line`
/// is the 0-based index of the node's own line; `is_head` puts the block above
/// (else below, for foot). Contiguous existing comment lines on that side
/// (as `style` recognizes them) are replaced; `text == None` deletes. The
/// text wraps to the document's width envelope at `indent`. Untouched lines
/// are preserved verbatim, so the moat holds.
///
/// The block's lines end the way the target line does (the file's dominant
/// ending if the target is an unterminated last line). A foot block below an
/// unterminated last line first terminates that line, so the comment never
/// lands on the value's own line; the block's last line then stays
/// unterminated, keeping the file's missing final newline.
pub fn place_line_comment(
    source: &str,
    target_line: usize,
    is_head: bool,
    indent: &str,
    style: &LineComment,
    text: Option<&str>,
) -> String {
    let mut lines: Vec<String> = source.split_inclusive('\n').map(str::to_string).collect();
    if target_line >= lines.len() {
        return source.to_string();
    }
    let eol = match crate::text::ending_of(&lines[target_line]) {
        "" => crate::text::dominant(source),
        e => e,
    };
    let is_comment_line = |l: &str| style.is_comment_line(l.trim_end_matches(['\n', '\r']));
    let wrapped = text.map(|t| {
        let width = crate::wrap::wrap_width(source);
        crate::wrap::wrap_comment(t, width, indent.chars().count(), style.delim.len())
    });
    let delim = style.delim;
    let mut block: Vec<String> = wrapped
        .into_iter()
        .flatten()
        .map(|l| format!("{indent}{delim}{l}{eol}"))
        .collect();

    if is_head {
        let mut start = target_line;
        while start > 0 && is_comment_line(&lines[start - 1]) {
            start -= 1;
        }
        lines.splice(start..target_line, block);
    } else {
        let mut end = target_line + 1;
        while end < lines.len() && is_comment_line(&lines[end]) {
            end += 1;
        }
        // The replaced run reached an unterminated EOF: the file had no final
        // newline, so the new last line keeps not having one.
        if end == lines.len() && crate::text::ending_of(&lines[end - 1]).is_empty() {
            let target = &mut lines[target_line];
            match block.last_mut() {
                Some(last) => {
                    last.truncate(last.len() - eol.len());
                    if !target.ends_with('\n') {
                        target.push_str(eol);
                    }
                }
                None => {
                    let kept = target.trim_end_matches(['\n', '\r']).len();
                    target.truncate(kept);
                }
            }
        }
        lines.splice(target_line + 1..end, block);
    }
    lines.concat()
}

/// The 0-based line index containing byte offset `at`.
pub fn line_index(source: &str, at: usize) -> usize {
    source[..at.min(source.len())].matches('\n').count()
}

/// The current text of the comment at `prefix`/`kind`, if any: in document
/// `di` of a stream, or (`None`) the document a plain comment edit reads.
fn current_comment(
    doc: &dyn Document,
    di: Option<usize>,
    prefix: &[Step],
    kind: CommentKind,
) -> Option<String> {
    let mut path = prefix.to_vec();
    path.push(Step::Comment(kind));
    let commented = match di {
        Some(di) => doc.to_commented_all().into_iter().nth(di)?,
        None => doc.to_commented()?,
    };
    match commented.resolve_comment(&path).into_iter().next() {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: LineComment = LineComment {
        markers: &["#"],
        delim: "# ",
    };

    fn place(src: &str, line: usize, head: bool, text: Option<&str>) -> String {
        place_line_comment(src, line, head, "", &HASH, text)
    }

    #[test]
    fn line_comment_style() {
        let s = LineComment {
            markers: &["#", "!"],
            delim: "# ",
        };
        assert!(s.is_comment_line("  ! x"));
        assert!(!s.is_comment_line("a # x"));
        assert_eq!(s.strip_marker("#!# text "), "text");
        assert_eq!(sanitize_comment_line("a\r\nb"), "a  b");
        assert_eq!(s.comment_text("  ## hi  "), Some("hi".to_string()));
        assert_eq!(s.comment_text("a = 1"), None);
        let lines = ["one".to_string(), "two\nthree".to_string()];
        assert_eq!(s.block(&lines, "  "), "  # one\n  # two three\n");
    }

    #[test]
    fn block_takes_the_target_lines_ending() {
        assert_eq!(
            place("A=1\r\nB=2\r\n", 1, true, Some("x")),
            "A=1\r\n# x\r\nB=2\r\n"
        );
        assert_eq!(
            place("A=1\r\nB=2\r\n", 0, false, Some("x")),
            "A=1\r\n# x\r\nB=2\r\n"
        );
        assert_eq!(
            place("A=1\nB=2\r\n", 0, true, Some("x")),
            "# x\nA=1\nB=2\r\n"
        );
    }

    #[test]
    fn foot_below_an_unterminated_last_line_starts_its_own_line() {
        // Used to glue the comment onto the value: `A=1# x`.
        assert_eq!(place("A=1", 0, false, Some("x")), "A=1\n# x");
        assert_eq!(
            place("A=1\r\nB=2", 1, false, Some("x")),
            "A=1\r\nB=2\r\n# x"
        );
        // Replacing an unterminated trailing foot keeps the missing newline.
        assert_eq!(place("A=1\n# old", 0, false, Some("x")), "A=1\n# x");
        // Deleting it too.
        assert_eq!(place("A=1\n# old", 0, false, None), "A=1");
        // A terminated file stays terminated.
        assert_eq!(place("A=1\n", 0, false, Some("x")), "A=1\n# x\n");
    }

    #[test]
    fn a_comment_target_through_a_path_expression_splits_off_its_kind() {
        // #109: the value part resolves to concrete paths; the kind rides along.
        let split = |src: &str| {
            let (value, kind) = split_comment_target(&crate::parse(src).unwrap()).unwrap();
            (value, kind)
        };
        let p = |src: &str| crate::parse(src).unwrap();
        assert_eq!(
            split(r#".items[] | select(.id == "b") | .n.#"#),
            (
                p(r#".items[] | select(.id == "b") | .n"#),
                CommentKind::Head
            )
        );
        assert_eq!(
            split(r#"(.items[] | select(.id == "b")).n.#.inline"#),
            (
                p(r#".items[] | select(.id == "b") | .n"#),
                CommentKind::Inline
            )
        );
        // `f | .#` is the comment of `f`'s own nodes.
        assert_eq!(
            split(".items[] | .#.foot"),
            (p(".items[]"), CommentKind::Foot)
        );
        assert_eq!(split(".a.#"), (p(".a"), CommentKind::Head));
        // Not a comment target.
        assert!(split_comment_target(&p(".items[] | .n")).is_none());
        assert!(split_comment_target(&p(".a.# | ascii_upcase")).is_none());
    }
}
