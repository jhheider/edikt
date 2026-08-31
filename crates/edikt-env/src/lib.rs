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
}

impl Env {
    /// Access the underlying syntax tree.
    pub fn syntax(&self) -> &SyntaxNode {
        &self.root
    }

    /// Set the entry `key` to a scalar, format-preserving. If `key` doesn't
    /// exist, a new `key=value` line is appended.
    pub fn set(&mut self, key: &str, value: &Value) -> Result<(), EditError> {
        let text = edit::scalar_string(value)?;
        match edit::find_entry(&self.root, key) {
            Some(entry) => {
                let value_node = entry
                    .children()
                    .find(|n| n.kind() == Sk::Value)
                    .ok_or_else(|| EditError::new("entry has no value slot"))?;
                let new_root = value_node.replace_with(edit::value_node_green(&text));
                self.root = SyntaxNode::new_root(new_root);
            }
            None => {
                let mut src = self.to_source();
                if !src.is_empty() && !src.ends_with('\n') {
                    src.push('\n');
                }
                src.push_str(&format!("{key}={text}\n"));
                self.root = SyntaxNode::new_root(parser::build(&src));
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
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        let key = comments::single_key(path)?;
        let (source, warnings) = comments::set_key_comment(&self.root, key, kind, text)?;
        self.root = SyntaxNode::new_root(parser::build(&source));
        Ok(warnings)
    }
    fn delete_comment(
        &mut self,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        let key = comments::single_key(path)?;
        let source = comments::delete_key_comment(&self.root, key, kind)?;
        self.root = SyntaxNode::new_root(parser::build(&source));
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

    fn cedit(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        edikt_core::apply_comment_mutation(&mut doc, &parse_expr(expr).unwrap()).unwrap();
        doc.to_source()
    }

    #[test]
    fn comment_mutation_head_foot_and_inline_refused() {
        // Head above an entry.
        assert_eq!(
            cedit("DATABASE_URL=x\nDEBUG=true\n", ".DEBUG.# = \"verbose\""),
            "DATABASE_URL=x\n# verbose\nDEBUG=true\n"
        );
        // Foot after an entry.
        assert_eq!(
            cedit("A=1\nB=2\n", ".B.#.foot = \"end\""),
            "A=1\nB=2\n# end\n"
        );
        // Replace an existing head; delete it.
        assert_eq!(
            cedit("# old\nK=v\n", ".K.# |= ascii_upcase"),
            "# OLD\nK=v\n"
        );
        assert_eq!(cedit("# drop\nK=v\n", "del(.K.#)"), "K=v\n");
        // Inline is refused; `.env` has no inline comments.
        let mut doc = parse("K=v\n").unwrap();
        let err = edikt_core::apply_comment_mutation(
            &mut doc,
            &parse_expr(".K.#.inline = \"x\"").unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no inline comments"), "got: {err}");
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
            "A=1\r\nB=2\r\n", // CRLF terminators preserved
            "A=1\n\nB=2\n",   // a blank line between entries
            "\n\n",           // blank-only document
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
    fn del_entries_in_pipeline() {
        assert_eq!(edit_src("A=1\nB=2\nC=3\n", "del(.A) | del(.B)"), "C=3\n");
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
    fn malformed_line_errors() {
        assert!(parse("not an entry line\n").is_err());
    }

    #[test]
    fn creates_new_key_by_appending() {
        assert_eq!(edit_src("A=1\n", r#".B = "2""#), "A=1\nB=2\n");
        // appends even when the file lacks a trailing newline
        assert_eq!(edit_src("A=1", r#".B = "2""#), "A=1\nB=2\n");
        // preserves the existing content and comments
        let out = edit_src(SAMPLE, r#".NEW_FLAG = "on""#);
        assert!(out.contains("# service env"));
        assert!(out.ends_with("NEW_FLAG=on\n"));
    }

    #[test]
    fn dotted_properties_keys_are_single_keys() {
        // In `.properties`, `app.name` is one key, addressed with a quoted field.
        let src = "app.name = edikt\nserver.port: 8080\n";
        assert_eq!(q(src, r#"."app.name""#), vec![Value::Str("edikt".into())]);
        assert_eq!(q(src, r#"."server.port""#), vec![Value::Str("8080".into())]);
        assert!(edit_src(src, r#"."server.port" = "9090""#).contains("server.port: 9090"));
    }

    // --- comment model (extraction + commented emit) -----------------------

    #[test]
    fn extracts_head_comments_and_trailing_foot() {
        let src = "# service env\nDATABASE_URL=x\n# stop here\n";
        let doc = parse(src).unwrap();
        let c = doc.to_commented().unwrap();
        assert_eq!(c.to_value(), doc.to_value(), "shapes must match");
        let edikt_core::CommentedNode::Object(entries) = &c.node else {
            panic!("expected object");
        };
        assert_eq!(entries[0].1.comments.head, vec!["service env"]);
        assert_eq!(entries[0].1.comments.foot, vec!["stop here"]);
    }

    #[test]
    fn commented_emit_round_trips_and_remaps_inline() {
        let c = parse(SAMPLE).unwrap().to_commented().unwrap();
        let (out, warnings) = emit_commented(&c).unwrap();
        assert!(warnings.is_empty());
        assert!(out.starts_with("# service env\nDATABASE_URL="));
        assert_eq!(parse(&out).unwrap().to_commented().unwrap(), c);

        // An inline comment (from a richer format) moves to its own line, and
        // that remap warns.
        let mut inline = edikt_core::Commented::from_value(&Value::Object(vec![(
            "PORT".into(),
            Value::Str("80".into()),
        )]));
        let edikt_core::CommentedNode::Object(entries) = &mut inline.node else {
            unreachable!();
        };
        entries[0].1.comments.inline = Some("the listen port".into());
        let (out2, warnings2) = emit_commented(&inline).unwrap();
        assert_eq!(out2, "# the listen port\nPORT=80\n");
        assert_eq!(warnings2.len(), 1);
        assert!(
            warnings2[0].contains("inline comments moved"),
            "got: {warnings2:?}"
        );
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

    // --- edit dispatch: pipe, del arity, non-assignment ---------------------

    #[test]
    fn piped_mutations_apply_in_order() {
        assert_eq!(
            edit_src("A=1\nB=2\n", r#".A = "x" | .B = "y""#),
            "A=x\nB=y\n"
        );
    }

    #[test]
    fn del_with_wrong_arity_errors() {
        // Function args are `;`-separated, so `del(.A; .B)` is two arguments.
        let mut doc = parse("A=1\nB=2\n").unwrap();
        let err = apply(&mut doc, &parse_expr("del(.A; .B)").unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("del(...) takes one path argument"),
            "got: {err}"
        );
    }

    #[test]
    fn a_bare_query_is_not_a_mutation() {
        let mut doc = parse("A=1\n").unwrap();
        let err = apply(&mut doc, &parse_expr(".A").unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected an assignment"), "got: {err}");
    }

    // --- comment paths: document-level and nested are refused ---------------

    #[test]
    fn comment_document_banner_is_a_followup() {
        let mut doc = parse("A=1\n").unwrap();
        let err =
            edikt_core::apply_comment_mutation(&mut doc, &parse_expr(r#".# = "banner""#).unwrap())
                .unwrap_err()
                .to_string();
        assert!(err.contains("document-level"), "got: {err}");
    }

    #[test]
    fn nested_comment_path_is_refused() {
        let mut doc = parse("A=1\n").unwrap();
        let err =
            edikt_core::apply_comment_mutation(&mut doc, &parse_expr(r#".a.b.# = "x""#).unwrap())
                .unwrap_err()
                .to_string();
        assert!(
            err.contains("flat: comment paths are a single"),
            "got: {err}"
        );
    }

    // --- comment deletion: inline and missing-key no-ops --------------------

    #[test]
    fn deleting_inline_comment_is_a_noop() {
        // `.env` has no inline comments, so `del(.K.#.inline)` changes nothing.
        assert_eq!(cedit("# h\nK=v\n", "del(.K.#.inline)"), "# h\nK=v\n");
    }

    #[test]
    fn deleting_comment_on_missing_key_is_a_noop() {
        assert_eq!(cedit("# h\nK=v\n", "del(.NOPE.#)"), "# h\nK=v\n");
    }

    // --- extraction: trailing comments with no entries ----------------------

    #[test]
    fn all_comments_no_entries_become_document_foot() {
        let doc = parse("# just a note\n# and another\n").unwrap();
        let c = doc.to_commented().unwrap();
        let edikt_core::CommentedNode::Object(entries) = &c.node else {
            panic!("expected object");
        };
        assert!(entries.is_empty(), "no entries in a comment-only file");
        assert_eq!(
            c.comments.foot,
            vec!["just a note".to_string(), "and another".to_string()]
        );
    }

    // --- emission edge cases ------------------------------------------------

    #[test]
    fn emit_rejects_a_top_level_scalar() {
        let err = emit(&Value::Str("x".into())).unwrap_err().to_string();
        assert!(err.contains("requires a top-level object"), "got: {err}");
    }

    #[test]
    fn emit_carries_an_entry_foot_comment() {
        let mut c = edikt_core::Commented::from_value(&Value::Object(vec![(
            "A".into(),
            Value::Str("1".into()),
        )]));
        let edikt_core::CommentedNode::Object(entries) = &mut c.node else {
            unreachable!();
        };
        entries[0].1.comments.foot.push("tail".into());
        let (out, warnings) = emit_commented(&c).unwrap();
        assert_eq!(out, "A=1\n# tail\n");
        assert!(warnings.is_empty());
    }

    #[test]
    fn emit_flattens_nesting_and_warns() {
        let (out, warnings) = emit(&Value::Object(vec![(
            "a".into(),
            Value::Object(vec![("b".into(), Value::Str("1".into()))]),
        )]))
        .unwrap();
        assert_eq!(out, "a.b=1\n");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("flattened"), "got: {warnings:?}");
    }

    // --- Document trait surface + syntax accessor ---------------------------

    #[test]
    fn syntax_accessor_exposes_the_tree() {
        let doc = parse("A=1\n# c\n").unwrap();
        assert_eq!(doc.syntax().text().to_string(), "A=1\n# c\n");
    }

    #[test]
    fn document_trait_features_comments_and_apply() {
        let mut doc = parse("# note\nA=1\n").unwrap();
        assert_eq!(doc.features(), FEATURES);
        assert!(doc.has_comments());
        assert!(!parse("A=1\n").unwrap().has_comments());
        // The trait's `apply` dispatches into the format-preserving edit path.
        Document::apply(&mut doc, &parse_expr(r#".A = "2""#).unwrap()).unwrap();
        assert_eq!(doc.to_source(), "# note\nA=2\n");
    }

    // --- Language mapping invariant -----------------------------------------

    #[test]
    fn language_kind_mapping_roundtrips() {
        use rowan::Language;
        for k in [
            Sk::Ws,
            Sk::Newline,
            Sk::Comment,
            Sk::Key,
            Sk::Sep,
            Sk::ValStr,
            Sk::Error,
            Sk::Value,
            Sk::Entry,
            Sk::Root,
        ] {
            let raw = crate::syntax::EnvLang::kind_to_raw(k);
            assert_eq!(crate::syntax::EnvLang::kind_from_raw(raw), k);
        }
    }
}
