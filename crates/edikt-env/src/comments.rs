//! `.env` / `.properties` ↔ the uniform comment model. Extraction is line-flat:
//! own-line `#`/`!` comments attach to the next entry (head), trailing ones to
//! the last entry (foot); there are no inline comments in this format, ever
//! (the `#` in `K=a#b` is value bytes). Emission therefore *remaps* an inline
//! comment from another format onto its own line, with a warning.

use crate::project;
use crate::syntax::{Sk, SyntaxNode};
use edikt_core::wrap::{wrap_comment, wrap_width};
use edikt_core::{
    CommentKind, Commented, CommentedNode, Comments, EditError, Step, Value, flatten_commented,
    line_index, place_line_comment,
};
use rowan::NodeOrToken;

// --- in-place comment write-back ---------------------------------------

/// Is `line` a `.env`/`.properties` comment line (`#` or `!`, after indent)?
fn is_comment_line(line: &str) -> bool {
    matches!(line.trim_start().as_bytes().first(), Some(b'#' | b'!'))
}

/// Set the `kind` comment on the entry for `key` (`.env` has head/foot only -
/// inline is refused, a `#` in a value is data). Returns warnings (none here).
pub(crate) fn set_key_comment(
    root: &SyntaxNode,
    key: &str,
    kind: CommentKind,
    text: &str,
) -> Result<(String, Vec<String>), EditError> {
    if matches!(kind, CommentKind::Inline) {
        return Err(EditError::new(
            "`.env` has no inline comments (a `#` in a value is data); use `.#` for a head comment",
        ));
    }
    let source = edikt_syntax::to_source(root);
    let (line, indent) = entry_line(root, key, &source)?;
    let width = wrap_width(&source);
    let wrapped = wrap_comment(text, width, indent.chars().count(), 2);
    let out = place_line_comment(
        &source,
        line,
        matches!(kind, CommentKind::Head),
        &indent,
        "# ",
        &is_comment_line,
        Some(&wrapped),
    );
    Ok((out, Vec::new()))
}

/// Delete the `kind` comment on the entry for `key` (a miss is a no-op).
pub(crate) fn delete_key_comment(
    root: &SyntaxNode,
    key: &str,
    kind: CommentKind,
) -> Result<String, EditError> {
    if matches!(kind, CommentKind::Inline) {
        return Ok(edikt_syntax::to_source(root)); // no inline to delete
    }
    let source = edikt_syntax::to_source(root);
    let Ok((line, indent)) = entry_line(root, key, &source) else {
        return Ok(source); // missing key -> no-op
    };
    Ok(place_line_comment(
        &source,
        line,
        matches!(kind, CommentKind::Head),
        &indent,
        "# ",
        &is_comment_line,
        None,
    ))
}

/// The (line index, indent) of the entry for `key`.
fn entry_line(root: &SyntaxNode, key: &str, source: &str) -> Result<(usize, String), EditError> {
    let entry = crate::edit::find_entry(root, key)
        .ok_or_else(|| EditError::new(format!("no key `{key}`")))?;
    let start: usize = entry.text_range().start().into();
    let indent: String = source[start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    Ok((line_index(source, start), indent))
}

/// Extract the value path down to a single key (`.env` is flat).
pub(crate) fn single_key(path: &[Step]) -> Result<&str, EditError> {
    match path {
        [Step::Field(k)] => Ok(k),
        [] => Err(EditError::new(
            "document-level (`.#`) comment editing for `.env` is a follow-up",
        )),
        _ => Err(EditError::new(
            "`.env` is flat: comment paths are a single `.key`",
        )),
    }
}

// --- extraction ---------------------------------------------------------

/// Project the whole document to the commented value model.
pub(crate) fn to_commented(root: &SyntaxNode) -> Commented {
    let mut entries: Vec<(String, Commented)> = Vec::new();
    let mut pending: Vec<String> = Vec::new();

    for elem in root.children_with_tokens() {
        match elem {
            NodeOrToken::Node(n) if n.kind() == Sk::Entry => {
                entries.push((
                    project::entry_key(&n),
                    Commented {
                        comments: Comments {
                            head: std::mem::take(&mut pending),
                            inline: None,
                            foot: Vec::new(),
                        },
                        node: CommentedNode::Scalar(Value::Str(project::entry_value(&n))),
                    },
                ));
            }
            NodeOrToken::Token(t) if t.kind() == Sk::Comment => {
                pending.push(strip_marker(t.text()));
            }
            _ => {}
        }
    }

    let mut comments = Comments::default();
    if !pending.is_empty() {
        match entries.last_mut() {
            Some((_, e)) => e.comments.foot.extend(pending),
            None => comments.foot = pending,
        }
    }
    Commented {
        comments,
        node: CommentedNode::Object(entries),
    }
}

/// `# text` / `! text` -> `text`.
fn strip_marker(text: &str) -> String {
    text.trim_start_matches(['#', '!']).trim().to_string()
}

// --- emission -----------------------------------------------------------

/// Emit a commented value as a flat `.env`: every leaf becomes a `key=value`
/// line (nesting flattened to dotted keys), head/foot comments as `# ` lines.
/// An inline comment can't exist here, so it moves to a head line of its own -
/// with a warning, since that is a remap, not a placement.
pub fn emit_commented(c: &Commented) -> Result<(String, Vec<String>), EditError> {
    if !matches!(c.node, CommentedNode::Object(_)) {
        return Err(EditError::new("env output requires a top-level object"));
    }
    let flat = flatten_commented(c);
    let flattened = flat.iter().any(|e| e.key.contains('.'));
    let mut remapped_inline = false;
    let mut out = String::new();
    for e in &flat {
        for l in &e.comments.head {
            push_comment(&mut out, l);
        }
        if let Some(inline) = &e.comments.inline {
            remapped_inline = true;
            push_comment(&mut out, inline);
        }
        out.push_str(&format!("{}={}\n", e.key, e.value));
        for l in &e.comments.foot {
            push_comment(&mut out, l);
        }
    }

    let mut warnings = Vec::new();
    if flattened {
        warnings.push("nested/array values were flattened to dotted keys".to_string());
    }
    if remapped_inline {
        warnings.push("inline comments moved to their own line (env has none)".to_string());
    }
    Ok((out, warnings))
}

fn push_comment(out: &mut String, line: &str) {
    out.push_str("# ");
    out.push_str(&line.replace(['\n', '\r'], " "));
    out.push('\n');
}
