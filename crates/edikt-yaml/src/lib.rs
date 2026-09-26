//! edikt YAML format module: **lossless in-place edit, query, and conversion**,
//! pure Rust.
//!
//! YAML is driven by [`libyaml-safer`](https://crates.io/crates/libyaml-safer), a
//! safe pure-Rust port of the reference parser (zero transitive deps). One parse
//! pass feeds all three jobs: its event stream is composed into a **span tree**
//! (see the `compose` module) that carries both the data model (fold to [`Value`] for
//! query/convert) and every node's byte marks (the lossless splice for edit).
//!
//! The moat holds: an edit replaces exactly the targeted node's bytes; comments,
//! indentation, quote style, and layout of every untouched region survive
//! byte-for-byte. A new mapping or sequence is written in the file's own
//! layout (block under block, flow under flow, at its indent width), and only
//! the targeted value's bytes change: edikt never rewrites what it didn't
//! target.

mod block;
mod comments;
mod compose;
mod edit;
mod emit;
mod layout;
mod scalar;
mod splice;

use compose::{Node, node_to_value};
// The edikt-core types that appear in this crate's own public API, re-exported
// so a dependent can call these methods without also taking a direct
// edikt-core dependency (jhheider/edikt#66). `parse` is aliased because this
// crate's own `parse` is the document parser.
pub use edikt_core::{
    CommentKind, Commented, Document, EditError, Expr, Feature, Step, Value, json,
    parse as parse_expr,
};

pub use comments::emit_commented;
pub use emit::emit;

/// Comment kinds this format supports (empty => none); the comment
/// capability, subsuming the boolean `Feature::Comments`.
pub const COMMENT_KINDS: &[CommentKind] =
    &[CommentKind::Head, CommentKind::Inline, CommentKind::Foot];

/// Capabilities of YAML: everything but sections.
pub const FEATURES: &[Feature] = &[
    Feature::Comments,
    Feature::Nesting,
    Feature::Arrays,
    Feature::TypedScalars,
];

/// A parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg}")]
pub struct ParseError {
    pub msg: String,
}

/// A parsed YAML stream: the original source plus one span tree per document.
/// A single-document stream (the common case) has one entry in `docs`; a
/// `---`-separated stream has one per document.
pub struct Yaml {
    /// The source after any byte-order mark; every span indexes into it.
    pub(crate) source: String,
    pub(crate) docs: Vec<Node>,
    /// The source opened with a UTF-8 byte-order mark (restored on output).
    bom: bool,
}

impl Yaml {
    /// Re-read `source` after a comment write, keeping the BOM flag.
    fn reload(&mut self, source: &str) -> Result<(), EditError> {
        let bom = self.bom;
        *self = parse(source).map_err(|e| EditError::new(e.msg))?;
        self.bom = bom;
        Ok(())
    }
}

/// Parse YAML `src` into a [`Yaml`] stream.
pub fn parse(src: &str) -> Result<Yaml, ParseError> {
    // libyaml would otherwise mark spans past the BOM's one char as bytes.
    let (bom, src) = edikt_core::text::split_bom(src);
    let mut docs = compose::compose_all(src)
        .map_err(|msg| ParseError { msg })?
        .into_vec();
    // An empty or comment-only stream is one null document, so every caller has
    // at least one document to project or edit.
    if docs.is_empty() {
        docs.push(compose::null_node());
    }
    Ok(Yaml {
        source: src.to_string(),
        docs,
        bom,
    })
}

