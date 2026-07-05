//! edikt KDL format module.
//!
//! Backed by [`kdl`](https://crates.io/crates/kdl) (kdl-rs), whose document is
//! format-preserving by construction - the `toml_edit` of KDL - so edikt gets
//! lossless KDL (comments, spacing, node layout) without a hand-rolled CST.
//!
//! KDL nodes carry positional **arguments**, `key=value` **properties**, and a
//! **children** block, none of which the flat `Value` model has a slot for
//! directly. The projection convention (documented in CLAUDE.md and
//! implemented in [`project`]) maps them: nodes group by name (repeats ->
//! arrays), a node is its children object / lone scalar / argument array, and a
//! node mixing arguments with props/children puts the arguments under the
//! reserved key `"-"`.

mod comments;
mod edit;
mod project;

pub use comments::emit_commented;
pub use edikt_core::EditError;
pub use edit::{apply, emit};

use edikt_core::{CommentKind, Document, Expr, Feature, Step, Value};
use kdl::KdlDocument;

/// Comment kinds this format supports (empty => none); the comment
/// capability, subsuming the boolean `Feature::Comments`.
pub const COMMENT_KINDS: &[CommentKind] =
    &[CommentKind::Head, CommentKind::Inline, CommentKind::Foot];

/// Capabilities of KDL: comments, nesting, arrays, and typed scalars.
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

/// A parsed KDL document, backed by kdl-rs's format-preserving tree.
pub struct Kdl {
    doc: KdlDocument,
}

impl Kdl {
    /// Set the value at `path`, format-preserving. Existing arguments and
    /// properties update in place; a missing leaf node is created; a run of
    /// repeated nodes extends when the assignment's array matches its prefix.
    /// Replacing a whole node body wholesale is refused (like YAML).
    pub fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        if path.is_empty() {
            return Err(EditError::new("cannot set the whole document"));
        }
        edit::set_in_doc(&mut self.doc, path, value, 0)
    }

    /// The value at `path`, or `None`.
    pub fn value_at(&self, path: &[Step]) -> Option<Value> {
        edikt_core::eval(&Expr::Path(path.to_vec()), &self.to_value())
            .ok()?
            .into_iter()
            .next()
    }

    /// Delete the node / property / argument at `path` (a miss is a no-op).
    pub fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        if path.is_empty() {
            return Err(EditError::new("del(.) is not allowed"));
        }
        edit::delete_in_doc(&mut self.doc, path)
    }
}

/// Parse KDL source into a [`Kdl`] document.
pub fn parse(src: &str) -> Result<Kdl, ParseError> {
    let doc = KdlDocument::parse(src).map_err(|e| ParseError { msg: e.to_string() })?;
    Ok(Kdl { doc })
}

