//! edikt TOML format module.
//!
//! Backed by `toml_edit`, whose whole purpose is format-preserving TOML edits -
//! so edikt gets lossless TOML (comments, spacing, table layout) essentially for
//! free, and the moat holds without a hand-rolled CST.

mod comments;
mod edit;
mod project;
mod slice;
mod spelling;
mod tree;

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
use toml_edit::{DocumentMut, Item, Table, Value as TomlValue};

/// Comment kinds this format supports (empty => none); the comment
/// capability, subsuming the boolean `Feature::Comments`.
pub const COMMENT_KINDS: &[CommentKind] =
    &[CommentKind::Head, CommentKind::Inline, CommentKind::Foot];

/// Capabilities of TOML.
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

/// A parsed TOML document, backed by a lossless `toml_edit` tree.
pub struct Toml {
    doc: DocumentMut,
    had_comments: bool,
    /// The parsed source (after any BOM). `toml_edit` writes every line
    /// ending as `\n` and always ends the file with one, so serializing
    /// restores the original's endings and missing final newline from it.
    original: String,
    /// The source opened with a UTF-8 byte-order mark (restored on output).
    bom: bool,
    /// The key spellings `toml_edit` can't keep on its own, found on first
    /// use (see the `spelling` module).
    respelled: std::sync::OnceLock<spelling::Respelled>,
}

impl Toml {
    /// The key spellings rendering would lose, from the original source.
    fn respelled(&self) -> &spelling::Respelled {
        self.respelled
            .get_or_init(|| spelling::respelled(&self.original))
    }

    /// The document as text, every key spelled as the source spelled it
    /// (jhheider/edikt#104). Errors when a spelling can't be kept.
    fn render(&self) -> Result<String, EditError> {
        let out = spelling::restore(self.doc.to_string(), self.doc.as_table(), self.respelled())?;
        Ok(self.finish(out))
    }

    /// `toml_edit`'s output with the source's line endings, missing final
    /// newline and BOM put back.
    fn finish(&self, mut out: String) -> String {
        // `toml_edit` terminates the last line unconditionally; a file that
        // didn't end with a newline keeps not ending with one.
        if !self.original.is_empty() && !self.original.ends_with('\n') && out.ends_with('\n') {
            out.pop();
        }
        // `toml_edit` also drops every `\r` it writes (decor and string reprs
        // alike), so put the original's line endings back, line by line.
        let out = edikt_core::text::restore_endings(&self.original, &out);
        edikt_core::text::with_bom(self.bom, out)
    }

    /// Run the edit `f`, refusing it (and leaving the document as it was)
    /// when its result can't keep every key spelling (jhheider/edikt#104). A
    /// file that spells each key one way has nothing to check.
    fn guarded<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, EditError>,
    ) -> Result<T, EditError> {
        if self.respelled().is_empty() {
            return f(self);
        }
        let before = self.doc.clone();
        let had_comments = self.had_comments;
        let result = f(self).and_then(|t| self.render().map(|_| t));
        if result.is_err() {
            self.doc = before;
            self.had_comments = had_comments;
        }
        result
    }
}

/// Parse TOML source into a [`Toml`] document.
pub fn parse(src: &str) -> Result<Toml, ParseError> {
    let (bom, src) = edikt_core::text::split_bom(src);
    let doc = src
        .parse::<DocumentMut>()
        .map_err(|e| ParseError { msg: e.to_string() })?;
    let had_comments = has_comment_decor(&doc);
    Ok(Toml {
        doc,
        had_comments,
        original: src.to_string(),
        bom,
        respelled: std::sync::OnceLock::new(),
    })
}

/// Whether any decor in the document holds a comment. `toml_edit` keeps
/// comments only in decor strings (key, value, and table prefix/suffix, an
/// array's or inline table's inner whitespace, the document trailer), which
/// hold nothing but whitespace and comments, so a `#` there is a comment,
/// while a `#` inside a string value (`tag = "v #1"`) is never looked at.
fn has_comment_decor(doc: &DocumentMut) -> bool {
    fn raw(s: Option<&str>) -> bool {
        s.is_some_and(|s| s.contains('#'))
    }
    fn decor(d: &toml_edit::Decor) -> bool {
        raw(d.prefix().and_then(|r| r.as_str())) || raw(d.suffix().and_then(|r| r.as_str()))
    }
    fn key(k: &toml_edit::Key) -> bool {
        decor(k.leaf_decor()) || decor(k.dotted_decor())
    }
    fn value(v: &TomlValue) -> bool {
        if decor(v.decor()) {
            return true;
        }
        match v {
            TomlValue::Array(a) => raw(a.trailing().as_str()) || a.iter().any(value),
            TomlValue::InlineTable(t) => {
                raw(t.preamble().as_str())
                    || t.iter()
                        .any(|(k, _)| t.get_key_value(k).is_some_and(|(k, v)| key(k) || item(v)))
            }
            _ => false,
        }
    }
    fn item(i: &Item) -> bool {
        match i {
            Item::None => false,
            Item::Value(v) => value(v),
            Item::Table(t) => table(t),
            Item::ArrayOfTables(a) => a.iter().any(table),
        }
    }
    fn table(t: &Table) -> bool {
        decor(t.decor())
            || t.iter()
                .any(|(k, _)| t.get_key_value(k).is_some_and(|(k, i)| key(k) || item(i)))
    }
    raw(doc.trailing().as_str()) || table(doc.as_table())
}

impl Document for Toml {
    fn to_source(&self) -> String {
        // An edit that can't keep the spellings was refused (`guarded`), so
        // the fallback is for the unreachable.
        self.render()
            .unwrap_or_else(|_| self.finish(self.doc.to_string()))
    }
    fn to_value(&self) -> Value {
        project::table_to_value(self.doc.as_table())
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<Vec<String>, EditError> {
        edit::apply(self, expr).map(|()| Vec::new())
    }
    fn has_comments(&self) -> bool {
        self.had_comments
    }
    fn to_commented(&self) -> Option<edikt_core::Commented> {
        Some(comments::to_commented(&self.doc))
    }
    /// A table comes back as the file's own text: its body, then its
    /// sub-table sections with their headers re-rooted (see the `slice`
    /// module). Only for the document as parsed: after an edit the source no
    /// longer describes the tree, so it falls back to emitting.
    fn source_slice(&self, path: &[Step]) -> Vec<String> {
        if self.to_source() != edikt_core::text::with_bom(self.bom, self.original.clone()) {
            return Vec::new();
        }
        slice::source_slices(&self.original, path)
    }
    fn set_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        self.guarded(|doc| {
            let warnings = comments::set_node_comment(&mut doc.doc, path, kind, text)?;
            doc.had_comments = true;
            Ok(warnings)
        })
    }
    fn delete_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        self.guarded(|doc| comments::delete_node_comment(&mut doc.doc, path, kind))
    }
}

#[cfg(test)]
mod tests;
