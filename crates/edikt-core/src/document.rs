//! The format-agnostic document seam.

use crate::{CommentKind, Commented, EditError, Expr, Feature, Step, Value};

/// A parsed config document.
///
/// Each format module implements this over its own lossless CST. It is the
/// interface the CLI drives, uniform across JSONC/INI/env: serialize
/// losslessly, project to the [`Value`] model for querying/conversion, and
/// report the format's [`Feature`] set.
///
/// Mutation (`set`/`delete`/`append`) will extend this trait with M2; for now it
/// covers the read/query path.
pub trait Document {
    /// Byte-identical serialization for an unedited document (the round-trip
    /// invariant). Reflects in-place edits once mutation lands.
    fn to_source(&self) -> String;

    /// Project to the value model for querying and conversion. Trivia (comments,
    /// layout) is dropped — this is the data-model view, not the source view.
    fn to_value(&self) -> Value;

    /// The format's capabilities.
    fn features(&self) -> &'static [Feature];

    /// Apply a mutation expression (assignment / `del`) in place,
    /// format-preserving. Query expressions should be evaluated against
    /// [`Document::to_value`] instead; use [`Expr::is_mutation`] to choose.
    fn apply(&mut self, expr: &Expr) -> Result<(), EditError>;

    /// Whether the source contains any comments — used to warn on conversion,
    /// which drops them.
    fn has_comments(&self) -> bool;

    /// Project to the comment-annotated value model ([`Commented`]) so
    /// conversion can carry comments across formats. Shape and order must match
    /// [`Document::to_value`] exactly (same keys, same merge/resolution rules) —
    /// the CLI pairs the two projections by position. `None` means the format
    /// doesn't extract comments; conversion then falls back to the plain value
    /// path and warns that comments were dropped.
    fn to_commented(&self) -> Option<Commented> {
        None
    }

    /// The **original source text** of each node selected by `path`, in document
    /// order (aligned 1:1 with [`crate::eval`]'s results for the same path). This
    /// is the format-preserving "get": a structural query returns the exact bytes
    /// — comments, indentation, quoting — rather than a re-serialized value.
    ///
    /// The default returns empty, meaning "this format doesn't source-slice";
    /// the caller then falls back to emitting the value in the target format.
    /// Only formats with structural values (JSONC, YAML) need override it.
    fn source_slice(&self, path: &[Step]) -> Vec<String> {
        let _ = path;
        Vec::new()
    }

    /// Set the `kind` comment on the node at `path` to `text` (raw, unwrapped),
    /// format-preserving: only that comment's bytes change, or one comment line
    /// is inserted. Multi-line head/foot text is wrapped to the document's
    /// envelope by the implementation. Returns any warnings (a layout that had
    /// to expand to hold the comment, or a kind remapped to one the format
    /// supports). The default rejects — comment editing is added per format.
    fn set_comment(
        &mut self,
        path: &[Step],
        kind: CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        let _ = (path, kind, text);
        Err(EditError::new(
            "editing comments (`#`) isn't supported for this format yet",
        ))
    }

    /// Delete the `kind` comment on the node at `path` (a miss is a no-op).
    /// The default rejects — added per format alongside [`Document::set_comment`].
    fn delete_comment(&mut self, path: &[Step], kind: CommentKind) -> Result<(), EditError> {
        let _ = (path, kind);
        Err(EditError::new(
            "deleting comments (`#`) isn't supported for this format yet",
        ))
    }
}
