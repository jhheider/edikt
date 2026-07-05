//! edikt TOML format module.
//!
//! Backed by `toml_edit`, whose whole purpose is format-preserving TOML edits -
//! so edikt gets lossless TOML (comments, spacing, table layout) essentially for
//! free, and the moat holds without a hand-rolled CST.

mod comments;
mod edit;
mod project;

pub use comments::emit_commented;
pub use edikt_core::EditError;
pub use edit::{apply, emit};

use edikt_core::{CommentKind, Document, Expr, Feature, Step, Value, eval};
use toml_edit::{DocumentMut, TableLike};

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
    fn to_commented(&self) -> Option<edikt_core::Commented> {
        Some(comments::to_commented(&self.doc))
    }
    fn set_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        let warnings = comments::set_node_comment(&mut self.doc, path, kind, text)?;
        self.had_comments = true;
        Ok(warnings)
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

    /// Apply a comment mutation through the Document write methods.
    fn cedit(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        edikt_core::apply_comment_mutation(&mut doc, &parse_expr(expr).unwrap()).unwrap();
        doc.to_source()
    }

    #[test]
    fn value_editing_paths() {
        // += and |= arithmetic; a pipe of edits; del - the apply arms.
        assert!(edit_src("n = 1\n", ".n += 4").contains("n = 5"));
        assert!(edit_src("n = 10\n", ".n |= . / 2").contains("n = 5"));
        assert_eq!(
            edit_src("a = 1\nb = 2\n", ".a = 9 | .b = 8"),
            "a = 9\nb = 8\n"
        );
        assert!(!edit_src("a = 1\nb = 2\n", "del(.a)").contains("a ="));
        // Setting an array and a nested object (inline table).
        assert!(edit_src("xs = []\n", ".xs = [1, 2, 3]").contains("xs = [1, 2, 3]"));
        let obj = edit_src("t = 0\n", r#".t = {"x": 1, "y": true}"#);
        assert!(
            obj.contains("x = 1") && obj.contains("y = true"),
            "got: {obj}"
        );
    }

    #[test]
    fn null_value_is_rejected() {
        // TOML has no null; setting one is a clean error, not a panic.
        let mut doc = parse("a = 1\n").unwrap();
        let err = apply(&mut doc, &parse_expr(".a = null").unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("null"), "got: {err}");
    }

    #[test]
    fn emit_shapes_tables_and_arrays() {
        // A top-level scalar stays a key; a nested object becomes a `[table]`;
        // an array of objects emits as an inline array of tables (and round-trips).
        let value = Value::Object(vec![
            ("name".into(), Value::Str("edikt".into())),
            (
                "server".into(),
                Value::Object(vec![("port".into(), Value::Int(8080))]),
            ),
            (
                "bin".into(),
                Value::Array(vec![
                    Value::Object(vec![("name".into(), Value::Str("a".into()))]),
                    Value::Object(vec![("name".into(), Value::Str("b".into()))]),
                ]),
            ),
        ]);
        let (out, warnings) = emit(&value).unwrap();
        assert!(warnings.is_empty());
        assert!(out.contains("name = \"edikt\""));
        assert!(
            out.contains("[server]") && out.contains("port = 8080"),
            "got: {out}"
        );
        assert!(out.contains("bin = [{"), "got: {out}");
        // Re-reading yields the same value.
        assert_eq!(parse(&out).unwrap().to_value(), value);
        // A non-object top level can't be a TOML document.
        assert!(emit(&Value::Int(1)).is_err());
    }

    #[test]
    fn comment_mutation_set_edit_delete() {
        // Set a head comment above a value; surrounding bytes untouched.
        assert_eq!(
            cedit(
                "# banner\n[a]\nx = 1  # inline x\ny = 2\n",
                ".a.y.# = \"note\""
            ),
            "# banner\n[a]\nx = 1  # inline x\n# note\ny = 2\n"
        );
        // Set an inline comment on a value.
        assert_eq!(
            cedit("[s]\nport = 8080\n", ".s.port.#.inline = \"listen\""),
            "[s]\nport = 8080 # listen\n"
        );
        // Set a head comment on a table header.
        assert_eq!(
            cedit("[a]\nx = 1\n", ".a.# = \"the a table\""),
            "# the a table\n[a]\nx = 1\n"
        );
        // Edit an existing comment via `|=`, then read it back.
        let edited = cedit("# old\nk = 1\n", ".k.# |= ascii_upcase");
        assert_eq!(edited, "# OLD\nk = 1\n");
        // Delete a comment, keeping the value and layout.
        assert_eq!(cedit("# drop\nk = 1\n", "del(.k.#)"), "k = 1\n");
    }

    #[test]
    fn comment_wraps_to_the_envelope() {
        // Longest line is short, so head wraps at the 80 floor.
        let long = "this is a fairly long explanatory comment that should wrap to the file width envelope and not run off forever";
        let out = cedit("k = 1\n", &format!(".k.# = \"{long}\""));
        for line in out.lines().filter(|l| l.starts_with("# ")) {
            assert!(line.chars().count() <= 80, "line too wide: {line:?}");
        }
        // And it re-reads (unwrapped) as the same text.
        let commented = parse(&out).unwrap().to_commented().unwrap();
        let path = parse_expr(".k.#").unwrap();
        assert_eq!(
            commented.resolve_comment(path.as_path().unwrap()),
            vec![Value::Str(long.into())]
        );
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

    // --- comment model (extraction + commented emit) -----------------------

    #[test]
    fn extracts_comments_by_kind() {
        let doc = parse(SAMPLE).unwrap();
        let c = doc.to_commented().unwrap();
        assert_eq!(c.to_value(), doc.to_value(), "shapes must match");
        let edikt_core::CommentedNode::Object(top) = &c.node else {
            panic!("expected object");
        };
        // `# package` precedes the [package] header.
        assert_eq!(top[0].0, "package");
        assert_eq!(top[0].1.comments.head, vec!["package"]);
        let edikt_core::CommentedNode::Object(pkg) = &top[0].1.node else {
            panic!("expected table object");
        };
        // `version = "0.1.0"   # semver` - the entry's inline comment.
        assert_eq!(pkg[1].0, "version");
        assert_eq!(pkg[1].1.comments.inline.as_deref(), Some("semver"));
    }

    #[test]
    fn extracts_array_element_and_trailing_comments() {
        let src = "xs = [\n  1, # one\n  # about two\n  2,\n]\n# trailing\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let edikt_core::CommentedNode::Object(top) = &c.node else {
            panic!("expected object");
        };
        let edikt_core::CommentedNode::Array(items) = &top[0].1.node else {
            panic!("expected array");
        };
        assert_eq!(items[0].comments.inline.as_deref(), Some("one"));
        assert_eq!(items[1].comments.head, vec!["about two"]);
        // Document-trailing comments land as the deepest last entry's foot.
        assert_eq!(items[1].comments.foot, vec!["trailing"]);
    }

    #[test]
    fn commented_emit_places_all_kinds() {
        let c = parse(SAMPLE).unwrap().to_commented().unwrap();
        let (out, warnings) = emit_commented(&c).unwrap();
        assert!(warnings.is_empty());
        assert!(out.contains("# package\n[package]"), "got: {out}");
        assert!(out.contains("version = \"0.1.0\" # semver"), "got: {out}");
        // The emitted TOML re-parses with the same comments and values.
        let again = parse(&out).unwrap().to_commented().unwrap();
        assert_eq!(again, c);
    }

    #[test]
    fn commented_emit_multiline_array_round_trips() {
        let src = "xs = [\n  1, # one\n  # about two\n  2,\n]\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let (out, warnings) = emit_commented(&c).unwrap();
        assert!(warnings.is_empty(), "got: {warnings:?}");
        let again = parse(&out).unwrap().to_commented().unwrap();
        assert_eq!(again, c, "emitted:\n{out}");
    }

    #[test]
    fn plain_emit_matches_commented_emit_without_comments() {
        for src in [SAMPLE, "a = 1\nb = 2\n\n[t]\nx = true\n"] {
            let v = parse(src).unwrap().to_value();
            let (plain, _) = emit(&v).unwrap();
            let (commented, _) = emit_commented(&edikt_core::Commented::from_value(&v)).unwrap();
            assert_eq!(plain, commented, "the two emitters must agree: {src:?}");
        }
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
