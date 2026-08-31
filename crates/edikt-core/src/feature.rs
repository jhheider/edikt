//! Format capabilities.
//!
//! Each format module declares a static `&[Feature]`. The set is consulted at
//! edit time (an operation needing a feature the format lacks fails cleanly) and
//! at conversion time (each source-used feature the target lacks is a warning).

/// A capability a config format may or may not support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// Comments / trivia the CST preserves (JSONC, INI, `.env`; not plain JSON).
    Comments,
    /// Nested containers beyond a single level (JSON objects/arrays).
    Nesting,
    /// Ordered sequences.
    Arrays,
    /// Non-string scalars: numbers, booleans, null.
    TypedScalars,
    /// A single level of named grouping (INI `[section]`).
    Sections,
    /// Non-finite numbers: `Infinity`, `-Infinity`, `NaN`. JSON5, YAML and TOML
    /// spell these; plain JSON has no grammar for them, so a conversion into it
    /// degrades them to `null` and must say so.
    NonFinite,
}

impl Feature {
    /// Human-readable name, used in conversion warnings.
    pub fn as_str(self) -> &'static str {
        match self {
            Feature::Comments => "comments",
            Feature::Nesting => "nesting",
            Feature::Arrays => "arrays",
            Feature::TypedScalars => "typed scalars",
            Feature::Sections => "sections",
            Feature::NonFinite => "non-finite numbers (degraded to null)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(Feature::Comments.as_str(), "comments");
        assert_eq!(Feature::Nesting.as_str(), "nesting");
        assert_eq!(Feature::Arrays.as_str(), "arrays");
        assert_eq!(Feature::TypedScalars.as_str(), "typed scalars");
        assert_eq!(Feature::Sections.as_str(), "sections");
        assert_eq!(
            Feature::NonFinite.as_str(),
            "non-finite numbers (degraded to null)"
        );
    }
}
