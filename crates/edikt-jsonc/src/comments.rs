//! JSONC ↔ the uniform comment model: extract `//` and `/* */` comments out of
//! the CST into [`Commented`] (head / inline / foot), and emit a [`Commented`]
//! back as pretty JSONC with `//` comments placed.
//!
//! Attachment rules (uniform across edikt's formats): own-line comments belong
//! to the *next* entry (`head`); a comment on an entry's own line is its
//! `inline`; comments after a container's last entry are that entry's `foot`
//! (or the container's, when it is empty).

use crate::project;
use crate::syntax::{Sk, SyntaxNode};
use edikt_core::wrap::{wrap_comment, wrap_width};
use edikt_core::{
    CommentKind, Commented, CommentedNode, Comments, EditError, Step, Value, line_index,
    place_line_comment,
};
use rowan::NodeOrToken;

// --- in-place comment write-back ---------------------------------------

/// Is `line` a JSONC comment line (`//` or `/*`, after indent)?
fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("/*")
}

/// Set the `kind` comment on the member/element at `path`, format-preserving.
/// Head/foot go on own lines around it; inline after its value. A compact
/// (single-line) container can't hold an own-line comment without expanding -
/// that reflow lands in a follow-up, so it errors here.
pub(crate) fn set_node_comment(
    root: &SyntaxNode,
    path: &[Step],
    kind: CommentKind,
    text: &str,
) -> Result<(String, Vec<String>), EditError> {
    let target = resolve_target(root, path)?;
    let source = edikt_syntax::to_source(root);
    let start: usize = target.text_range().start().into();
    let end: usize = target.text_range().end().into();
    let line_start = source[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent = &source[line_start..start];

    let out = match kind {
        CommentKind::Head | CommentKind::Foot => {
            if !indent.chars().all(|c| c == ' ' || c == '\t') {
                return Err(EditError::new(
                    "adding a comment to a compact object needs layout expansion (a follow-up)",
                ));
            }
            let width = wrap_width(&source);
            let wrapped = wrap_comment(text, width, indent.chars().count(), 3);
            place_line_comment(
                &source,
                line_index(&source, start),
                matches!(kind, CommentKind::Head),
                indent,
                "// ",
                &is_comment_line,
                Some(&wrapped),
            )
        }
        CommentKind::Inline => set_inline(&source, start, end, Some(text))?,
    };
    Ok((out, Vec::new()))
}

/// Delete the `kind` comment on the member/element at `path` (a miss is a no-op).
pub(crate) fn delete_node_comment(
    root: &SyntaxNode,
    path: &[Step],
    kind: CommentKind,
) -> Result<String, EditError> {
    let source = edikt_syntax::to_source(root);
    let Ok(target) = resolve_target(root, path) else {
        return Ok(source);
    };
    let start: usize = target.text_range().start().into();
    let end: usize = target.text_range().end().into();
    let line_start = source[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent = &source[line_start..start];
    let out = match kind {
        CommentKind::Head | CommentKind::Foot if indent.chars().all(|c| c.is_whitespace()) => {
            place_line_comment(
                &source,
                line_index(&source, start),
                matches!(kind, CommentKind::Head),
                indent,
                "// ",
                &is_comment_line,
                None,
            )
        }
        CommentKind::Head | CommentKind::Foot => source, // compact: nothing to drop
        CommentKind::Inline => set_inline(&source, start, end, None)?,
    };
    Ok(out)
}

/// Rewrite (or clear) the inline comment trailing a single-line member/element.
fn set_inline(
    source: &str,
    start: usize,
    end: usize,
    text: Option<&str>,
) -> Result<String, EditError> {
    if source[start..end].contains('\n') {
        return Err(EditError::new(
            "an inline comment on a multi-line value isn't supported yet (a follow-up)",
        ));
    }
    let nl = source[end..]
        .find('\n')
        .map(|i| end + i)
        .unwrap_or(source.len());
    // A container close (`}`/`]`) after the value on this line means it is
    // compact - an inline comment would comment it out; that reflow is later.
    if source[end..nl].contains(['}', ']', '{', '[']) {
        return Err(EditError::new(
            "adding an inline comment here needs layout expansion (a follow-up)",
        ));
    }
    let tail = match text {
        Some(t) => format!(" // {}", sanitize(t)),
        None => String::new(),
    };
    Ok(format!("{}{}{}", &source[..end], tail, &source[nl..]))
}

/// Resolve a comment target to its `Member` (object key) or element `Value` node.
fn resolve_target(root: &SyntaxNode, path: &[Step]) -> Result<SyntaxNode, EditError> {
    let Some((last, parent)) = path.split_last() else {
        return Err(EditError::new(
            "document-level (`.#`) comment editing for JSONC is a follow-up",
        ));
    };
    let container = crate::edit::resolve_value_node(root, parent)
        .ok_or_else(|| EditError::new("path not found"))?;
    match last {
        Step::Field(key) => {
            let object = container
                .children()
                .find(|n| n.kind() == Sk::Object)
                .ok_or_else(|| EditError::new("comment target is not an object member"))?;
            crate::edit::find_member(&object, key)
                .ok_or_else(|| EditError::new(format!("no key `{key}`")))
        }
        Step::Index(i) => {
            let array = container
                .children()
                .find(|n| n.kind() == Sk::Array)
                .ok_or_else(|| EditError::new("comment target is not an array element"))?;
            let values: Vec<_> = array.children().filter(|n| n.kind() == Sk::Value).collect();
            let idx = if *i < 0 { values.len() as i64 + i } else { *i };
            if idx < 0 || idx as usize >= values.len() {
                return Err(EditError::new("array index out of range"));
            }
            Ok(values[idx as usize].clone())
        }
        _ => Err(EditError::new(
            "comment paths address object keys or array elements",
        )),
    }
}

// --- extraction ---------------------------------------------------------

/// Project the whole document to the commented value model.
pub(crate) fn to_commented(root: &SyntaxNode) -> Commented {
    let mut result: Option<Commented> = None;
    let mut pending: Vec<String> = Vec::new();
    let mut same_line = false;
    for elem in root.children_with_tokens() {
        match elem {
            NodeOrToken::Node(n) if n.kind() == Sk::Value => {
                let mut c = value_commented(&n);
                let mut head = std::mem::take(&mut pending);
                head.append(&mut c.comments.head);
                c.comments.head = head;
                result = Some(c);
                same_line = true;
            }
            NodeOrToken::Token(t) => match t.kind() {
                Sk::Ws => {
                    if t.text().contains('\n') {
                        same_line = false;
                    }
                }
                Sk::LineComment | Sk::BlockComment => {
                    let lines = comment_lines(t.kind(), t.text());
                    match &mut result {
                        Some(c) if same_line => append_inline(&mut c.comments, &lines),
                        Some(c) => c.comments.foot.extend(lines),
                        None => pending.extend(lines),
                    }
                    if t.text().contains('\n') {
                        same_line = false;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    result.unwrap_or_else(|| Commented::scalar(Value::Null))
}

/// A `Value` node (scalar, object, or array) to a commented node. Comments
/// *around* the node belong to the caller; this handles comments *inside*.
fn value_commented(value: &SyntaxNode) -> Commented {
    if let Some(obj) = value.children().find(|n| n.kind() == Sk::Object) {
        return container_commented(&obj, true);
    }
    if let Some(arr) = value.children().find(|n| n.kind() == Sk::Array) {
        return container_commented(&arr, false);
    }
    Commented::scalar(project::value_node(value))
}

/// Walk an object's or array's children in lexical order, attaching each
/// comment token to the entry it belongs to.
fn container_commented(container: &SyntaxNode, is_object: bool) -> Commented {
    let mut entries: Vec<(String, Commented)> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut same_line = true; // relative to the opening brace's line
    let mut have_entry = false;

    for elem in container.children_with_tokens() {
        match elem {
            NodeOrToken::Node(n) => {
                let (key, mut c) = if is_object {
                    member_commented(&n)
                } else {
                    (String::new(), value_commented(&n))
                };
                let mut head = std::mem::take(&mut pending);
                head.append(&mut c.comments.head);
                c.comments.head = head;
                entries.push((key, c));
                have_entry = true;
                same_line = true;
            }
            NodeOrToken::Token(t) => match t.kind() {
                Sk::Ws => {
                    if t.text().contains('\n') {
                        same_line = false;
                    }
                }
                Sk::LineComment | Sk::BlockComment => {
                    let lines = comment_lines(t.kind(), t.text());
                    if same_line && have_entry {
                        let last = entries.len() - 1;
                        append_inline(&mut entries[last].1.comments, &lines);
                    } else {
                        pending.extend(lines);
                    }
                    if t.text().contains('\n') {
                        same_line = false;
                    }
                }
                _ => {}
            },
        }
    }

    // Comments after the last entry are its foot; in an empty container they
    // become the container's own foot.
    let mut comments = Comments::default();
    if !pending.is_empty() {
        match entries.last_mut() {
            Some((_, c)) => c.comments.foot.extend(pending),
            None => comments.foot = pending,
        }
    }
    let node = if is_object {
        CommentedNode::Object(entries)
    } else {
        CommentedNode::Array(entries.into_iter().map(|(_, c)| c).collect())
    };
    Commented { comments, node }
}

/// One object member: its key, and its value with any member-internal comments
/// attached (before the value -> head; after it, e.g. `"a": 1 /* x */,` -> inline).
fn member_commented(member: &SyntaxNode) -> (String, Commented) {
    let key = member
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == Sk::Str)
        .map(|t| project::unescape(t.text()))
        .unwrap_or_default();
    let mut c = member
        .children()
        .find(|n| n.kind() == Sk::Value)
        .map(|v| value_commented(&v))
        .unwrap_or_else(|| Commented::scalar(Value::Null));

    let mut seen_value = false;
    for elem in member.children_with_tokens() {
        match elem {
            NodeOrToken::Node(n) if n.kind() == Sk::Value => seen_value = true,
            NodeOrToken::Token(t) if matches!(t.kind(), Sk::LineComment | Sk::BlockComment) => {
                let lines = comment_lines(t.kind(), t.text());
                if seen_value {
                    append_inline(&mut c.comments, &lines);
                } else {
                    c.comments.head.extend(lines);
                }
            }
            _ => {}
        }
    }
    (key, c)
}

/// A comment token's text as delimiter-free trimmed lines. A multi-line block
/// comment yields one entry per line, with leading `*` decoration stripped.
fn comment_lines(kind: Sk, text: &str) -> Vec<String> {
    let inner = match kind {
        Sk::LineComment => text.strip_prefix("//").unwrap_or(text),
        _ => text
            .strip_prefix("/*")
            .and_then(|s| s.strip_suffix("*/"))
            .unwrap_or(text),
    };
    inner
        .lines()
        .map(|l| l.trim().strip_prefix('*').unwrap_or(l.trim()).trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Join comment lines onto a node's inline slot (an inline comment is a single
/// line by construction).
fn append_inline(comments: &mut Comments, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let mut text = comments.inline.take().unwrap_or_default();
    for l in lines {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(l);
    }
    comments.inline = Some(text);
}

// --- emission -----------------------------------------------------------

/// Emit a commented value as pretty (2-space) JSONC with a trailing newline,
/// placing head comments as `//` lines, inline comments after the value (and
/// its comma), and foot comments as `//` lines after. JSONC holds every
/// comment kind, so nothing warns.
pub fn emit_commented(c: &Commented) -> String {
    let mut out = String::new();
    for l in &c.comments.head {
        push_comment_line(&mut out, 0, l);
    }
    write_node(c, 0, &mut out);
    if let Some(inline) = &c.comments.inline {
        push_inline(&mut out, inline);
    }
    out.push('\n');
    for l in &c.comments.foot {
        push_comment_line(&mut out, 0, l);
    }
    out
}

fn write_node(c: &Commented, indent: usize, out: &mut String) {
    match &c.node {
        CommentedNode::Object(entries) if entries.is_empty() => out.push_str("{}"),
        CommentedNode::Array(items) if items.is_empty() => out.push_str("[]"),
        CommentedNode::Object(entries) => {
            out.push_str("{\n");
            for (i, (k, v)) in entries.iter().enumerate() {
                for l in &v.comments.head {
                    push_comment_line(out, indent + 1, l);
                }
                indent_by(indent + 1, out);
                out.push_str(&Value::Str(k.clone()).to_json());
                out.push_str(": ");
                write_node(v, indent + 1, out);
                if i + 1 < entries.len() {
                    out.push(',');
                }
                if let Some(inline) = &v.comments.inline {
                    push_inline(out, inline);
                }
                out.push('\n');
                for l in &v.comments.foot {
                    push_comment_line(out, indent + 1, l);
                }
            }
            indent_by(indent, out);
            out.push('}');
        }
        CommentedNode::Array(items) => {
            out.push_str("[\n");
            for (i, v) in items.iter().enumerate() {
                for l in &v.comments.head {
                    push_comment_line(out, indent + 1, l);
                }
                indent_by(indent + 1, out);
                write_node(v, indent + 1, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                if let Some(inline) = &v.comments.inline {
                    push_inline(out, inline);
                }
                out.push('\n');
                for l in &v.comments.foot {
                    push_comment_line(out, indent + 1, l);
                }
            }
            indent_by(indent, out);
            out.push(']');
        }
        CommentedNode::Scalar(v) => out.push_str(&v.to_json()),
    }
}

fn push_comment_line(out: &mut String, indent: usize, line: &str) {
    indent_by(indent, out);
    out.push_str("// ");
    out.push_str(&sanitize(line));
    out.push('\n');
}

fn push_inline(out: &mut String, text: &str) {
    out.push_str(" // ");
    out.push_str(&sanitize(text));
}

/// A comment line must not contain a newline (it would break out of the
/// comment); collapse any that sneak through.
fn sanitize(line: &str) -> String {
    line.replace(['\n', '\r'], " ")
}

fn indent_by(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str("  ");
    }
}
