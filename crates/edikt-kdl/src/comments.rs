//! KDL ↔ the uniform comment model, over `kdl-rs`'s per-node decor.
//!
//! A node's `leading` decor holds the whitespace and `//`/`/* */` comments
//! before it — its head; the segment of `before_terminator` after the node's
//! content holds a trailing `//` comment — its inline. The document's
//! `trailing` decor is the foot. Extraction and emission share the projection
//! convention (grouping, the `"-"` args key) with [`crate::project`].

use crate::project;
use edikt_core::{Commented, CommentedNode, Comments, EditError};
use kdl::{KdlDocument, KdlNode};

// --- extraction ------------------------------------------------------------

pub(crate) fn to_commented(doc: &KdlDocument) -> Commented {
    let mut root = doc_commented(doc);
    let foot = own_line_comments(doc.format().map(|f| f.trailing.as_str()).unwrap_or(""));
    if !foot.is_empty() {
        root.attach_trailing_foot(foot);
    }
    root
}

fn doc_commented(doc: &KdlDocument) -> Commented {
    let groups = project::group_nodes(doc);
    let entries = groups
        .into_iter()
        .map(|(name, nodes)| {
            let v = if nodes.len() == 1 {
                node_commented(nodes[0])
            } else {
                // A repeated node → array; each occurrence carries its own decor.
                Commented {
                    comments: Comments::default(),
                    node: CommentedNode::Array(nodes.iter().map(|n| node_commented(n)).collect()),
                }
            };
            (name, v)
        })
        .collect();
    Commented {
        comments: Comments::default(),
        node: CommentedNode::Object(entries),
    }
}

fn node_commented(node: &KdlNode) -> Commented {
    let mut c = Commented::from_value(&project::node_to_value(node));
    if let Some(fmt) = node.format() {
        c.comments.head = own_line_comments(&fmt.leading);
        // A trailing `// …` on the node's line rides in the terminator decor
        // (`"// pinned\n"`), before the newline.
        c.comments.inline = trailing_comment(&fmt.terminator);
    }
    c
}

/// The `//` and `/* */` comments in a decor string, delimiter-stripped, one
/// entry per line, in order.
fn own_line_comments(decor: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in decor.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("//") {
            out.push(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("/*") {
            out.push(rest.trim_end_matches("*/").trim().to_string());
        }
    }
    out
}

/// A trailing `// …` comment on the node's own line (before its terminator).
fn trailing_comment(decor: &str) -> Option<String> {
    let seg = decor.split('\n').next().unwrap_or("");
    seg.find("//").map(|i| seg[i + 2..].trim().to_string())
}

// --- emission ---------------------------------------------------------------

/// Emit a commented value as KDL, placing head comments as `//` lines above
/// each node and inline comments after the node. KDL holds every comment kind,
/// so nothing warns.
pub fn emit_commented(c: &Commented) -> Result<(String, Vec<String>), EditError> {
    let CommentedNode::Object(_) = &c.node else {
        return Err(EditError::new(
            "KDL output requires a top-level object (a document is a list of nodes)",
        ));
    };
    // Build the plain document first (identical bytes when comment-free), then
    // splice comments into node decor by walking the two in lockstep.
    let mut doc = crate::edit::build_document(&c.to_value())?;
    decorate_doc(&mut doc, c);
    let mut text = doc.to_string();
    for l in &c.comments.foot {
        text.push_str(&format!("// {}\n", sanitize(l)));
    }
    Ok((text, Vec::new()))
}

/// Attach a commented object's per-entry comments onto the matching nodes.
fn decorate_doc(doc: &mut KdlDocument, c: &Commented) {
    let CommentedNode::Object(entries) = &c.node else {
        return;
    };
    // The document's nodes are grouped by name in the same order projection
    // produced; walk entry-by-entry, consuming that name's node run.
    let mut cursor = 0;
    for (name, cv) in entries {
        let occ: Vec<usize> = (cursor..doc.nodes().len())
            .filter(|&i| doc.nodes()[i].name().value() == *name)
            .collect();
        match &cv.node {
            CommentedNode::Array(items) => {
                for (item, &idx) in items.iter().zip(&occ) {
                    decorate_node(&mut doc.nodes_mut()[idx], item);
                }
                if let Some(&last) = occ.last() {
                    cursor = last + 1;
                }
            }
            _ => {
                if let Some(&idx) = occ.first() {
                    decorate_node(&mut doc.nodes_mut()[idx], cv);
                    cursor = idx + 1;
                }
            }
        }
    }
}

fn decorate_node(node: &mut KdlNode, c: &Commented) {
    let indent = leading_indent(node);
    if !c.comments.head.is_empty() {
        let mut leading = String::new();
        for l in &c.comments.head {
            leading.push_str(&format!("{indent}// {}\n", sanitize(l)));
        }
        leading.push_str(&indent);
        if let Some(f) = node.format_mut() {
            f.leading = leading;
        }
    }
    if let Some(inline) = &c.comments.inline
        && let Some(f) = node.format_mut()
    {
        // The terminator carries a trailing comment before its newline; the
        // built document autoformats every node to a `\n` terminator.
        f.terminator = format!(" // {}\n", sanitize(inline));
    }
    // Recurse into a children block.
    if let CommentedNode::Object(_) = &c.node
        && let Some(children) = node.children_mut()
    {
        decorate_doc(children, c);
    }
}

/// The indentation the autoformatter gave a node (the run of spaces/tabs after
/// the last newline of its leading decor), so injected comment lines align.
fn leading_indent(node: &KdlNode) -> String {
    let leading = node.format().map(|f| f.leading.as_str()).unwrap_or("");
    let tail = leading.rsplit('\n').next().unwrap_or("");
    tail.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

fn sanitize(line: &str) -> String {
    line.replace(['\n', '\r'], " ")
}
