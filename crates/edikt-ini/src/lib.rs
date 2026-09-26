//! edikt INI format module.
//!
//! A line-oriented lossless CST (hand-scanned into a `rowan` tree): `[section]`
//! headers, `key = value` / `key : value` entries, `;`/`#` comments, and blank
//! lines all round-trip byte-for-byte. Paths are `.section.key` (or `.key` for
//! the section-less preamble); values are strings. Edits touch only the targeted
//! value or line.

mod comments;
mod edit;
mod parser;
mod project;
mod syntax;

pub use comments::emit_commented;
pub use edit::apply;

// The edikt-core types that appear in this crate's own public API, re-exported
// so a dependent can call these methods without also taking a direct
// edikt-core dependency (jhheider/edikt#66). `parse` is aliased because this
// crate's own `parse` is the document parser.
pub use edikt_core::{
    CommentKind, Commented, Document, EditError, Expr, Feature, Step, Value, json,
    parse as parse_expr,
};
use syntax::{Sk, SyntaxNode};

/// Comment kinds this format supports (empty => none); the comment
/// capability, subsuming the boolean `Feature::Comments`.
pub const COMMENT_KINDS: &[CommentKind] =
    &[CommentKind::Head, CommentKind::Inline, CommentKind::Foot];

/// Capabilities of INI: comments and a single level of named sections.
pub const FEATURES: &[Feature] = &[Feature::Comments, Feature::Sections];

/// A parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg}")]
pub struct ParseError {
    pub msg: String,
}

/// A parsed INI document, backed by a lossless CST.
pub struct Ini {
    root: SyntaxNode,
    /// The source opened with a UTF-8 byte-order mark: kept out of the tree
    /// (it would read as part of the first key) and restored by `to_source`.
    bom: bool,
}

impl Ini {
    /// Access the underlying syntax tree.
    pub fn syntax(&self) -> &SyntaxNode {
        &self.root
    }

    /// Set the entry at `path` to a scalar, format-preserving. If the entry
    /// exists, only its value text changes. Otherwise a `key = value` line is
    /// inserted into the named section (creating the section if absent) or the
    /// preamble. A key, section name, or value INI can't read back as itself
    /// (a line break, surrounding whitespace, an inline-comment `;`/`#`, a
    /// key that would read as a header or comment) errors: INI has no
    /// quoting to fall back on.
    pub fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        let text = edikt_core::convert::scalar_string(value, "INI")?;
        edit::check_value(&text)?;
        if let Some(entry) = edit::resolve_entry(&self.root, path) {
            let value_node = entry
                .children()
                .find(|n| n.kind() == Sk::Value)
                .ok_or_else(|| EditError::new("entry has no value slot"))?;
            let new_root =
                value_node.replace_with(edikt_syntax::leaf_node(Sk::Value, Sk::ValStr, &text));
            self.root = SyntaxNode::new_root(new_root);
            return Ok(());
        }
        let (section, key) = match path {
            [Step::Field(k)] => (None, k.as_str()),
            [Step::Field(s), Step::Field(k)] => (Some(s.as_str()), k.as_str()),
            _ => return Err(EditError::new("INI paths are `.key` or `.section.key`")),
        };
        edit::check_key(key)?;
        if let Some(s) = section {
            edit::check_section(s)?;
        }
        let new_src = edit::insert_entry(&self.root, section, key, &text);
        self.root = SyntaxNode::new_root(parser::build(&new_src));
        Ok(())
    }

    /// The string value of the entry at `path`, or `None`.
    pub fn value_at(&self, path: &[Step]) -> Option<Value> {
        edit::resolve_entry(&self.root, path).map(|e| Value::Str(project::entry_value(&e)))
    }

    /// Delete the entry at `path`, removing its whole line (a missing entry is a
    /// no-op).
    pub fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        let root = self.root.clone_for_update();
        if let Some(entry) = edit::resolve_entry(&root, path) {
            entry.detach();
            self.root = SyntaxNode::new_root(root.green().into_owned());
        }
        Ok(())
    }
}

/// Parse INI source into an [`Ini`] document.
pub fn parse(src: &str) -> Result<Ini, ParseError> {
    let (bom, src) = edikt_core::text::split_bom(src);
    let root = SyntaxNode::new_root(parser::build(src));
    let malformed = edikt_syntax::tokens(&root).any(|t| t.kind() == Sk::Error);
    if malformed {
        return Err(ParseError {
            msg: "invalid INI: a line is neither a comment, section, nor key=value".to_string(),
        });
    }
    Ok(Ini { root, bom })
}

impl Document for Ini {
    fn to_source(&self) -> String {
        edikt_core::text::with_bom(self.bom, edikt_syntax::to_source(&self.root))
    }
    fn to_value(&self) -> Value {
        project::to_value(&self.root)
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<Vec<String>, EditError> {
        edit::apply(self, expr).map(|()| Vec::new())
    }
    fn has_comments(&self) -> bool {
        edikt_syntax::tokens(&self.root).any(|t| t.kind() == Sk::Comment)
    }
    fn to_commented(&self) -> Option<edikt_core::Commented> {
        Some(comments::to_commented(&self.root))
    }
    fn set_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        let (source, warnings) = comments::set_target_comment(&self.root, path, kind, text)?;
        self.root = SyntaxNode::new_root(parser::build(&source));
        Ok(warnings)
    }
    fn delete_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        let source = comments::delete_target_comment(&self.root, path, kind)?;
        self.root = SyntaxNode::new_root(parser::build(&source));
        Ok(())
    }
}

/// Emit a value as INI: top-level scalars become preamble entries, top-level
/// objects become `[section]`s (deeper nesting flattened to dotted keys), and
/// arrays flatten to indexed dotted keys. Returns the text and any warnings.
/// (The comment-free case of [`emit_commented`].)
pub fn emit(value: &Value) -> Result<(String, Vec<String>), EditError> {
    comments::emit_commented(&edikt_core::Commented::from_value(value))
}

#[cfg(test)]
mod tests;
