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
pub use edikt_core::EditError;
pub use edit::apply;

use edikt_core::{CommentKind, Document, Expr, Feature, Step, Value};
use syntax::{Sk, SyntaxNode};

/// Comment kinds this format supports (empty ⇒ none); the comment
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
}

impl Ini {
    /// Access the underlying syntax tree.
    pub fn syntax(&self) -> &SyntaxNode {
        &self.root
    }

    /// Set the entry at `path` to a scalar, format-preserving. If the entry
    /// exists, only its value text changes. Otherwise a `key = value` line is
    /// inserted into the named section (creating the section if absent) or the
    /// preamble.
    pub fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        let text = edit::scalar_string(value)?;
        if let Some(entry) = edit::resolve_entry(&self.root, path) {
            let value_node = entry
                .children()
                .find(|n| n.kind() == Sk::Value)
                .ok_or_else(|| EditError::new("entry has no value slot"))?;
            let new_root = value_node.replace_with(edit::value_node_green(&text));
            self.root = SyntaxNode::new_root(new_root);
            return Ok(());
        }
        let (section, key) = match path {
            [Step::Field(k)] => (None, k.as_str()),
            [Step::Field(s), Step::Field(k)] => (Some(s.as_str()), k.as_str()),
            _ => return Err(EditError::new("INI paths are `.key` or `.section.key`")),
        };
        let new_src = edit::insert_entry(&self.to_source(), section, key, &text);
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
            self.root = root;
        }
        Ok(())
    }
}

/// Parse INI source into an [`Ini`] document.
pub fn parse(src: &str) -> Result<Ini, ParseError> {
    let root = SyntaxNode::new_root(parser::build(src));
    let malformed = root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == Sk::Error);
    if malformed {
        return Err(ParseError {
            msg: "invalid INI: a line is neither a comment, section, nor key=value".to_string(),
        });
    }
    Ok(Ini { root })
}

