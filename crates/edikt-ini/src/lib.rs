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
            self.root = SyntaxNode::new_root(root.green().into_owned());
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
    fn apply(&mut self, expr: &Expr) -> Result<Vec<String>, EditError> {
        edit::apply(self, expr).map(|()| Vec::new())
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

    fn cedit(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        edikt_core::apply_comment_mutation(&mut doc, &parse_expr(expr).unwrap()).unwrap();
        doc.to_source()
    }

    #[test]
    fn comment_mutation_entry_and_header() {
        // Head above an entry.
        assert_eq!(
            cedit(
                "[server]\nhost = x\nport = 8080\n",
                ".server.port.# = \"listen\""
            ),
            "[server]\nhost = x\n; listen\nport = 8080\n"
        );
        // Inline on an entry.
        assert_eq!(
            cedit("[s]\nport = 8080\n", ".s.port.#.inline = \"the port\""),
            "[s]\nport = 8080  ; the port\n"
        );
        // Head above a section header.
        assert_eq!(
            cedit("[server]\nhost = x\n", ".server.# = \"web\""),
            "; web\n[server]\nhost = x\n"
        );
        // Replace an existing inline.
        assert_eq!(
            cedit("port = 8080  ; old\n", ".port.#.inline = \"new\""),
            "port = 8080  ; new\n"
        );
        // Delete an inline.
        assert_eq!(
            cedit("port = 8080  ; drop\n", "del(.port.#.inline)"),
            "port = 8080\n"
        );
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
    fn del_entries_in_pipeline() {
        assert_eq!(
            edit_src(
                "[server]\nhost = 0.0.0.0\nport=8080\n",
                "del(.server.host) | del(.server.port)"
            ),
            "[server]\n"
        );
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
    fn iterate_mutation_errors_are_honest() {
        // `[]` in a mutation path is the array family this flat format lacks.
        // A value that exists gives the precise type error (matching the
        // query side), not a misleading "path not found".
        assert!(
            edit_err("[s]\na=1\n", ".s.a[] |= . * 2").contains("cannot iterate over string"),
            "scalar iterate"
        );
        assert!(
            edit_err("[s]\na=1\n", ".s.a[] += 1").contains("cannot iterate over string"),
            "nested scalar iterate"
        );
        // A section iterate can't fan out either; say so clearly.
        assert!(
            edit_err("[s]\na=1\n", ".s[] |= . * 2").contains("not supported for INI"),
            "section iterate"
        );
        // Plain `=` already messages the path shape.
        assert!(
            edit_err("[s]\na=1\n", ".s[] = 5").contains("INI paths are `.key` or `.section.key`"),
            "assign shape"
        );
        // A missing iterate target stays a miss (no error), not a lie.
        assert!(
            edit_err("[s]\na=1\n", ".nope[] |= . * 2").contains("not supported for INI"),
            "absent target"
        );
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
        // `port=8080        ; inline text`: the entry's inline comment.
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

    // --- edit paths: piped mutations, bad paths, del arity, insert edges ---

    fn edit_err(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        apply(&mut doc, &parse_expr(expr).unwrap())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn piped_mutations_apply_in_order() {
        // Two assignments joined by `|` both land.
        let out = edit_src(SAMPLE, r#".global = 2 | .server.host = "127.0.0.1""#);
        assert!(out.contains("global = 2"), "got: {out}");
        assert!(out.contains("host = 127.0.0.1"), "got: {out}");
    }

    #[test]
    fn del_requires_exactly_one_path() {
        // `;`-separated args make two arguments; `del` takes exactly one.
        assert!(edit_err(SAMPLE, "del(.a; .b)").contains("del(...) takes one path argument"));
    }

    #[test]
    fn non_mutation_expr_is_rejected_by_apply() {
        assert!(
            edit_err(SAMPLE, ".server.host")
                .contains("expected an assignment (`path = value`) or `del(path)`")
        );
    }

    #[test]
    fn three_step_path_is_not_a_valid_ini_target() {
        assert!(
            edit_err(SAMPLE, r#".a.b.c = "x""#).contains("INI paths are `.key` or `.section.key`")
        );
    }

    #[test]
    fn cannot_store_object() {
        assert!(edit_err(SAMPLE, ".global = {}").contains("cannot store an array or object"));
    }

    #[test]
    fn insert_into_source_without_trailing_newline() {
        // A new key in an existing section: the missing final newline is supplied
        // before the inserted line.
        assert_eq!(
            edit_src("[server]\nhost = x", r#".server.port = "8080""#),
            "[server]\nhost = x\nport = 8080\n"
        );
        // A new section appended at EOF, likewise terminating the last line first.
        assert_eq!(
            edit_src("[server]\nhost = x", r#".db.url = "pg""#),
            "[server]\nhost = x\n\n[db]\nurl = pg\n"
        );
    }

    #[test]
    fn document_trait_surface() {
        // `features()` reports the static capability set.
        assert_eq!(Document::features(&parse("k = v\n").unwrap()), FEATURES);
        // `has_comments()`: false without any comment token, true with one.
        assert!(!Document::has_comments(&parse("k = v\n").unwrap()));
        assert!(Document::has_comments(&parse("; c\nk = v\n").unwrap()));
        // `syntax()` exposes the tree root.
        assert_eq!(parse("k = v\n").unwrap().syntax().kind(), Sk::Root);
        // The trait `apply` routes to the format-preserving edit path.
        let mut doc = parse("[s]\nk = v\n").unwrap();
        let d: &mut dyn Document = &mut doc;
        d.apply(&parse_expr(r#".s.k = "z""#).unwrap()).unwrap();
        assert!(d.to_source().contains("k = z"));
    }

    // --- comment edits: headers, deletes, error paths ----------------------

    fn cedit_err(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        edikt_core::apply_comment_mutation(&mut doc, &parse_expr(expr).unwrap())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn inline_comment_on_section_header() {
        // Set an inline on a `[section]` header: it has no Value node, so the
        // comment hangs off the `]`.
        assert_eq!(
            cedit("[server]\nhost = x\n", r#".server.#.inline = "svc""#),
            "[server]  ; svc\nhost = x\n"
        );
        // Delete it again.
        assert_eq!(
            cedit("[server]  ; svc\nhost = x\n", "del(.server.#.inline)"),
            "[server]\nhost = x\n"
        );
    }

    #[test]
    fn delete_head_and_foot_comments() {
        // A head block above a preamble entry.
        assert_eq!(cedit("; note\nkey = v\n", "del(.key.#)"), "key = v\n");
        // A foot block below an entry.
        assert_eq!(cedit("key = v\n; foot\n", "del(.key.#.foot)"), "key = v\n");
    }

    #[test]
    fn delete_comment_on_missing_target_is_a_noop() {
        assert_eq!(cedit("key = v\n", "del(.missing.#)"), "key = v\n");
    }

    #[test]
    fn document_level_comment_edit_is_unsupported() {
        assert!(
            cedit_err("key = v\n", r#".# = "banner""#)
                .contains("document-level (`.#`) comment editing for INI is a follow-up")
        );
    }

    #[test]
    fn deep_comment_path_is_rejected() {
        assert!(
            cedit_err("key = v\n", r#".a.b.c.# = "x""#)
                .contains("INI comment paths are `.key`, `.section`, or `.section.key`")
        );
    }

    #[test]
    fn emit_commented_rejects_non_object_root() {
        let c = edikt_core::Commented::from_value(&Value::Str("x".into()));
        let err = emit_commented(&c).unwrap_err().to_string();
        assert!(
            err.contains("INI output requires a top-level object"),
            "got: {err}"
        );
    }

    #[test]
    fn emit_commented_places_banners_headers_feet_and_flattens() {
        use edikt_core::{Commented, CommentedNode, Comments};
        // A document banner + foot, a preamble entry with a foot, and a section
        // carrying head/inline/foot around a nested object (which flattens).
        let tree = Commented {
            comments: Comments {
                head: vec!["banner line".into()],
                inline: None,
                foot: vec!["trailing note".into()],
            },
            node: CommentedNode::Object(vec![
                (
                    "g".into(),
                    Commented {
                        comments: Comments {
                            head: Vec::new(),
                            inline: None,
                            foot: vec!["after g".into()],
                        },
                        node: CommentedNode::Scalar(Value::Str("1".into())),
                    },
                ),
                (
                    "server".into(),
                    Commented {
                        comments: Comments {
                            head: vec!["about server".into()],
                            inline: Some("svc".into()),
                            foot: vec!["done server".into()],
                        },
                        node: CommentedNode::Object(vec![
                            ("host".into(), Commented::scalar(Value::Str("x".into()))),
                            (
                                "tls".into(),
                                Commented {
                                    comments: Comments::default(),
                                    node: CommentedNode::Object(vec![(
                                        "enabled".into(),
                                        Commented::scalar(Value::Str("true".into())),
                                    )]),
                                },
                            ),
                        ]),
                    },
                ),
            ]),
        };
        let (out, warnings) = emit_commented(&tree).unwrap();
        assert_eq!(
            out,
            "; banner line\ng = 1\n; after g\n\n; about server\n[server]  ; svc\nhost = x\ntls.enabled = true\n; done server\n; trailing note\n"
        );
        assert_eq!(
            warnings,
            vec!["nested/array values were flattened to dotted keys".to_string()]
        );
    }

    // --- parser edges: CRLF terminators, unclosed headers ------------------

    #[test]
    fn crlf_lines_round_trip_and_read() {
        let src = "a = 1\r\nb = 2\r\n";
        assert_eq!(parse(src).unwrap().to_source(), src);
        assert_eq!(q(src, ".a"), vec![Value::Str("1".into())]);
    }

    #[test]
    fn unclosed_header_is_flagged_but_bare_bracket_round_trips() {
        // `[` with content but no `]` is an error line (parse fails, exit 2).
        assert!(parse("[unclosed\n").is_err());
        // A bare `[` (nothing after the bracket) is losslessly preserved.
        assert_eq!(parse("[\n").unwrap().to_source(), "[\n");
    }

    #[test]
    fn syntax_kind_raw_round_trips() {
        use rowan::Language;
        for k in [
            Sk::Ws,
            Sk::Newline,
            Sk::Comment,
            Sk::Open,
            Sk::Close,
            Sk::Name,
            Sk::Key,
            Sk::Sep,
            Sk::ValStr,
            Sk::Error,
            Sk::Value,
            Sk::Entry,
            Sk::Header,
            Sk::Section,
            Sk::Root,
        ] {
            let raw = crate::syntax::IniLang::kind_to_raw(k);
            assert_eq!(crate::syntax::IniLang::kind_from_raw(raw), k);
        }
    }
}