impl Document for Yaml {
    fn to_source(&self) -> String {
        edikt_core::text::with_bom(self.bom, self.source.clone())
    }
    fn to_value(&self) -> Value {
        match self.docs.first() {
            Some(d) => node_to_value(d),
            None => Value::Null,
        }
    }
    fn to_values(&self) -> Vec<Value> {
        if self.docs.is_empty() {
            vec![Value::Null]
        } else {
            self.docs.iter().map(node_to_value).collect()
        }
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<Vec<String>, EditError> {
        edit::apply(self, expr)
    }
    fn has_comments(&self) -> bool {
        has_comment(&self.source, &self.docs)
    }
    fn to_commented(&self) -> Option<edikt_core::Commented> {
        // The first document's comments (bulk enumeration and single-doc
        // callers). Multi-document comment queries use `to_commented_all`.
        let node = self.docs.first()?;
        Some(comments::to_commented(&self.source, node))
    }
    fn to_commented_all(&self) -> Vec<edikt_core::Commented> {
        self.docs
            .iter()
            .map(|node| comments::to_commented(&self.source, node))
            .collect()
    }
    fn set_comment(
        &mut self,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        // Set the comment at `path` in every document where the path resolves;
        // in a single-document stream a non-resolving path errors (strict), as
        // before. Each write recomposes, so the next document's marks stay
        // correct.
        let multi = self.docs.len() > 1;
        let mut warnings = Vec::new();
        for idx in 0..self.docs.len() {
            match comments::set_node_comment(&self.source, &self.docs[idx], path, kind, text) {
                Ok((source, warns)) => {
                    warnings.extend(warns);
                    self.reload(&source)?;
                }
                Err(_) if multi => {} // path absent in this document: skip
                Err(e) => return Err(e),
            }
        }
        Ok(warnings)
    }
    fn delete_comment(
        &mut self,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        // Delete in every document; a missing path is already a per-document
        // no-op in `delete_node_comment`.
        for idx in 0..self.docs.len() {
            let source = comments::delete_node_comment(&self.source, &self.docs[idx], path, kind)?;
            self.reload(&source)?;
        }
        Ok(())
    }
    fn set_comment_in_doc(
        &mut self,
        doc: usize,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        // Scope to one document (bulk transforms, where each comment's new text
        // comes from its own current text).
        let node = self
            .docs
            .get(doc)
            .ok_or_else(|| EditError::new("document index out of range"))?;
        let (source, warnings) = comments::set_node_comment(&self.source, node, path, kind, text)?;
        self.reload(&source)?;
        Ok(warnings)
    }
    fn source_slice(&self, path: &[edikt_core::Step]) -> Vec<String> {
        // One document's slices after another, in stream order, so the result
        // aligns 1:1 with a per-document query (`to_values`).
        self.docs
            .iter()
            .flat_map(|node| edit::source_slices(&self.source, node, path))
            .collect()
    }
}

/// Whether `source` holds a comment: a `#` at a line start or after
/// whitespace, outside every scalar's text. The span tree gives each scalar's
/// (and key's) exact bytes, so the `#` in `"v #1"` or `'a #b'` is data. A
/// block scalar's header line (`key: | # note`) can carry a comment, so only
/// the lines after its `|`/`>` indicator count as scalar text.
fn has_comment(source: &str, docs: &[Node]) -> bool {
    fn collect(node: &Node, source: &str, out: &mut Vec<std::ops::Range<usize>>) {
        match &node.kind {
            compose::NodeKind::Scalar(_) => {
                let text = source.get(node.span.clone()).unwrap_or("");
                // Past any anchor/tag properties, a block scalar opens with
                // its indicator; no other scalar can start with `|` or `>`.
                let is_block = text
                    .split_whitespace()
                    .find(|t| !t.starts_with(['&', '!']))
                    .is_some_and(|t| t.starts_with(['|', '>']));
                match text.find('\n').filter(|_| is_block) {
                    Some(nl) => out.push(node.span.start + nl..node.span.end),
                    None => out.push(node.span.clone()),
                }
            }
            compose::NodeKind::Sequence(items) => {
                items.iter().for_each(|n| collect(n, source, out));
            }
            compose::NodeKind::Mapping(entries) => {
                for e in entries {
                    out.push(e.key_span.clone());
                    collect(&e.value, source, out);
                }
            }
        }
    }
    let mut scalars = Vec::new();
    for doc in docs {
        collect(doc, source, &mut scalars);
    }
    scalars.sort_by_key(|r| r.start);
    let bytes = source.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        b == b'#' && (i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'\r')) && {
            // The last scalar starting at or before `i` is the only one
            // that can contain it (spans don't overlap).
            let k = scalars.partition_point(|r| r.start <= i);
            k == 0 || !scalars[k - 1].contains(&i)
        }
    })
}

#[cfg(test)]
mod tests;