impl Document for Ini {
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
    fn has_comments(&self) -> bool {
        self.root
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == Sk::Comment)
    }
    fn to_commented(&self) -> Option<edikt_core::Commented> {
        Some(comments::to_commented(&self.root))
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
mod tests {
    use super::*;
    use edikt_core::eval;
    use edikt_core::parse as parse_expr;

    const SAMPLE: &str = "; app config\nglobal = 1\n\n[server]\nhost = 0.0.0.0\nport=8080        ; inline text\n\n[logging]\nlevel = info\n";

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
            "key=value",
            "[only-section]\n",
            "  indented = yes  \n; trailing comment\n",
            "a:1\nb : 2\n",
        ] {
            assert_eq!(parse(src).unwrap().to_source(), src, "round-trip: {src:?}");
        }
    }

    #[test]
    fn projects_sections_and_preamble() {
        assert_eq!(q(SAMPLE, ".global"), vec![Value::Str("1".into())]);
        assert_eq!(
            q(SAMPLE, ".server.host"),
            vec![Value::Str("0.0.0.0".into())]
        );
        assert_eq!(q(SAMPLE, ".server.port"), vec![Value::Str("8080".into())]);
        assert_eq!(q(SAMPLE, ".logging.level"), vec![Value::Str("info".into())]);
        assert_eq!(
            q(SAMPLE, ".server | keys"),
            vec![Value::Array(vec![
                Value::Str("host".into()),
                Value::Str("port".into()),
            ])]
        );
    }

    #[test]
    fn set_preserves_key_and_spacing() {
        // `port=8080` has no spaces around `=`; the edit keeps that.
        let out = edit_src(SAMPLE, r#".server.port = "9090""#);
        assert!(out.contains("port=9090"), "got: {out}");
        assert!(out.contains("host = 0.0.0.0"));
        assert!(out.contains("; app config"));
    }

    #[test]
    fn set_preamble_and_spaced_entry() {
        assert!(edit_src(SAMPLE, ".global = 2").contains("global = 2"));
        assert!(edit_src(SAMPLE, r#".server.host = "127.0.0.1""#).contains("host = 127.0.0.1"));
    }

    #[test]
    fn del_removes_the_line_only() {
        let out = edit_src(SAMPLE, "del(.server.host)");
        assert!(!out.contains("host = 0.0.0.0"));
        assert!(out.contains("port=8080"));
        assert!(out.contains("[server]"));
        assert!(out.contains("[logging]"));
    }

    #[test]
    fn update_and_add_assign_strings() {
        assert!(edit_src(SAMPLE, ".logging.level |= ascii_upcase").contains("level = INFO"));
        assert!(edit_src(SAMPLE, r#".logging.level += "!""#).contains("level = info!"));
    }

    #[test]
    fn cannot_store_array() {
        let mut doc = parse(SAMPLE).unwrap();
        assert!(apply(&mut doc, &parse_expr(".global = [1, 2]").unwrap()).is_err());
    }

    #[test]
    fn malformed_line_errors() {
        assert!(parse("this is not ini\n").is_err());
    }

    #[test]
    fn creates_new_key_in_existing_section() {
        let src = "[server]\nhost = x\n\n[logging]\nlevel = info\n";
        assert_eq!(
            edit_src(src, r#".server.port = "8080""#),
            "[server]\nhost = x\nport = 8080\n\n[logging]\nlevel = info\n"
        );
    }

    #[test]
    fn creates_new_section_and_preamble_key() {
        assert_eq!(
            edit_src("[server]\nhost = x\n", r#".db.url = "pg""#),
            "[server]\nhost = x\n\n[db]\nurl = pg\n"
        );
        // preamble key added before the first section, keeping the leading comment
        let out = edit_src(SAMPLE, ".added = 1");
        assert!(out.contains("; app config"));
        assert!(out.contains("added = 1"));
        assert!(parse(&out).is_ok());
    }

    #[test]
    fn roundtrips_every_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ini");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("fixtures/ini directory") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("ini") {
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
        assert!(count >= 3, "expected several ini fixtures, found {count}");
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
        // `; app config` precedes the preamble key `global`.
        assert_eq!(top[0].0, "global");
        assert_eq!(top[0].1.comments.head, vec!["app config"]);
        // `port=8080        ; inline text` — the entry's inline comment.
        let edikt_core::CommentedNode::Object(server) = &top[1].1.node else {
            panic!("expected section object");
        };
        assert_eq!(server[1].0, "port");
        assert_eq!(server[1].1.comments.inline.as_deref(), Some("inline text"));
    }

    #[test]
    fn section_head_and_trailing_foot() {
        let src = "; before server\n[server]  ; svc\nhost = x\n; done\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let edikt_core::CommentedNode::Object(top) = &c.node else {
            panic!("expected object");
        };
        assert_eq!(top[0].1.comments.head, vec!["before server"]);
        assert_eq!(top[0].1.comments.inline.as_deref(), Some("svc"));
        let edikt_core::CommentedNode::Object(server) = &top[0].1.node else {
            panic!("expected section object");
        };
        assert_eq!(server[0].1.comments.foot, vec!["done"]);
    }

    #[test]
    fn commented_emit_places_all_kinds() {
        let c = parse(SAMPLE).unwrap().to_commented().unwrap();
        let (out, warnings) = emit_commented(&c).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            out,
            "; app config\nglobal = 1\n\n[server]\nhost = 0.0.0.0\nport = 8080  ; inline text\n\n[logging]\nlevel = info\n"
        );
        // The emitted INI re-parses with the same comments and values.
        let again = parse(&out).unwrap().to_commented().unwrap();
        assert_eq!(again, c);
    }

    #[test]
    fn plain_emit_matches_comment_free_output() {
        let v = parse(SAMPLE).unwrap().to_value();
        let (out, warnings) = emit(&v).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            out,
            "global = 1\n\n[server]\nhost = 0.0.0.0\nport = 8080\n\n[logging]\nlevel = info\n"
        );
    }

    #[test]
    fn fixture_edit_preserves_inline_comment() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ini");
        let src = std::fs::read_to_string(dir.join("app.ini")).unwrap();
        let out = edit_src(&src, r#".server.port = "9090""#);
        assert!(
            out.contains("port = 9090          ; the listen port"),
            "got: {out}"
        );
        assert!(out.contains("; Application configuration"));
    }
}
