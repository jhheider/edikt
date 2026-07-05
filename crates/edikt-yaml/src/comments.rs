//! YAML ↔ the uniform comment model, over the span tree.
//!
//! libyaml's event stream carries no comments, but the span tree carries every
//! node's byte range — so **extraction** scans the source *between* spans:
//! full-comment lines above a node are its head, a `# …` after a node's end
//! (or after the `:` of an entry whose value opens on a later line) is its
//! inline, and comment lines after the last node are the document's foot.
//!
//! **Emission** goes the other way with the same machinery: emit the plain
//! value through the existing libyaml emitter (byte-identical to today when
//! nothing is commented), re-parse *our own output* into a span tree, and
//! splice comment bytes in at the positions the spans dictate.

use crate::compose::{self, Entry, Node, NodeKind};
use crate::emit::emit;
use edikt_core::{Commented, CommentedNode, Comments, EditError, Value};

// --- extraction ---------------------------------------------------------

/// Project the document to the commented value model. Shape (including merge-key
/// resolution) mirrors [`compose::node_to_value`] exactly.
pub(crate) fn to_commented(src: &str, doc: &Node) -> Commented {
    // An empty document (nothing, or only comments) has no node to scan
    // around: everything commented is the null scalar's head.
    if doc.span.start == doc.span.end {
        let mut c = Commented::scalar(Value::Null);
        c.comments.head = comment_lines_between(src, 0, src.len());
        return c;
    }
    let mut c = node_commented(src, doc, 0);
    // A scalar root has no entries to claim the banner; container roots hand
    // it to their first entry (floor 0).
    if matches!(doc.kind, NodeKind::Scalar(_)) {
        c.comments.head = comment_lines_between(src, 0, line_start(src, doc.span.start));
        c.comments.inline = comment_after(src, doc.span.end);
    }
    // Comments after the document's last content line.
    let after = (line_end(src, content_end(doc)) + 1).min(src.len());
    c.comments.foot = comment_lines_between(src, after, src.len());
    c
}

/// Where a node's *content* ends: the deepest last scalar (or key). Collection
/// end marks are useless here — libyaml places them past any trailing
/// comments, at the token where it detected the dedent.
fn content_end(node: &Node) -> usize {
    match &node.kind {
        NodeKind::Scalar(_) => node.span.end,
        NodeKind::Sequence(items) => items.last().map(content_end).unwrap_or(node.span.start),
        NodeKind::Mapping(entries) => entries
            .last()
            .map(|e| content_end(&e.value).max(e.key_span.end))
            .unwrap_or(node.span.start),
    }
}

/// Compose a node's commented form. `floor` is where the previous sibling (or
/// the owning entry's key) ended — the scan region for the first head comment.
fn node_commented(src: &str, node: &Node, floor: usize) -> Commented {
    match &node.kind {
        NodeKind::Scalar(v) => Commented::scalar(v.clone()),
        NodeKind::Sequence(items) => {
            let mut out = Vec::new();
            let mut floor = floor;
            for item in items {
                let mut c = node_commented(src, item, item.span.start);
                c.comments.head =
                    comment_lines_between(src, floor, line_start(src, item.span.start));
                c.comments.inline = comment_after(src, content_end(item));
                out.push(c);
                floor = content_end(item);
            }
            Commented {
                comments: Comments::default(),
                node: CommentedNode::Array(out),
            }
        }
        NodeKind::Mapping(entries) => {
            let mut out: Vec<(String, Commented)> = Vec::new();
            let mut merged: Vec<(String, Value)> = Vec::new();
            let mut floor = floor;
            for e in entries {
                // A merge key (`<<`) is resolved away in the value projection;
                // mirror that here (its comments go with it — they describe the
                // merge line, which won't exist in the converted output).
                if e.key == "<<" {
                    compose::collect_merge(&compose::node_to_value(&e.value), &mut merged);
                } else {
                    let mut c = node_commented(src, &e.value, e.key_span.end);
                    c.comments.head =
                        comment_lines_between(src, floor, line_start(src, e.key_span.start));
                    c.comments.inline = entry_inline(src, e);
                    out.push((e.key.clone(), c));
                }
                floor = content_end(&e.value).max(e.key_span.end);
            }
            for (k, v) in merged {
                if !out.iter().any(|(ek, _)| ek == &k) {
                    out.push((k, Commented::from_value(&v)));
                }
            }
            Commented {
                comments: Comments::default(),
                node: CommentedNode::Object(out),
            }
        }
    }
}

/// An entry's inline comment: after the value when it sits on the key's line,
/// otherwise on the key line after the `:` (skipping anchors, tags, and block
/// scalar indicators — `web: &a # c`, `text: | # c`).
fn entry_inline(src: &str, e: &Entry) -> Option<String> {
    let same_line = !src[e.key_span.end..e.value.span.start.max(e.key_span.end)].contains('\n');
    if same_line {
        return comment_after(src, e.value.span.end);
    }
    let seg = &src[e.key_span.end..line_end(src, e.key_span.end)];
    let mut rest = seg.trim_start();
    rest = rest.strip_prefix(':').unwrap_or(rest);
    loop {
        rest = rest.trim_start();
        if let Some(text) = rest.strip_prefix('#') {
            return Some(text.trim().to_string());
        }
        // Skip one anchor/tag/block-indicator token, if that's what's next.
        let token: &str = rest.split_whitespace().next()?;
        let skippable = token.starts_with(['&', '!'])
            || (token.starts_with(['|', '>'])
                && token[1..]
                    .chars()
                    .all(|c| matches!(c, '+' | '-' | '0'..='9')));
        if !skippable {
            return None;
        }
        rest = &rest[token.len()..];
    }
}

