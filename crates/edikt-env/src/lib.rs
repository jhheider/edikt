//! edikt `.env` / `.properties` format module.
//!
//! Flat, string-valued, honest line-level editing only — no grammar, no
//! interpolation, no quoting semantics, no inline comments. `key=value` /
//! `key:value` entries, `#`/`!` comments, and blanks round-trip byte-for-byte.
//! Paths are a single `.key`; edits change only the targeted value or line.

mod edit;
mod parser;
mod project;
mod syntax;

pub use edikt_core::EditError;
pub use edit::apply;

use edikt_core::{Document, Expr, Feature, Value};
use syntax::{Sk, SyntaxNode};

/// Capabilities: comments only. Flat and string-valued — no nesting, arrays,
/// typed scalars, or sections.
pub const FEATURES: &[Feature] = &[Feature::Comments];

/// A parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub msg: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}
impl std::error::Error for ParseError {}

/// A parsed `.env` / `.properties` document, backed by a lossless CST.
pub struct Env {
    root: SyntaxNode,
}

impl Env {
    /// Access the underlying syntax tree.
    pub fn syntax(&self) -> &SyntaxNode {
        &self.root
    }

    /// Set the entry `key` to a scalar, format-preserving. The entry must exist.
    pub fn set(&mut self, key: &str, value: &Value) -> Result<(), EditError> {
        let text = edit::scalar_string(value)?;
        let entry = edit::find_entry(&self.root, key).ok_or_else(|| {
            EditError::new("key not found (creating new keys is not supported yet)")
        })?;
        let value_node = entry
            .children()
            .find(|n| n.kind() == Sk::Value)
            .ok_or_else(|| EditError::new("entry has no value slot"))?;
        let new_root = value_node.replace_with(edit::value_node_green(&text));
        self.root = SyntaxNode::new_root(new_root);
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
            self.root = root;
        }
        Ok(())
    }
}

/// Parse `.env` / `.properties` source into an [`Env`] document.
pub fn parse(src: &str) -> Result<Env, ParseError> {
    let root = SyntaxNode::new_root(parser::build(src));
    let malformed = root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == Sk::Error);
    if malformed {
        return Err(ParseError {
            msg: "invalid: a line is neither a comment nor key=value".to_string(),
        });
    }
    Ok(Env { root })
}

impl Document for Env {
    fn to_source(&self) -> String {
        edikt_syntax::to_source(&self.root)
    }
    fn to_value(&self) -> Value {
        project::to_value(&self.root)
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<(), EditError> {
        edit::apply(self, expr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edikt_core::eval;
    use edikt_core::parse as parse_expr;

    const SAMPLE: &str = "# service env\nDATABASE_URL=postgres://localhost/app\nDEBUG = true\nEMPTY=\nWITH_HASH=a#b\n";

    fn q(src: &str, expr: &str) -> Vec<Value> {
        let v = parse(src).unwrap().to_value();
        eval(&parse_expr(expr).unwrap(), &v).unwrap()
    }

    fn edit_src(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        apply(&mut doc, &parse_expr(expr).unwrap()).unwrap();
        doc.to_source()
    }

    #[test]
    fn roundtrips_byte_identically() {
        for src in [
            SAMPLE,
            "",
            "KEY=value",
            "a:1\nb : 2\n",
            "  spaced = yes  \n# comment\n",
            "! properties comment\nkey.with.dots=1\n",
        ] {
            assert_eq!(parse(src).unwrap().to_source(), src, "round-trip: {src:?}");
        }
    }

    #[test]
    fn projects_flat() {
        assert_eq!(
            q(SAMPLE, ".DATABASE_URL"),
            vec![Value::Str("postgres://localhost/app".into())]
        );
        assert_eq!(q(SAMPLE, ".DEBUG"), vec![Value::Str("true".into())]);
        assert_eq!(q(SAMPLE, ".EMPTY"), vec![Value::Str("".into())]);
        // No inline-comment parsing: the `#` stays in the value.
        assert_eq!(q(SAMPLE, ".WITH_HASH"), vec![Value::Str("a#b".into())]);
    }

    #[test]
    fn set_preserves_separator_style() {
        // `DATABASE_URL=...` has no spaces; `DEBUG = true` does. Keep each.
        assert!(
            edit_src(SAMPLE, r#".DATABASE_URL = "sqlite://x""#).contains("DATABASE_URL=sqlite://x")
        );
        assert!(edit_src(SAMPLE, ".DEBUG = false").contains("DEBUG = false"));
    }

    #[test]
    fn del_removes_line_and_keeps_comment() {
        let out = edit_src(SAMPLE, "del(.DEBUG)");
        assert!(!out.contains("DEBUG"));
        assert!(out.contains("# service env"));
        assert!(out.contains("DATABASE_URL="));
    }

    #[test]
    fn update_and_add_assign() {
        assert!(edit_src(SAMPLE, ".DEBUG |= ascii_upcase").contains("DEBUG = TRUE"));
        assert!(edit_src(SAMPLE, r#".DEBUG += "!""#).contains("DEBUG = true!"));
    }

    #[test]
    fn nesting_and_arrays_rejected() {
        let mut doc = parse(SAMPLE).unwrap();
        assert!(apply(&mut doc, &parse_expr(".DEBUG = [1]").unwrap()).is_err());
        assert!(apply(&mut doc, &parse_expr(".a.b = 1").unwrap()).is_err()); // no nesting
    }

    #[test]
    fn missing_and_malformed() {
        let mut doc = parse(SAMPLE).unwrap();
        assert!(apply(&mut doc, &parse_expr(".NOPE = 1").unwrap()).is_err());
        assert!(parse("not an entry line\n").is_err());
    }

    #[test]
    fn dotted_properties_keys_are_single_keys() {
        // In `.properties`, `app.name` is one key, addressed with a quoted field.
        let src = "app.name = edikt\nserver.port: 8080\n";
        assert_eq!(q(src, r#"."app.name""#), vec![Value::Str("edikt".into())]);
        assert_eq!(q(src, r#"."server.port""#), vec![Value::Str("8080".into())]);
        assert!(edit_src(src, r#"."server.port" = "9090""#).contains("server.port: 9090"));
    }

    #[test]
    fn roundtrips_every_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/env");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("fixtures/env directory") {
            let path = entry.unwrap().path();
            match path.extension().and_then(|e| e.to_str()) {
                Some("env") | Some("properties") => {}
                _ => continue,
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
        assert!(count >= 2, "expected env fixtures, found {count}");
    }
}
