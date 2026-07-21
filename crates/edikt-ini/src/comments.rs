//! INI ↔ the uniform comment model: extract `;`/`#` comments out of the CST
//! into [`Commented`] (head / inline / foot), matching the shape of
//! [`crate::project::to_value`] exactly, and emit a [`Commented`] back as INI
//! with comments placed (`;`-delimited).

use crate::project;
use crate::syntax::{Sk, SyntaxNode};
use edikt_core::wrap::{wrap_comment, wrap_width};
use edikt_core::{
    CommentKind, Commented, CommentedNode, Comments, EditError, Step, Value, flatten_commented,
    line_index, place_line_comment,
};
use rowan::NodeOrToken;

// --- in-place comment write-back ---------------------------------------

/// Is `line` an INI comment line (`;` or `#`, after indent)?
fn is_comment_line(line: &str) -> bool {
    matches!(line.trim_start().as_bytes().first(), Some(b';' | b'#'))
}

/// Set the `kind` comment on the node (entry or section header) at `path`.
pub(crate) fn set_target_comment(
    root: &SyntaxNode,
    path: &[Step],
    kind: CommentKind,
    text: &str,
) -> Result<(String, Vec<String>), EditError> {
    let target = resolve_target(root, path)?;
    let source = edikt_syntax::to_source(root);
    let start: usize = target.text_range().start().into();
    let indent: String = source[start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();

    let out = match kind {
        CommentKind::Head | CommentKind::Foot => {
            let width = wrap_width(&source);
            let wrapped = wrap_comment(text, width, indent.chars().count(), 2);
            place_line_comment(
                &source,
                line_index(&source, start),
                matches!(kind, CommentKind::Head),
                &indent,
                "; ",
                &is_comment_line,
                Some(&wrapped),
            )
        }
        CommentKind::Inline => rewrite_inline(&source, &target, Some(text)),
    };
    Ok((out, Vec::new()))
}

/// Delete the `kind` comment on the node at `path` (a miss is a no-op).
pub(crate) fn delete_target_comment(
    root: &SyntaxNode,
    path: &[Step],
    kind: CommentKind,
) -> Result<String, EditError> {
    let source = edikt_syntax::to_source(root);
    let Ok(target) = resolve_target(root, path) else {
        return Ok(source);
    };
    let start: usize = target.text_range().start().into();
    let indent: String = source[start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let out = match kind {
        CommentKind::Head | CommentKind::Foot => place_line_comment(
            &source,
            line_index(&source, start),
            matches!(kind, CommentKind::Head),
            &indent,
            "; ",
            &is_comment_line,
            None,
        ),
        CommentKind::Inline => rewrite_inline(&source, &target, None),
    };
    Ok(out)
}

/// Rewrite (or clear) an entry/header line's inline comment: keep everything up
/// to the value/`]`, then set `  ; text` (or nothing) before the line's newline.
fn rewrite_inline(source: &str, target: &SyntaxNode, set: Option<&str>) -> String {
    // Content end: after the entry's Value node, or after a header's `]`.
    let content_end: usize = target
        .children()
        .find(|n| n.kind() == Sk::Value)
        .map(|v| v.text_range().end())
        .or_else(|| {
            target
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == Sk::Close)
                .map(|t| t.text_range().end())
        })
        .map(usize::from)
        .unwrap_or_else(|| target.text_range().end().into());
    let nl = source[content_end..]
        .find('\n')
        .map(|i| content_end + i)
        .unwrap_or(source.len());
    let tail = match set {
        Some(text) => format!("  ; {}", sanitize(text)),
        None => String::new(),
    };
    format!("{}{}{}", &source[..content_end], tail, &source[nl..])
}

/// Resolve a comment target to its entry or header node.
fn resolve_target(root: &SyntaxNode, path: &[Step]) -> Result<SyntaxNode, EditError> {
    match path {
        [] => Err(EditError::new(
            "document-level (`.#`) comment editing for INI is a follow-up",
        )),
        [Step::Field(name)] => {
            // A `[name]` section's header, else a preamble entry.
            if let Some(header) = section_header(root, name) {
                Ok(header)
            } else {
                crate::edit::resolve_entry(root, path)
                    .ok_or_else(|| EditError::new(format!("no key or section `{name}`")))
            }
        }
        [Step::Field(_), Step::Field(_)] => {
            crate::edit::resolve_entry(root, path).ok_or_else(|| EditError::new("no such entry"))
        }
        _ => Err(EditError::new(
            "INI comment paths are `.key`, `.section`, or `.section.key`",
        )),
    }
}

/// The `Header` node of a named section, if it exists.
fn section_header(root: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    root.children()
        .filter(|n| n.kind() == Sk::Section)
        .find_map(|s| {
            s.children()
                .find(|c| c.kind() == Sk::Header)
                .filter(|h| project::section_name(h) == name)
        })
}

// --- extraction ---------------------------------------------------------

/// Project the whole document to the commented value model. Own-line comments
/// attach to the next entry (or the next section's header); comments trailing
/// the document attach as the last entry's foot.
pub(crate) fn to_commented(root: &SyntaxNode) -> Commented {
    let mut top: Vec<(String, Commented)> = Vec::new();
    let mut pending: Vec<String> = Vec::new();

    for section in root.children().filter(|n| n.kind() == Sk::Section) {
        match section.children().find(|n| n.kind() == Sk::Header) {
            Some(header) => {
                let mut entries = Vec::new();
                let comments = Comments {
                    head: std::mem::take(&mut pending),
                    inline: node_inline(&header),
                    foot: Vec::new(),
                };
                walk_body(&section, &mut entries, &mut pending);
                top.push((
                    project::section_name(&header),
                    Commented {
                        comments,
                        node: CommentedNode::Object(entries),
                    },
                ));
            }
            // The headerless preamble: its entries are top-level.
            None => walk_body(&section, &mut top, &mut pending),
        }
    }

    let mut root_c = Commented {
        comments: Comments::default(),
        node: CommentedNode::Object(top),
    };
    if !pending.is_empty() {
        root_c.attach_trailing_foot(pending);
    }
    root_c
}

/// Collect a section's entries, attaching pending own-line comments as each
/// entry's head and in-entry comments as its inline.
fn walk_body(section: &SyntaxNode, out: &mut Vec<(String, Commented)>, pending: &mut Vec<String>) {
    for elem in section.children_with_tokens() {
        match elem {
            NodeOrToken::Node(n) if n.kind() == Sk::Entry => {
                let comments = Comments {
                    head: std::mem::take(pending),
                    inline: node_inline(&n),
                    foot: Vec::new(),
                };
                out.push((
                    project::entry_key(&n),
                    Commented {
                        comments,
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
}

/// The inline comment token inside an `Entry` or `Header` node, if any.
fn node_inline(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == Sk::Comment)
        .map(|t| strip_marker(t.text()))
}

/// `; text` / `# text` -> `text`.
fn strip_marker(text: &str) -> String {
    text.trim_start_matches([';', '#']).trim().to_string()
}

// --- emission -----------------------------------------------------------

/// Emit a commented value as INI: top-level scalars become preamble entries,
/// top-level objects become `[section]`s (deeper nesting flattened to dotted
/// keys), arrays flatten to indexed dotted keys. Comments place natively -
/// head/foot as `; ` lines, inline after the value, so nothing warns beyond
/// the flatten warning.
pub fn emit_commented(c: &Commented) -> Result<(String, Vec<String>), EditError> {
    let CommentedNode::Object(entries) = &c.node else {
        return Err(EditError::new("INI output requires a top-level object"));
    };
    let mut out = String::new();
    let mut flattened = false;

    for l in &c.comments.head {
        push_comment(&mut out, l);
    }

    // Preamble: top-level scalars and arrays, in order.
    let preamble: Vec<(String, Commented)> = entries
        .iter()
        .filter(|(_, v)| !matches!(v.node, CommentedNode::Object(_)))
        .cloned()
        .collect();
    if !preamble.is_empty() {
        let flat = flatten_commented(&Commented {
            comments: Comments::default(),
            node: CommentedNode::Object(preamble),
        });
        flattened |= flat.iter().any(|e| e.key.contains('.'));
        for e in &flat {
            push_flat_entry(&mut out, e);
        }
    }

    // Sections: top-level objects, in order.
    for (name, v) in entries {
        let CommentedNode::Object(_) = &v.node else {
            continue;
        };
        if !out.is_empty() {
            out.push('\n');
        }
        for l in &v.comments.head {
            push_comment(&mut out, l);
        }
        out.push_str(&format!("[{name}]"));
        if let Some(inline) = &v.comments.inline {
            out.push_str(&format!("  ; {}", sanitize(inline)));
        }
        out.push('\n');
        // Flatten the section body only (its own comments are already placed
        // around the header, so strip them off the copy).
        let body = Commented {
            comments: Comments::default(),
            node: v.node.clone(),
        };
        let flat = flatten_commented(&body);
        flattened |= flat.iter().any(|e| e.key.contains('.'));
        for e in &flat {
            push_flat_entry(&mut out, e);
        }
        for l in &v.comments.foot {
            push_comment(&mut out, l);
        }
    }

    // A root inline comment has no line of its own here; it trails the document
    // with the root foot.
    for l in c.comments.inline.iter().chain(&c.comments.foot) {
        push_comment(&mut out, l);
    }

    let mut warnings = Vec::new();
    if flattened {
        warnings.push("nested/array values were flattened to dotted keys".to_string());
    }
    Ok((out, warnings))
}

fn push_flat_entry(out: &mut String, e: &edikt_core::FlatEntry) {
    for l in &e.comments.head {
        push_comment(out, l);
    }
    out.push_str(&format!("{} = {}", e.key, e.value));
    if let Some(inline) = &e.comments.inline {
        out.push_str(&format!("  ; {}", sanitize(inline)));
    }
    out.push('\n');
    for l in &e.comments.foot {
        push_comment(out, l);
    }
}

fn push_comment(out: &mut String, line: &str) {
    out.push_str("; ");
    out.push_str(&sanitize(line));
    out.push('\n');
}

/// A comment must stay on one line.
fn sanitize(line: &str) -> String {
    line.replace(['\n', '\r'], " ")
}
