//! edikt TOML format module.
//!
//! Backed by `toml_edit`, whose whole purpose is format-preserving TOML edits —
//! so edikt gets lossless TOML (comments, spacing, table layout) essentially for
//! free, and the moat holds without a hand-rolled CST.

mod edit;
mod project;

pub use edikt_core::EditError;
pub use edit::{apply, emit};

use edikt_core::{Document, Expr, Feature, Step, Value, eval};
use toml_edit::{DocumentMut, TableLike};

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
}

impl Toml {
    /// Set the value at `path`, format-preserving. Creates a key in an existing
    /// table; intermediate-table creation and array-index paths are follow-ups.
    pub fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        let Some((last, parent)) = path.split_last() else {
            return Err(EditError::new("cannot set the whole document"));
        };
        let Step::Field(key) = last else {
            return Err(EditError::new("TOML set targets object keys"));
        };
        let mut current: &mut dyn TableLike = self.doc.as_table_mut();
        for step in parent {
            let Step::Field(k) = step else {
                return Err(EditError::new("TOML paths for set are object keys"));
            };
            let item = current
                .get_mut(k)
                .ok_or_else(|| EditError::new(format!("no key `{k}`")))?;
            current = item
                .as_table_like_mut()
                .ok_or_else(|| EditError::new(format!("`{k}` is not a table")))?;
        }
        let new_item = edit::value_to_item(value)?;
        if let Some(existing) = current.get_mut(key) {
            // Preserve the existing value's decor (spacing + inline comment).
            let decor = existing.as_value().map(|v| v.decor().clone());
            *existing = new_item;
            if let (Some(decor), Some(v)) = (decor, existing.as_value_mut()) {
                *v.decor_mut() = decor;
            }
        } else {
            current.insert(key, new_item);
        }
        Ok(())
    }

    /// The value at `path`, or `None`.
    pub fn value_at(&self, path: &[Step]) -> Option<Value> {
        eval(&Expr::Path(path.to_vec()), &self.to_value())
            .ok()?
            .into_iter()
            .next()
    }

    /// Delete the key at `path` (a missing key is a no-op).
    pub fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        let Some((last, parent)) = path.split_last() else {
            return Ok(());
        };
        let Step::Field(key) = last else {
            return Err(EditError::new("TOML del targets object keys"));
        };
        let mut current: &mut dyn TableLike = self.doc.as_table_mut();
        for step in parent {
            let Step::Field(k) = step else {
                return Ok(());
            };
            match current.get_mut(k).and_then(|i| i.as_table_like_mut()) {
                Some(next) => current = next,
                None => return Ok(()),
            }
        }
        current.remove(key);
        Ok(())
    }
}

/// Parse TOML source into a [`Toml`] document.
pub fn parse(src: &str) -> Result<Toml, ParseError> {
    let doc = src
        .parse::<DocumentMut>()
        .map_err(|e| ParseError { msg: e.to_string() })?;
    let had_comments = src
        .lines()
        .any(|l| l.trim_start().starts_with('#') || l.contains(" #"));
    Ok(Toml { doc, had_comments })
}

impl Document for Toml {
    fn to_source(&self) -> String {
        self.doc.to_string()
    }
    fn to_value(&self) -> Value {
        project::table_to_value(self.doc.as_table())
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<(), EditError> {
        edit::apply(self, expr)
    }
    fn has_comments(&self) -> bool {
        self.had_comments
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edikt_core::parse as parse_expr;

    const SAMPLE: &str = "# package\n[package]\nname = \"edikt\"\nversion = \"0.1.0\"   # semver\n\n[dependencies]\nrowan = \"0.16\"\n";

    fn q(src: &str, expr: &str) -> Vec<Value> {
        eval(&parse_expr(expr).unwrap(), &parse(src).unwrap().to_value()).unwrap()
    }

    fn edit_src(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        apply(&mut doc, &parse_expr(expr).unwrap()).unwrap();
        doc.to_source()
    }

    #[test]
    fn roundtrips_byte_identically() {
        for src in [SAMPLE, "", "a = 1\n", "[t]\nx = true\n[t.nested]\ny = 2\n"] {
            assert_eq!(parse(src).unwrap().to_source(), src, "round-trip: {src:?}");
        }
    }

    #[test]
    fn projects_tables_and_scalars() {
        assert_eq!(q(SAMPLE, ".package.name"), vec![Value::Str("edikt".into())]);
        assert_eq!(
            q(SAMPLE, ".package.version"),
            vec![Value::Str("0.1.0".into())]
        );
        assert_eq!(
            q(SAMPLE, ".dependencies.rowan"),
            vec![Value::Str("0.16".into())]
        );
    }

    #[test]
    fn set_preserves_layout_and_comments() {
        let out = edit_src(SAMPLE, r#".package.version = "0.2.0""#);
        assert!(out.contains("version = \"0.2.0\"   # semver"), "got: {out}");
        assert!(out.contains("# package"));
        assert!(out.contains("name = \"edikt\""));
    }

    #[test]
    fn typed_scalars_and_update() {
        let src = "[server]\nport = 8080\ndebug = false\n";
        assert_eq!(
            q(src, ".server.port"),
            vec![Value::Int(8080)],
            "TOML integers stay typed"
        );
        assert!(edit_src(src, ".server.port |= . + 1").contains("port = 8081"));
        assert!(edit_src(src, ".server.debug = true").contains("debug = true"));
    }

    #[test]
    fn del_and_new_key() {
        assert!(!edit_src(SAMPLE, "del(.dependencies.rowan)").contains("rowan"));
        // new key in an existing table
        assert!(edit_src(SAMPLE, r#".package.edition = "2024""#).contains("edition = \"2024\""));
    }

    #[test]
    fn roundtrips_every_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/toml");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("fixtures/toml directory") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            assert_eq!(
                parse(&src).unwrap().to_source(),
                src,
                "round-trip must be byte-identical: {}",
                path.display()
            );
            count += 1;
        }
        assert!(count >= 2, "expected toml fixtures, found {count}");
    }

    #[test]
    fn fixture_edit_bumps_version_keeping_comment() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/toml");
        let src = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        let out = edit_src(&src, r#".package.version = "0.2.0""#);
        assert!(
            out.contains("version = \"0.2.0\"          # bump on release"),
            "got: {out}"
        );
        assert!(out.contains("# Project configuration"));
    }
}
