//! The format-agnostic document seam.

use crate::{Feature, Value};

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
}
