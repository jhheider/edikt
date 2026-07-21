//! KDL ↔ the uniform comment model, over `kdl-rs`'s per-node decor.
//!
//! A node's `leading` decor holds the whitespace and `//`/`/* */` comments
//! before it, its head; the segment of `before_terminator` after the node's
//! content holds a trailing `//` comment, its inline. The document's
//! `trailing` decor is the foot. Extraction and emission share the projection
//! convention (grouping, the `"-"` args key) with [`crate::project`].

use crate::project;
use edikt_core::wrap::{wrap_comment, wrap_width};
use edikt_core::{CommentKind, Commented, CommentedNode, Comments, EditError, Step, Value};
use kdl::{KdlDocument, KdlNode};

// --- in-place comment write-back ---------------------------------------

/// Set the `kind` comment on the node at `path` (a value prefix, no `#` step)
/// to `text`, format-preserving. Head wraps to the document width; inline never
/// wraps. Foot and document-level (`.#`) are follow-ups; comments attach to
/// nodes, not properties/arguments.
pub(crate) fn set_node_comment(
    doc: &mut KdlDocument,
    path: &[Step],
    kind: CommentKind,
    text: &str,
) -> Result<Vec<String>, EditError> {
    if matches!(kind, CommentKind::Foot) {
        return Err(EditError::new(
            "setting a foot comment isn't supported for KDL yet",
        ));
    }
    let width = wrap_width(&doc.to_string());
    let node = resolve_node(doc, path)?;
    let indent = leading_indent(node);
    match kind {
        CommentKind::Head => {
            let wrapped = wrap_comment(text, width, indent.chars().count(), 3);
            let existing = node.format().map(|f| f.leading.clone()).unwrap_or_default();
            let leading = rebuild_leading(&existing, &wrapped, &indent);
            ensure_format(node).leading = leading;
        }
        CommentKind::Inline => {
            let ending = node
                .format()
                .map(|f| line_ending(&f.terminator))
                .unwrap_or("\n");
            ensure_format(node).terminator = format!(" // {}{ending}", sanitize(text));
        }
        CommentKind::Foot => unreachable!(),
    }
    Ok(Vec::new())
}

/// Delete the `kind` comment on the node at `path` (a miss is a no-op).
pub(crate) fn delete_node_comment(
    doc: &mut KdlDocument,
    path: &[Step],
    kind: CommentKind,
) -> Result<(), EditError> {
    if matches!(kind, CommentKind::Foot) {
        return Err(EditError::new(
            "deleting a foot comment isn't supported for KDL yet",
        ));
    }
    let node = resolve_node(doc, path)?;
    let indent = leading_indent(node);
    match kind {
        CommentKind::Head => {
            let existing = node.format().map(|f| f.leading.clone()).unwrap_or_default();
            let leading = rebuild_leading(&existing, &[], &indent);
            ensure_format(node).leading = leading;
        }
        CommentKind::Inline => {
            let ending = node
                .format()
                .map(|f| line_ending(&f.terminator))
                .unwrap_or("\n")
                .to_string();
            ensure_format(node).terminator = ending;
        }
        CommentKind::Foot => unreachable!(),
    }
    Ok(())
}

/// Resolve a value path to the `&mut KdlNode` it names (`.foo`, `.foo.child`,
/// `.name[i]`, `.name[i].child`). Errors if the path lands on a property/arg
/// (comments attach to nodes) or an ambiguous repeated node.
fn resolve_node<'a>(doc: &'a mut KdlDocument, path: &[Step]) -> Result<&'a mut KdlNode, EditError> {
    let Some((Step::Field(name), rest)) = path.split_first() else {
        return Err(EditError::new(
            "document-level (`.#`) comment editing for KDL is a follow-up",
        ));
    };
    let occ: Vec<usize> = (0..doc.nodes().len())
        .filter(|&i| doc.nodes()[i].name().value() == name)
        .collect();
    if occ.is_empty() {
        return Err(EditError::new(format!("no node `{name}`")));
    }
    let (idx, rest) = match rest.split_first() {
        Some((Step::Index(i), r)) => {
            let n = occ.len() as i64;
            let resolved = if *i < 0 { n + i } else { *i };
            if resolved < 0 || resolved >= n {
                return Err(EditError::new(format!("`{name}` index out of range")));
            }
            (occ[resolved as usize], r)
        }
        _ => {
            if occ.len() != 1 {
                return Err(EditError::new(format!(
                    "`{name}` is repeated: address one occurrence (.{name}[i])"
                )));
            }
            (occ[0], rest)
        }
    };
    if rest.is_empty() {
        return Ok(&mut doc.nodes_mut()[idx]);
    }
    let children = doc.nodes_mut()[idx]
        .children_mut()
        .as_mut()
        .ok_or_else(|| {
            EditError::new(format!(
                "`{name}` has no child nodes (comments attach to nodes)"
            ))
        })?;
    resolve_node(children, rest)
}

fn ensure_format(node: &mut KdlNode) -> &mut kdl::KdlNodeFormat {
    if node.format().is_none() {
        node.set_format(kdl::KdlNodeFormat::default());
    }
    node.format_mut().unwrap()
}

/// Rebuild a `leading` decor with `wrapped` as its comment block: keep leading
/// blank lines and the trailing indent, drop old comments. Empty `wrapped`
/// clears the comment while preserving spacing and indent.
fn rebuild_leading(existing: &str, wrapped: &[String], indent: &str) -> String {
    let mut out = String::new();
    for line in existing.split_inclusive('\n') {
        if line.ends_with('\n') && line.trim().is_empty() {
            out.push_str(line);
        } else {
            break;
        }
    }
    for w in wrapped {
        out.push_str(&format!("{indent}// {w}\n"));
    }
    out.push_str(indent);
    out
}

/// The line ending of a node terminator (`\r\n`, else `\n`).
fn line_ending(terminator: &str) -> &'static str {
    if terminator.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

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
                // A repeated node -> array; each occurrence carries its own decor.
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
    let mut c = node_body_commented(node);
    if let Some(fmt) = node.format() {
        c.comments.head = own_line_comments(&fmt.leading);
        // A trailing `// ...` on the node's line rides in the terminator decor
        // (`"// pinned\n"`), before the newline.
        c.comments.inline = trailing_comment(&fmt.terminator);
    }
    c
}

/// A node's value projection, carrying its **child nodes'** comments. Mirrors
/// [`project::node_to_value`]: args and properties are comment-free scalars, so
/// they come straight from the value, but recurses through [`doc_commented`]
/// for a children block, so a comment on a nested node survives extraction (and
/// thus conversion) instead of being flattened away.
fn node_body_commented(node: &KdlNode) -> Commented {
    let Some(children) = node.children() else {
        return Commented::from_value(&project::node_to_value(node));
    };
    let Value::Object(pairs) = project::node_to_value(node) else {
        unreachable!("a node with children projects to an object");
    };
    let CommentedNode::Object(kids) = doc_commented(children).node else {
        unreachable!("a document projects to an object");
    };
    // node_to_value appends the children entries last, so the leading
    // `pairs.len() - kids.len()` entries are the arguments (`-`) and properties.
    let split = pairs.len() - kids.len();
    let mut entries: Vec<(String, Commented)> = pairs[..split]
        .iter()
        .map(|(k, v)| (k.clone(), Commented::from_value(v)))
        .collect();
    entries.extend(kids);
    Commented {
        comments: Comments::default(),
        node: CommentedNode::Object(entries),
    }
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

/// A trailing `// ...` comment on the node's own line (before its terminator).
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
