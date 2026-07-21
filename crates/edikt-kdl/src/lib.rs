//! edikt KDL format module.
//!
//! Backed by [`kdl`](https://crates.io/crates/kdl) (kdl-rs), whose document is
//! format-preserving by construction (the `toml_edit` of KDL), so edikt gets
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
    fn apply(&mut self, expr: &Expr) -> Result<Vec<String>, EditError> {
        edit::apply(self, expr).map(|()| Vec::new())
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
    use edikt_core::CommentedNode;
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

    /// The error string from applying a (mutating) expression through `apply`.
    fn edit_err(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        apply(&mut doc, &parse_expr(expr).unwrap())
            .unwrap_err()
            .to_string()
    }

    /// The error string from applying a comment mutation.
    fn cedit_err(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        edikt_core::apply_comment_mutation(&mut doc, &parse_expr(expr).unwrap())
            .unwrap_err()
            .to_string()
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

    // --- edit dispatch: pipe, del arity, non-mutation --------------------

    #[test]
    fn pipe_applies_both_mutations() {
        // Two edits piped land in order over the same document.
        assert_eq!(edit_src("a 1\nb 2\n", ".a = 10 | .b = 20"), "a 10\nb 20\n");
    }

    #[test]
    fn add_assign_propagates_a_computation_error() {
        // `+=` evaluates `current + addend`; an un-addable addend errors out.
        assert!(
            apply(
                &mut parse(SAMPLE).unwrap(),
                &parse_expr(".layout.gaps += [1]").unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn del_arity_and_non_path_and_non_mutation_errors() {
        // `del(...)` wants exactly one path argument.
        assert!(edit_err("a 1\n", "del(.a; .b)").contains("one path argument"));
        // A bare query expression isn't a mutation.
        assert!(edit_err("a 1\n", ".a").contains("expected an assignment"));
    }

    // --- set: path shape and missing-intermediate errors -----------------

    #[test]
    fn assignment_path_must_start_with_a_node_name() {
        assert!(edit_err("a 1\n", ".[0] = 1").contains("start with a node name"));
    }

    #[test]
    fn set_through_a_missing_node_errors() {
        assert!(edit_err("a 1\n", ".nope.child = 1").contains("no node `nope`"));
    }

    // --- set: repeated nodes ---------------------------------------------

    #[test]
    fn set_repeated_occurrence_whole_node_by_index() {
        // `.bind[0] = [...]` addresses one occurrence and replaces its args.
        let out = edit_src(SAMPLE, ".bind[0] = [\"Mod+x\", \"focus-x\"]");
        assert_eq!(
            q(&out, ".bind[0]"),
            vec![Value::Array(vec![
                Value::Str("Mod+x".into()),
                Value::Str("focus-x".into()),
            ])]
        );
        // The other occurrence is untouched.
        assert!(out.contains("bind \"Mod+l\" \"focus-right\""), "got: {out}");
    }

    #[test]
    fn set_repeated_with_a_field_step_is_ambiguous() {
        assert!(edit_err(SAMPLE, ".bind.foo = 1").contains("is repeated"));
    }

    // --- set: the `-` args key on a mixed node ----------------------------

    #[test]
    fn set_args_key_whole_and_by_index() {
        // Replace the whole argument row of a mixed node via `-`.
        let out = edit_src(SAMPLE, ".layout.[\"-\"] = \"wide\"");
        assert_eq!(q(&out, ".layout.[\"-\"]"), vec![Value::Str("wide".into())]);
        assert!(out.contains("gaps=8"), "props untouched: {out}");
        // ...and by index.
        let out2 = edit_src(SAMPLE, ".layout.[\"-\"][0] = \"narrow\"");
        assert_eq!(
            q(&out2, ".layout.[\"-\"]"),
            vec![Value::Str("narrow".into())]
        );
    }

    #[test]
    fn set_deeper_than_an_argument_scalar_errors() {
        assert!(edit_err(SAMPLE, ".layout.[\"-\"][0].x = 1").contains("arguments are scalars"));
    }

    // --- set: properties, bare nodes, arg-nodes ---------------------------

    #[test]
    fn set_through_a_scalar_property_errors() {
        assert!(edit_err(SAMPLE, ".layout.gaps.x = 1").contains("property `gaps` is a scalar"));
    }

    #[test]
    fn a_bare_node_grows_a_children_block() {
        // The autoformatter attaches the fresh children block directly (no space).
        assert_eq!(
            edit_src("flag\n", ".flag.child = 1"),
            "flag{\n    child 1\n}\n"
        );
    }

    #[test]
    fn an_argument_node_refuses_a_child() {
        assert!(
            edit_err("title \"hello\"\n", ".title.child = 1")
                .contains("holds arguments, not children")
        );
    }

    #[test]
    fn iterate_in_an_assignment_path_errors() {
        assert!(edit_err(SAMPLE, ".layout[] = 1").contains("`[]` in assignment paths"));
    }

    #[test]
    fn a_comment_step_through_plain_apply_errors() {
        // Comment edits route through `apply_comment_mutation`; the plain edit
        // path refuses a `#` step cleanly.
        assert!(edit_err(SAMPLE, ".layout.# = \"x\"").contains("editing comments"));
        assert!(edit_err(SAMPLE, "del(.layout.#)").contains("deleting comments"));
    }

    // --- set: replace_args prefix-append / rebuild ------------------------

    #[test]
    fn set_single_leaf_replaces_its_arguments() {
        // A lone leaf node updates its argument in place (byte-preserving shape).
        let out = edit_src("title \"hello\"\n", ".title = \"world\"");
        assert_eq!(q(&out, ".title"), vec![Value::Str("world".into())]);
    }

    #[test]
    fn set_arguments_appends_a_matching_prefix() {
        // New array starts with the current args -> the extra appends, the
        // existing `"a"` keeps its quoted bytes.
        let out = edit_src("node \"a\"\n", ".node = [\"a\", \"b\"]");
        assert!(out.contains("node \"a\" b"), "got: {out}");
    }

    #[test]
    fn set_arguments_rebuilds_on_a_different_shape() {
        // Shrinking the arg row (not a prefix) rebuilds it; props would survive.
        assert_eq!(
            edit_src("node \"a\" \"b\"\n", ".node = [\"x\"]"),
            "node x\n"
        );
    }

    // --- set: extend_repeated failure modes -------------------------------

    #[test]
    fn assigning_a_scalar_to_repeated_nodes_errors() {
        assert!(edit_err(SAMPLE, ".bind = 1").contains("is repeated"));
    }

    #[test]
    fn assigning_a_non_prefix_array_to_repeated_nodes_errors() {
        let err = edit_err(SAMPLE, ".bind = [[\"nomatch\"], [\"y\"]]");
        assert!(err.contains("wholesale is not supported"), "got: {err}");
    }

    // --- delete edge cases ------------------------------------------------

    #[test]
    fn delete_path_must_start_with_a_node_name() {
        assert!(edit_err("a 1\n", "del(.[0])").contains("start with a node name"));
    }

    #[test]
    fn delete_repeated_index_out_of_range_is_a_noop() {
        assert_eq!(edit_src(SAMPLE, "del(.bind[99])"), SAMPLE);
    }

    #[test]
    fn delete_an_argument_of_one_repeated_occurrence() {
        // `del(.bind[0][0])` removes the first arg of the first bind only.
        let out = edit_src(SAMPLE, "del(.bind[0][0])");
        assert_eq!(q(&out, ".bind[0]"), vec![Value::Str("focus-left".into())]);
        assert!(out.contains("bind \"Mod+l\" \"focus-right\""), "got: {out}");
    }

    #[test]
    fn delete_a_field_of_a_repeated_node_is_ambiguous() {
        assert!(edit_err(SAMPLE, "del(.bind.foo)").contains("is repeated"));
    }

    #[test]
    fn delete_args_key_whole_and_by_index() {
        // Clearing `-` drops the positional arguments, keeping props/children.
        let out = edit_src(SAMPLE, "del(.layout.[\"-\"])");
        assert!(!out.contains("\"tall\""), "arg gone: {out}");
        assert!(
            out.contains("gaps=8") && out.contains("border width=2"),
            "got: {out}"
        );
        // By index removes just that argument.
        let out2 = edit_src(SAMPLE, "del(.layout.[\"-\"][0])");
        assert!(!out2.contains("\"tall\""), "arg gone: {out2}");
    }

    #[test]
    fn delete_deeper_than_an_argument_scalar_errors() {
        assert!(edit_err(SAMPLE, "del(.layout.[\"-\"][0][0])").contains("arguments are scalars"));
    }

    #[test]
    fn delete_through_a_scalar_property_errors() {
        assert!(edit_err(SAMPLE, "del(.layout.gaps.x)").contains("property `gaps` is a scalar"));
    }

    #[test]
    fn delete_a_child_node_through_a_field() {
        let out = edit_src(SAMPLE, "del(.layout.border)");
        assert!(!out.contains("border"), "got: {out}");
        assert!(out.contains("layout \"tall\" gaps=8"), "parent kept: {out}");
    }

    #[test]
    fn delete_a_missing_field_of_a_childless_node_is_a_noop() {
        assert_eq!(
            edit_src("title \"x\"\n", "del(.title.nope)"),
            "title \"x\"\n"
        );
    }

    #[test]
    fn delete_iterate_is_unsupported() {
        assert!(edit_err(SAMPLE, "del(.layout[])").contains("del(.[]) is not supported"));
    }

    // --- build: value_to_nodes / emit -------------------------------------

    #[test]
    fn create_a_node_with_multiple_dash_arguments() {
        let out = edit_src(
            "root {\n    x 1\n}\n",
            ".root.node = {\"-\": [\"a\", \"b\"], \"k\": 9}",
        );
        assert!(out.contains("node a b {"), "got: {out}");
        assert!(out.contains("k 9"), "got: {out}");
    }

    #[test]
    fn emit_requires_a_top_level_object() {
        let err = emit(&Value::Int(1)).unwrap_err().to_string();
        assert!(err.contains("top-level object"), "got: {err}");
    }

    // --- project.rs: scalar kinds & mixed-arg projection ------------------

    #[test]
    fn projects_multiple_arguments_of_a_mixed_node() {
        assert_eq!(
            q("node \"a\" \"b\" x=1\n", ".node.[\"-\"]"),
            vec![Value::Array(vec![
                Value::Str("a".into()),
                Value::Str("b".into())
            ])]
        );
    }

    #[test]
    fn projects_every_scalar_kind() {
        // An integer wider than i64 degrades to a float, like JSON parsers.
        assert_eq!(
            q("big 9999999999999999999\n", ".big"),
            vec![Value::Float(1e19)]
        );
        assert_eq!(q("f 1.5\n", ".f"), vec![Value::Float(1.5)]);
        assert_eq!(q("flag #true\n", ".flag"), vec![Value::Bool(true)]);
        assert_eq!(q("n #null\n", ".n"), vec![Value::Null]);
    }

    #[test]
    fn emits_float_and_null_scalars() {
        assert_eq!(edit_src("x 0\n", ".x = 1.5"), "x 1.5\n");
        assert_eq!(edit_src("node \"a\"\n", ".node[0] = null"), "node #null\n");
    }

    // --- comments.rs: unsupported foot, inline delete, CRLF ---------------

    #[test]
    fn foot_comment_edits_are_unsupported() {
        assert!(cedit_err("n 1\n", ".n.#.foot = \"x\"").contains("foot comment isn't supported"));
        assert!(cedit_err("n 1\n", "del(.n.#.foot)").contains("foot comment isn't supported"));
    }

    #[test]
    fn deletes_an_inline_comment() {
        // The comment goes; the space that preceded it (the node's own decor,
        // not the terminator) stays.
        assert_eq!(cedit("n 1 // pinned\n", "del(.n.#.inline)"), "n 1 \n");
    }

    #[test]
    fn inline_comment_keeps_crlf_line_ending() {
        assert_eq!(
            cedit("n 1\r\nm 2\r\n", ".n.#.inline = \"hi\""),
            "n 1 // hi\r\nm 2\r\n"
        );
    }

    // --- comments.rs: comment-target resolution errors --------------------

    #[test]
    fn comment_target_resolution_errors() {
        // Document-level `.#` editing isn't wired for KDL yet.
        assert!(cedit_err("a 1\n", ".# = \"banner\"").contains("document-level"));
        // A missing node.
        assert!(cedit_err("a 1\n", ".nope.# = \"x\"").contains("no node `nope`"));
        // An out-of-range occurrence index.
        assert!(cedit_err(SAMPLE, ".bind[99].# = \"x\"").contains("index out of range"));
        // Descending into a node with no children.
        assert!(cedit_err("a 1\n", ".a.child.# = \"x\"").contains("has no child nodes"));
    }

    // --- comments.rs: extraction of foot / repeated / block ---------------

    #[test]
    fn extracts_foot_repeated_and_block_comments() {
        // A trailing document comment lands as the last entry's foot.
        let c = parse("a 1\n// foot\n").unwrap().to_commented().unwrap();
        let CommentedNode::Object(top) = &c.node else {
            panic!("expected object");
        };
        assert_eq!(top.last().unwrap().1.comments.foot, vec!["foot"]);

        // A repeated node projects to a comment-carrying array.
        let c2 = parse(SAMPLE).unwrap().to_commented().unwrap();
        let CommentedNode::Object(top2) = &c2.node else {
            panic!("expected object");
        };
        let bind = &top2.iter().find(|(k, _)| k == "bind").unwrap().1;
        assert!(
            matches!(bind.node, CommentedNode::Array(_)),
            "bind is an array"
        );

        // A `/* */` block comment extracts, delimiter-stripped.
        let c3 = parse("/* block */\nn 1\n").unwrap().to_commented().unwrap();
        let CommentedNode::Object(top3) = &c3.node else {
            panic!("expected object");
        };
        assert_eq!(top3[0].1.comments.head, vec!["block"]);
    }

    // --- comments.rs: commented emission ----------------------------------

    #[test]
    fn emit_commented_requires_a_top_level_object() {
        let c = edikt_core::Commented::from_value(&Value::Int(1));
        assert!(
            emit_commented(&c)
                .unwrap_err()
                .to_string()
                .contains("top-level object")
        );
    }

    #[test]
    fn emit_commented_places_foot_inline_and_nested_head() {
        // A document-level foot emits as trailing `//` lines after the nodes.
        let c = edikt_core::Commented {
            comments: edikt_core::Comments {
                foot: vec!["bye".into()],
                ..Default::default()
            },
            node: CommentedNode::Object(vec![(
                "a".into(),
                edikt_core::Commented::scalar(Value::Int(1)),
            )]),
        };
        assert_eq!(emit_commented(&c).unwrap().0, "a 1\n// bye\n");

        // Head comments on a run of repeated nodes emit above each. (Each bind
        // carries two args so the occurrences stay distinct nodes rather than
        // collapsing into one multi-arg node on re-emit.)
        let c2 = parse("// a\nbind \"x\" \"1\"\n// b\nbind \"y\" \"2\"\n")
            .unwrap()
            .to_commented()
            .unwrap();
        let out2 = emit_commented(&c2).unwrap().0;
        assert!(
            out2.contains("// a") && out2.contains("// b"),
            "got: {out2}"
        );

        // An inline comment emits after the node.
        let c3 = parse("layout \"tall\" // pinned\n")
            .unwrap()
            .to_commented()
            .unwrap();
        assert!(
            emit_commented(&c3).unwrap().0.contains("// pinned"),
            "inline"
        );

        // A head comment on a nested child recurses into the children block.
        // (Built directly here; extraction produces the same shape; see
        // `nested_child_comments_survive_extraction`.)
        let head = |text: &str, v: Value| edikt_core::Commented {
            comments: edikt_core::Comments {
                head: vec![text.into()],
                ..Default::default()
            },
            node: CommentedNode::Scalar(v),
        };
        let c4 = edikt_core::Commented {
            comments: edikt_core::Comments::default(),
            node: CommentedNode::Object(vec![(
                "parent".into(),
                edikt_core::Commented {
                    comments: edikt_core::Comments::default(),
                    node: CommentedNode::Object(vec![("child".into(), head("kid", Value::Int(1)))]),
                },
            )]),
        };
        assert_eq!(
            emit_commented(&c4).unwrap().0,
            "parent {\n    // kid\n    child 1\n}\n"
        );
    }

    #[test]
    fn nested_child_comments_survive_extraction() {
        // Regression: a comment on a *child* node now carries through
        // to_commented (node_commented recurses), so conversion keeps it.
        // Previously the child's decor was flattened away at extraction.
        let src = "parent {\n    // kid note\n    child 1 // trailing\n}\n";
        let c = parse(src).unwrap().to_commented().unwrap();

        let CommentedNode::Object(top) = &c.node else {
            panic!("expected object");
        };
        let parent = &top.iter().find(|(k, _)| k == "parent").unwrap().1;
        let CommentedNode::Object(kids) = &parent.node else {
            panic!("expected nested object");
        };
        let child = &kids.iter().find(|(k, _)| k == "child").unwrap().1;
        assert_eq!(child.comments.head, vec!["kid note"]);
        assert_eq!(child.comments.inline.as_deref(), Some("trailing"));

        // ...and round-trips through emission byte-for-byte (the emit path
        // autoformats scalars, so an integer child keeps this exact; comment
        // placement is what the fix guarantees).
        assert_eq!(emit_commented(&c).unwrap().0, src);
    }

    // --- lib.rs: whole-document guards & Document trait -------------------

    #[test]
    fn whole_document_set_and_delete_are_refused() {
        let mut doc = parse("a 1\n").unwrap();
        assert!(doc.set(&[], &Value::Int(1)).is_err());
        assert!(doc.delete(&[]).is_err());
    }

    #[test]
    fn document_trait_surface() {
        let mut doc = parse("a 1\n").unwrap();
        assert_eq!(doc.features(), FEATURES);
        assert!(!doc.has_comments());
        // The trait `apply` routes to `edit::apply`.
        Document::apply(&mut doc, &parse_expr(".a = 2").unwrap()).unwrap();
        assert_eq!(doc.to_source(), "a 2\n");
    }
}
