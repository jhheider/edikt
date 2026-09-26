//! edikt `.env` / `.properties` format module.
//!
//! Flat, string-valued, honest line-level editing only: no grammar, no
//! interpolation, no quoting semantics, no inline comments. `key=value` /
//! `key:value` entries, `#`/`!` comments, and blanks round-trip byte-for-byte.
//! Paths are a single `.key`; edits change only the targeted value or line.

mod comments;
mod edit;
mod parser;
mod project;
mod syntax;

pub use comments::{emit_commented, emit_commented_with};
pub use edit::apply;
pub use parser::Dialect;

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
/// capability, subsuming the boolean `Feature::Comments`. No inline: a `#`
/// inside a value is data, not a comment.
pub const COMMENT_KINDS: &[CommentKind] = &[CommentKind::Head, CommentKind::Foot];

/// Capabilities: comments only. Flat and string-valued: no nesting, arrays,
/// typed scalars, or sections.
pub const FEATURES: &[Feature] = &[Feature::Comments];

/// A parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg}")]
pub struct ParseError {
    pub msg: String,
}

/// A parsed `.env` / `.properties` document, backed by a lossless CST.
pub struct Env {
    root: SyntaxNode,
    /// Remembered so an appended key is spelled the way the file spells its
    /// existing ones; a `key=value` line inside an envspaced file would not
    /// parse back as the same document.
    dialect: Dialect,
    /// The source opened with a UTF-8 byte-order mark: kept out of the tree
    /// (it would read as part of the first key) and restored by `to_source`.
    bom: bool,
}

impl Env {
    /// Access the underlying syntax tree.
    pub fn syntax(&self) -> &SyntaxNode {
        &self.root
    }

    /// Set the entry `key` to a scalar, format-preserving. If `key` doesn't
    /// exist, a new `key=value` line is appended. A key or value the line
    /// format can't read back as itself (a line break, surrounding
    /// whitespace, a key that would read as a comment or end early) errors:
    /// there is no quoting to fall back on.
    pub fn set(&mut self, key: &str, value: &Value) -> Result<(), EditError> {
        let text = edikt_core::convert::scalar_string(value, edit::format_name(self.dialect))?;
        edit::check_value(&text, self.dialect)?;
        match edit::find_entry(&self.root, key) {
            Some(entry) => {
                let value_node = entry
                    .children()
                    .find(|n| n.kind() == Sk::Value)
                    .ok_or_else(|| EditError::new("entry has no value slot"))?;
                let new_root =
                    value_node.replace_with(edikt_syntax::leaf_node(Sk::Value, Sk::ValStr, &text));
                self.root = SyntaxNode::new_root(new_root);
            }
            None => {
                edit::check_key(key, self.dialect)?;
                let mut src = edikt_syntax::to_source(&self.root);
                // The new line ends the way most of the file's lines do.
                let eol = edikt_core::text::dominant(&src);
                if !src.is_empty() && !src.ends_with('\n') {
                    src.push_str(eol);
                }
                // An appended key must be spelled the way the rest of the
                // file is, or the document stops parsing as itself.
                let sep = match self.dialect {
                    parser::Dialect::Punctuated => "=",
                    parser::Dialect::Spaced => " ",
                };
                src.push_str(&format!("{key}{sep}{text}{eol}"));
                self.root = SyntaxNode::new_root(parser::build(&src, self.dialect));
            }
        }
        Ok(())
    }

    /// The string value of `key`, or `None`.
    pub fn value_at(&self, key: &str) -> Option<Value> {
        edit::find_entry(&self.root, key).map(|e| Value::Str(project::entry_value(&e)))
    }

    /// Delete `key`, removing its whole line (a missing key is a no-op).
    pub fn delete(&mut self, key: &str) -> Result<(), EditError> {
        let root = self.root.clone_for_update();
        if let Some(entry) = edit::find_entry(&root, key) {
            entry.detach();
            self.root = SyntaxNode::new_root(root.green().into_owned());
        }
        Ok(())
    }
}

/// Parse `.env` / `.properties` source into an [`Env`] document.
pub fn parse(src: &str) -> Result<Env, ParseError> {
    parse_with(src, Dialect::Punctuated)
}

/// Parse space-separated `key value` source (`sshd_config`-shaped) into an
/// [`Env`] document.
///
/// Same flat, string-valued model as `.env`; only the separator differs. Not an
/// `ssh_config` parser: `Match` / `Host` blocks scope the keys beneath them and
/// this model is flat, so a file using them is out of scope rather than
/// half-supported.
pub fn parse_spaced(src: &str) -> Result<Env, ParseError> {
    parse_with(src, Dialect::Spaced)
}

/// Parse with an explicit [`Dialect`].
pub fn parse_with(src: &str, dialect: Dialect) -> Result<Env, ParseError> {
    let (bom, src) = edikt_core::text::split_bom(src);
    let root = SyntaxNode::new_root(parser::build(src, dialect));
    let malformed = edikt_syntax::tokens(&root).any(|t| t.kind() == Sk::Error);
    if malformed {
        return Err(ParseError {
            msg: "invalid: a line is neither a comment nor key=value".to_string(),
        });
    }
    Ok(Env { root, dialect, bom })
}

impl Document for Env {
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
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        let key = comments::single_key(path)?;
        let (source, warnings) = comments::set_key_comment(&self.root, key, kind, text)?;
        self.root = SyntaxNode::new_root(parser::build(&source, self.dialect));
        Ok(warnings)
    }
    fn delete_comment(
        &mut self,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        let key = comments::single_key(path)?;
        let source = comments::delete_key_comment(&self.root, key, kind)?;
        self.root = SyntaxNode::new_root(parser::build(&source, self.dialect));
        Ok(())
    }
}

/// Emit a value as a flat `.env`: every leaf becomes a `key=value` line, with
/// nested objects/arrays flattened to dotted keys. Returns the text and warnings.
/// (The comment-free case of [`emit_commented`].)
pub fn emit(value: &Value) -> Result<(String, Vec<String>), EditError> {
    comments::emit_commented(&edikt_core::Commented::from_value(value))
}

#[cfg(test)]
mod tests;