/// A `# …` trailing `pos` on the same line (allowing one `,` first, so flow
/// items like `[a, # c` still surface theirs).
fn comment_after(src: &str, pos: usize) -> Option<String> {
    let seg = &src[pos..line_end(src, pos)];
    let rest = seg.trim_start().trim_start_matches(',').trim_start();
    rest.strip_prefix('#').map(|t| t.trim().to_string())
}

/// Full comment lines between two byte positions (whole lines only — partial
/// lines at either boundary belong to the nodes on them).
fn comment_lines_between(src: &str, from: usize, to: usize) -> Vec<String> {
    if from >= to {
        return Vec::new();
    }
    // Start at `from` if it begins a line, else at the next line.
    let start = if from == line_start(src, from) {
        from
    } else {
        (line_end(src, from) + 1).min(to)
    };
    src[start..to]
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            t.starts_with('#')
                .then(|| t.trim_start_matches('#').trim().to_string())
        })
        .collect()
}

// --- emission -----------------------------------------------------------

/// Emit a commented value as YAML: the plain emitter's output (identical when
/// nothing is commented), with comments spliced in at span positions. YAML
/// holds every comment kind, so nothing warns.
pub fn emit_commented(c: &Commented) -> Result<(String, Vec<String>), EditError> {
    let (text, warnings) = emit(&c.to_value())?;
    if !c.has_comments() {
        return Ok((text, warnings));
    }
    // Our own emitter's output always re-parses; the span tree of that output
    // is structurally identical to `c` (both mirror the same value).
    let doc = compose::compose_source(&text).map_err(EditError::new)?;

    let mut inserts: Vec<(usize, String)> = Vec::new();
    if !c.comments.head.is_empty() {
        inserts.push((0, comment_block(&c.comments.head, "")));
    }
    plan(&text, c, &doc, &mut inserts);
    if let Some(inline) = &c.comments.inline {
        inserts.push((line_end(&text, content_end(&doc)), inline_text(inline)));
    }
    if !c.comments.foot.is_empty() {
        inserts.push((text.len(), comment_block(&c.comments.foot, "")));
    }

    // Apply back-to-front so positions stay valid; the stable sort keeps
    // planning order for equal positions (a foot before the next head).
    inserts.sort_by_key(|(pos, _)| *pos);
    let mut out = text;
    for (pos, s) in inserts.into_iter().rev() {
        out.insert_str(pos, &s);
    }
    Ok((out, warnings))
}

/// Walk the commented tree and the emitted text's span tree in parallel,
/// planning one insert per comment block.
fn plan(text: &str, c: &Commented, n: &Node, inserts: &mut Vec<(usize, String)>) {
    match (&c.node, &n.kind) {
        (CommentedNode::Object(centries), NodeKind::Mapping(nentries)) => {
            for ((_, cv), ne) in centries.iter().zip(nentries) {
                let key_line = line_start(text, ne.key_span.start);
                let indent = indent_of(text, key_line);
                if !cv.comments.head.is_empty() {
                    inserts.push((key_line, comment_block(&cv.comments.head, indent)));
                }
                if let Some(inline) = &cv.comments.inline {
                    let same_line = !text
                        [ne.key_span.end..ne.value.span.start.max(ne.key_span.end)]
                        .contains('\n');
                    let anchor = if same_line {
                        ne.value.span.end
                    } else {
                        ne.key_span.end
                    };
                    inserts.push((line_end(text, anchor), inline_text(inline)));
                }
                if !cv.comments.foot.is_empty() {
                    let last = content_end(&ne.value).max(ne.key_span.end);
                    let after = (line_end(text, last) + 1).min(text.len());
                    inserts.push((after, comment_block(&cv.comments.foot, indent)));
                }
                plan(text, cv, &ne.value, inserts);
            }
        }
        (CommentedNode::Array(citems), NodeKind::Sequence(nitems)) => {
            for (cv, ni) in citems.iter().zip(nitems) {
                let item_line = line_start(text, ni.span.start);
                let indent = indent_of(text, item_line);
                let mut head = cv.comments.head.clone();
                // A container item has no single line to trail; its inline
                // joins the head above it.
                let container = !matches!(ni.kind, NodeKind::Scalar(_));
                if let Some(inline) = &cv.comments.inline {
                    if container {
                        head.push(inline.clone());
                    } else {
                        inserts.push((line_end(text, ni.span.end), inline_text(inline)));
                    }
                }
                if !head.is_empty() {
                    inserts.push((item_line, comment_block(&head, indent)));
                }
                if !cv.comments.foot.is_empty() {
                    let after = (line_end(text, content_end(ni)) + 1).min(text.len());
                    inserts.push((after, comment_block(&cv.comments.foot, indent)));
                }
                plan(text, cv, ni, inserts);
            }
        }
        _ => {}
    }
}

// --- shared line arithmetic ----------------------------------------------

fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

fn line_end(text: &str, pos: usize) -> usize {
    text[pos..]
        .find('\n')
        .map(|i| pos + i)
        .unwrap_or(text.len())
}

fn indent_of(text: &str, line_start: usize) -> &str {
    let line = &text[line_start..line_end(text, line_start)];
    &line[..line.len() - line.trim_start().len()]
}

fn comment_block(lines: &[String], indent: &str) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(indent);
        out.push_str("# ");
        out.push_str(&sanitize(l));
        out.push('\n');
    }
    out
}

fn inline_text(text: &str) -> String {
    format!(" # {}", sanitize(text))
}

fn sanitize(line: &str) -> String {
    line.replace(['\n', '\r'], " ")
}