impl Document for Kdl {
    fn to_source(&self) -> String {
        self.doc.to_string()
    }
    fn to_value(&self) -> Value {
        project::doc_to_value(&self.doc)
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<(), EditError> {
        edit::apply(self, expr)
    }
    fn has_comments(&self) -> bool {
        // kdl-rs keeps comments in decor strings; a `//` or `/*` anywhere in
        // the serialized form means the source carried one.
        let s = self.doc.to_string();
        s.contains("//") || s.contains("/*")
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
        comments::set_node_comment(&mut self.doc, path, kind, text)
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
    use edikt_core::eval;
    use edikt_core::parse as parse_expr;

    const SAMPLE: &str = "// window manager config\nlayout \"tall\" gaps=8 {\n    border width=2\n}\nbind \"Mod+h\" \"focus-left\"\nbind \"Mod+l\" \"focus-right\"\n";

    fn q(src: &str, expr: &str) -> Vec<Value> {
        eval(&parse_expr(expr).unwrap(), &parse(src).unwrap().to_value()).unwrap()
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
    fn comment_mutation_set_edit_delete() {
        // Head comment on a top-level node.
        assert_eq!(
            cedit("a 1\nb 2\n", ".b.# = \"note\""),
            "a 1\n// note\nb 2\n"
        );
        // Head on a nested child, indented to match.
        assert_eq!(
            cedit(
                "layout {\n    border width=2\n}\n",
                ".layout.border.# = \"frame\""
            ),
            "layout {\n    // frame\n    border width=2\n}\n"
        );
        // Inline on a node.
        assert_eq!(
            cedit(
                "server {\n    port 8080\n}\n",
                ".server.port.#.inline = \"listen\""
            ),
            "server {\n    port 8080 // listen\n}\n"
        );
        // Edit via `|=` and read back.
        assert_eq!(
            cedit("// old\nn 1\n", ".n.# |= ascii_upcase"),
            "// OLD\nn 1\n"
        );
        // Delete.
        assert_eq!(cedit("// drop\nn 1\n", "del(.n.#)"), "n 1\n");
    }

    #[test]
    fn comment_on_repeated_node_needs_an_index() {
        // `.bind` is repeated; a bare comment target is ambiguous.
        let mut doc = parse(SAMPLE).unwrap();
        let err =
            edikt_core::apply_comment_mutation(&mut doc, &parse_expr(".bind.# = \"x\"").unwrap())
                .unwrap_err()
                .to_string();
        assert!(err.contains("repeated"), "got: {err}");
        // Indexing one occurrence works.
        let out = cedit(SAMPLE, ".bind[0].# = \"first bind\"");
        assert!(out.contains("// first bind\nbind \"Mod+h\""), "got: {out}");
    }

    #[test]
    fn roundtrips_byte_identically() {
        for src in [
            SAMPLE,
            "",
            "node\n",
            "a 1\nb 2\n",
            "parent {\n    child key=1\n}\n",
            "// leading\nnode arg /* inline */ prop=#true\n",
        ] {
            assert_eq!(parse(src).unwrap().to_source(), src, "round-trip: {src:?}");
        }
    }

    #[test]
    fn projects_args_props_children() {
        // `layout` mixes an argument with a property + children -> object, with
        // the argument under the reserved "-" key.
        assert_eq!(
            q(SAMPLE, ".layout.[\"-\"]"),
            vec![Value::Str("tall".into())]
        );
        assert_eq!(q(SAMPLE, ".layout.gaps"), vec![Value::Int(8)]);
        // Nested child node -> nested object.
        assert_eq!(q(SAMPLE, ".layout.border.width"), vec![Value::Int(2)]);
        // `bind` is arguments-only, so each occurrence is a plain array; the
        // repeated name makes `.bind` an array of those arrays.
        assert_eq!(
            q(SAMPLE, ".bind[0]"),
            vec![Value::Array(vec![
                Value::Str("Mod+h".into()),
                Value::Str("focus-left".into()),
            ])]
        );
        assert_eq!(q(SAMPLE, ".bind[1][0]"), vec![Value::Str("Mod+l".into())]);
        assert_eq!(q(SAMPLE, ".bind | length"), vec![Value::Int(2)]);
    }

    #[test]
    fn lone_argument_is_a_scalar() {
        assert_eq!(
            q("title \"hello\"\n", ".title"),
            vec![Value::Str("hello".into())]
        );
        assert_eq!(q("count 42\n", ".count"), vec![Value::Int(42)]);
        assert_eq!(q("flag\n", ".flag"), vec![Value::Null]);
    }

    #[test]
    fn set_property_in_place_keeps_layout() {
        // Only the `gaps=8` value changes; the comment, the child, everything
        // else stays byte-identical.
        let out = edit_src(SAMPLE, ".layout.gaps = 16");
        assert!(out.contains("layout \"tall\" gaps=16 {"), "got: {out}");
        assert!(out.contains("// window manager config"));
        assert!(out.contains("border width=2"));
    }

    #[test]
    fn set_nested_child_and_argument() {
        assert!(edit_src(SAMPLE, ".layout.border.width = 4").contains("border width=4"));
        // Index into a repeated node's (args-only) array. The value round-trips
        // as a string, though KDL v2 may render it as a bare identifier.
        let out = edit_src(SAMPLE, ".bind[0][1] = \"focus-up\"");
        assert_eq!(q(&out, ".bind[0][1]"), vec![Value::Str("focus-up".into())]);
        assert!(out.contains("bind \"Mod+h\""), "arg 0 untouched: {out}");
    }

    #[test]
    fn update_assign_computes() {
        assert!(edit_src(SAMPLE, ".layout.gaps |= . + 2").contains("gaps=10"));
    }

    #[test]
    fn creates_new_leaf_node() {
        let out = edit_src("a 1\n", ".b = 2");
        assert_eq!(out, "a 1\nb 2\n");
        // A string value round-trips (KDL v2 may render `hi` as a bare ident).
        assert_eq!(
            q(&edit_src("a 1\n", ".title = \"hi\""), ".title"),
            vec![Value::Str("hi".into())]
        );
    }

    #[test]
    fn append_repeated_node() {
        // `.bind` is a two-element array of arrays; appending a third arg-list
        // adds a node. The new bind re-reads with the right args...
        let out = edit_src(SAMPLE, ".bind += [[\"Mod+j\", \"focus-down\"]]");
        assert_eq!(q(&out, ".bind | length"), vec![Value::Int(3)]);
        assert_eq!(q(&out, ".bind[2][0]"), vec![Value::Str("Mod+j".into())]);
        // ...and the existing binds are untouched byte-for-byte.
        assert!(out.contains("bind \"Mod+h\" \"focus-left\""));
        assert!(out.contains("bind \"Mod+l\" \"focus-right\""));
    }

    #[test]
    fn delete_node_property_and_occurrence() {
        // Delete a whole node.
        assert!(!edit_src(SAMPLE, "del(.layout)").contains("layout"));
        // Delete just a property.
        let out = edit_src(SAMPLE, "del(.layout.gaps)");
        assert!(!out.contains("gaps"));
        assert!(out.contains("layout \"tall\""));
        // Delete one occurrence of a repeated node.
        let out2 = edit_src(SAMPLE, "del(.bind[0])");
        assert!(out2.contains("bind \"Mod+l\""));
        assert!(!out2.contains("Mod+h"));
    }

    #[test]
    fn edit_edge_cases() {
        // Set an argument by index; delete a property; delete an argument.
        assert!(edit_src("node \"a\" \"b\"\n", ".node[0] = \"z\"").contains("node z \"b\""));
        assert!(!edit_src("node key=1 other=2\n", "del(.node.key)").contains("key="));
        assert_eq!(
            edit_src("node \"a\" \"b\"\n", "del(.node[0])"),
            "node \"b\"\n"
        );
        // Delete a whole property-bearing node, and a missing key is a no-op.
        assert_eq!(edit_src("a 1\nb 2\n", "del(.a)"), "b 2\n");
        assert_eq!(edit_src("a 1\n", "del(.nope)"), "a 1\n");
        // Create a node from an object with args (`-`) plus scalar entries. The
        // `Value` model can't distinguish a property from a single-arg child, so
        // the inverse emits scalar entries as child nodes (args stay args).
        let out = edit_src(
            "root {\n    x 1\n}\n",
            r#".root.child = {"-": "arg", "prop": true, "kid": 9}"#,
        );
        assert!(out.contains("child arg {"), "got: {out}");
        assert!(
            out.contains("prop #true") && out.contains("kid 9"),
            "got: {out}"
        );
    }

    #[test]
    fn rejects_unrepresentable_and_missing() {
        // A null value -> a bare node; a scalar arg that's a container errors.
        assert!(edit_src("a 1\n", ".b = null").contains("b"));
        let mut doc = parse("a 1\n").unwrap();
        // Setting an argument index to a container is refused.
        assert!(apply(&mut doc, &parse_expr(".a[0] = [1, 2]").unwrap()).is_err());
    }

    #[test]
    fn refuses_to_replace_a_node_body() {
        let mut doc = parse(SAMPLE).unwrap();
        let err = apply(&mut doc, &parse_expr(".layout = 1").unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("whole body"), "got: {err}");
    }

    #[test]
    fn emit_from_value_round_trips() {
        let value = parse(SAMPLE).unwrap().to_value();
        let (kdl, warnings) = emit(&value).unwrap();
        assert!(warnings.is_empty());
        // Re-reading the emitted KDL yields the same value.
        assert_eq!(parse(&kdl).unwrap().to_value(), value);
    }

    // --- comment model -----------------------------------------------------

    #[test]
    fn extracts_head_and_inline_comments() {
        let src = "// the layout\nlayout \"tall\" // pinned\nbind \"x\"\n";
        let doc = parse(src).unwrap();
        let c = doc.to_commented().unwrap();
        assert_eq!(c.to_value(), doc.to_value(), "shapes must match");
        let edikt_core::CommentedNode::Object(top) = &c.node else {
            panic!("expected object");
        };
        assert_eq!(top[0].0, "layout");
        assert_eq!(top[0].1.comments.head, vec!["the layout"]);
        assert_eq!(top[0].1.comments.inline.as_deref(), Some("pinned"));
    }

    #[test]
    fn commented_emit_places_comments() {
        let src = "// the layout\nlayout \"tall\"\nbind \"x\"\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let (out, warnings) = emit_commented(&c).unwrap();
        assert!(warnings.is_empty());
        assert!(out.contains("// the layout"), "got: {out}");
        // Re-reading keeps the comment.
        assert!(parse(&out).unwrap().has_comments());
    }

    #[test]
    fn roundtrips_every_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/kdl");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("fixtures/kdl directory") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("kdl") {
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
        assert!(count >= 2, "expected kdl fixtures, found {count}");
    }
}
