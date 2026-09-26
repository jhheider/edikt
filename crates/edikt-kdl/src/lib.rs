//! edikt KDL format module.
//!
//! Backed by [`kdl`](https://crates.io/crates/kdl) (kdl-rs), whose document is
//! format-preserving by construction (the `toml_edit` of KDL), so edikt gets
//! lossless KDL (comments, spacing, node layout) without a hand-rolled CST.
//!
//! KDL nodes carry positional **arguments**, `key=value` **properties**, and a
//! **children** block, none of which the flat `Value` model has a slot for
//! directly. The projection convention (documented in docs/design/contract.md and
//! implemented in the `project` module) maps them: nodes group by name (repeats ->
//! arrays), a node is its children object / lone scalar / argument array, and a
//! node mixing arguments with props/children puts the arguments under the
//! reserved key `"-"`.

mod comments;
mod edit;
mod project;
mod spell;

pub use comments::emit_commented;
pub use edit::{apply, emit};

// The edikt-core types that appear in this crate's own public API, re-exported
// so a dependent can call these methods without also taking a direct
// edikt-core dependency (jhheider/edikt#66). `parse` is aliased because this
// crate's own `parse` is the document parser.
pub use edikt_core::{
    CommentKind, Commented, Document, EditError, Expr, Feature, Step, Value, json,
    parse as parse_expr,
};
use kdl::KdlDocument;

/// Comment kinds this format supports (empty => none); the comment
/// capability, subsuming the boolean `Feature::Comments`.
pub const COMMENT_KINDS: &[CommentKind] =
    &[CommentKind::Head, CommentKind::Inline, CommentKind::Foot];

/// Capabilities of KDL: comments, nesting, arrays, and typed scalars.
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

/// A parsed KDL document, backed by kdl-rs's format-preserving tree.
pub struct Kdl {
    doc: KdlDocument,
}

impl Kdl {
    /// Set the value at `path`, format-preserving. Existing arguments and
    /// properties update in place; a missing leaf node is created; a run of
    /// repeated nodes extends when the assignment's array matches its prefix.
    /// Replacing a whole node body wholesale is refused (like YAML).
    pub fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        if path.is_empty() {
            return Err(EditError::new("cannot set the whole document"));
        }
        let before = self.doc.to_string();
        let unit = edit::indent_unit(&self.doc);
        edit::set_in_doc(&mut self.doc, path, value, 0, &unit)?;
        // kdl-rs autoformats a new node with `\n` line breaks; in a CRLF file,
        // respell the inserted text's breaks to match. The original bytes on
        // either side of the one insertion are left as they were.
        let eol = edikt_core::text::dominant(&before);
        if eol != "\n" {
            let after = self.doc.to_string();
            let fixed = edikt_core::text::eol_inserted(&before, &after, eol);
            if fixed != after {
                self.doc = KdlDocument::parse(&fixed)
                    .map_err(|e| EditError::new(format!("internal: re-reading an edit: {e}")))?;
            }
        }
        Ok(())
    }

    /// The value at `path`, or `None`.
    pub fn value_at(&self, path: &[Step]) -> Option<Value> {
        edikt_core::eval(&Expr::Path(path.to_vec()), &self.to_value())
            .ok()?
            .into_iter()
            .next()
    }

    /// Delete the node / property / argument at `path` (a miss is a no-op).
    pub fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        if path.is_empty() {
            return Err(EditError::new("del(.) is not allowed"));
        }
        edit::delete_in_doc(&mut self.doc, path, false)
    }
}

/// Whether any decor in `doc` holds a comment. kdl-rs keeps comments (line,
/// block, and `/-` slashdash) only in decor strings, which otherwise hold
/// whitespace, so a comment opener there is a comment, while `//` inside a
/// string value (`url "https://x"`) is never looked at.
fn comments_in_decor(doc: &KdlDocument) -> bool {
    fn text(s: &str) -> bool {
        s.contains("//") || s.contains("/*") || s.contains("/-")
    }
    fn node(n: &kdl::KdlNode) -> bool {
        n.format().is_some_and(|f| {
            [
                &f.leading,
                &f.before_ty_name,
                &f.after_ty_name,
                &f.after_ty,
                &f.before_children,
                &f.before_terminator,
                // A line comment ending the node rides in its terminator.
                &f.terminator,
                &f.trailing,
            ]
            .into_iter()
            .any(|s| text(s))
        }) || n.entries().iter().any(|e| {
            e.format().is_some_and(|f| {
                [
                    &f.leading,
                    &f.trailing,
                    &f.after_ty,
                    &f.before_ty_name,
                    &f.after_ty_name,
                    &f.after_key,
                    &f.after_eq,
                ]
                .into_iter()
                .any(|s| text(s))
            })
        }) || n.children().is_some_and(comments_in_decor)
    }
    doc.format()
        .is_some_and(|f| text(&f.leading) || text(&f.trailing))
        || doc.nodes().iter().any(node)
}

/// Parse KDL source into a [`Kdl`] document.
pub fn parse(src: &str) -> Result<Kdl, ParseError> {
    let doc = KdlDocument::parse(src).map_err(|e| ParseError { msg: e.to_string() })?;
    Ok(Kdl { doc })
}

impl Document for Kdl {
    fn to_source(&self) -> String {
        self.doc.to_string()
    }
    fn to_value(&self) -> Value {
        project::doc_to_value(&self.doc)
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<Vec<String>, EditError> {
        edit::apply(self, expr).map(|()| Vec::new())
    }
    fn has_comments(&self) -> bool {
        comments_in_decor(&self.doc)
    }
    fn to_commented(&self) -> Option<edikt_core::Commented> {
        Some(comments::to_commented(&self.doc))
    }
    fn set_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        comments::set_node_comment(&mut self.doc, path, kind, text)
    }
    fn delete_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        comments::delete_node_comment(&mut self.doc, path, kind)
    }
}

#[cfg(test)]
mod tests;
