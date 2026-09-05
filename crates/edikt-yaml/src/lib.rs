//! edikt YAML format module: **lossless in-place edit, query, and conversion**,
//! pure Rust.
//!
//! YAML is driven by [`libyaml-safer`](https://crates.io/crates/libyaml-safer), a
//! safe pure-Rust port of the reference parser (zero transitive deps). One parse
//! pass feeds all three jobs: its event stream is composed into a **span tree**
//! (see [`compose`]) that carries both the data model (fold to [`Value`] for
//! query/convert) and every node's byte marks (the lossless splice for edit).
//!
//! The moat holds: an edit replaces exactly the targeted node's bytes; comments,
//! indentation, quote style, and layout of every untouched region survive
//! byte-for-byte. Restructuring a block in place (replacing a whole
//! mapping/sequence, or creating nested keys) is refused rather than reflowed -
//! edikt never rewrites what it didn't target.

mod comments;
mod compose;
mod edit;
mod emit;
mod scalar;

use compose::{Node, node_to_value};
// The edikt-core types that appear in this crate's own public API, re-exported
// so a dependent can call these methods without also taking a direct
// edikt-core dependency (jhheider/edikt#66). `parse` is aliased because this
// crate's own `parse` is the document parser.
pub use edikt_core::{
    CommentKind, Commented, Document, EditError, Expr, Feature, Step, Value, json,
    parse as parse_expr,
};

pub use comments::emit_commented;
pub use emit::emit;

/// Comment kinds this format supports (empty => none); the comment
/// capability, subsuming the boolean `Feature::Comments`.
pub const COMMENT_KINDS: &[CommentKind] =
    &[CommentKind::Head, CommentKind::Inline, CommentKind::Foot];

/// Capabilities of YAML: everything but sections.
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

/// A parsed YAML stream: the original source plus one span tree per document.
/// A single-document stream (the common case) has one entry in `docs`; a
/// `---`-separated stream has one per document.
pub struct Yaml {
    pub(crate) source: String,
    pub(crate) docs: Vec<Node>,
}

/// Parse YAML `src` into a [`Yaml`] stream.
pub fn parse(src: &str) -> Result<Yaml, ParseError> {
    let mut docs = compose::compose_all(src)
        .map_err(|msg| ParseError { msg })?
        .into_vec();
    // An empty or comment-only stream is one null document, so every caller has
    // at least one document to project or edit.
    if docs.is_empty() {
        docs.push(compose::null_node());
    }
    Ok(Yaml {
        source: src.to_string(),
        docs,
    })
}

impl Document for Yaml {
    fn to_source(&self) -> String {
        self.source.clone()
    }
    fn to_value(&self) -> Value {
        match self.docs.first() {
            Some(d) => node_to_value(d),
            None => Value::Null,
        }
    }
    fn to_values(&self) -> Vec<Value> {
        if self.docs.is_empty() {
            vec![Value::Null]
        } else {
            self.docs.iter().map(node_to_value).collect()
        }
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<Vec<String>, EditError> {
        edit::apply(self, expr)
    }
    fn has_comments(&self) -> bool {
        // A `#` at line start or after whitespace opens a comment. This can
        // false-positive on a `#` inside a quoted scalar, which only ever
        // over-warns on conversion; acceptable, and never wrong the other way.
        self.source
            .lines()
            .any(|l| l.trim_start().starts_with('#') || l.contains(" #"))
    }
    fn to_commented(&self) -> Option<edikt_core::Commented> {
        // The first document's comments (bulk enumeration and single-doc
        // callers). Multi-document comment queries use `to_commented_all`.
        let node = self.docs.first()?;
        Some(comments::to_commented(&self.source, node))
    }
    fn to_commented_all(&self) -> Vec<edikt_core::Commented> {
        self.docs
            .iter()
            .map(|node| comments::to_commented(&self.source, node))
            .collect()
    }
    fn set_comment(
        &mut self,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        // Set the comment at `path` in every document where the path resolves;
        // in a single-document stream a non-resolving path errors (strict), as
        // before. Each write recomposes, so the next document's marks stay
        // correct.
        let multi = self.docs.len() > 1;
        let mut warnings = Vec::new();
        for idx in 0..self.docs.len() {
            match comments::set_node_comment(&self.source, &self.docs[idx], path, kind, text) {
                Ok((source, warns)) => {
                    warnings.extend(warns);
                    *self = parse(&source).map_err(|e| EditError::new(e.msg))?;
                }
                Err(_) if multi => {} // path absent in this document: skip
                Err(e) => return Err(e),
            }
        }
        Ok(warnings)
    }
    fn delete_comment(
        &mut self,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        // Delete in every document; a missing path is already a per-document
        // no-op in `delete_node_comment`.
        for idx in 0..self.docs.len() {
            let source = comments::delete_node_comment(&self.source, &self.docs[idx], path, kind)?;
            *self = parse(&source).map_err(|e| EditError::new(e.msg))?;
        }
        Ok(())
    }
    fn set_comment_in_doc(
        &mut self,
        doc: usize,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        // Scope to one document (bulk transforms, where each comment's new text
        // comes from its own current text).
        let node = self
            .docs
            .get(doc)
            .ok_or_else(|| EditError::new("document index out of range"))?;
        let (source, warnings) = comments::set_node_comment(&self.source, node, path, kind, text)?;
        *self = parse(&source).map_err(|e| EditError::new(e.msg))?;
        Ok(warnings)
    }
    fn source_slice(&self, path: &[edikt_core::Step]) -> Vec<String> {
        // One document's slices after another, in stream order, so the result
        // aligns 1:1 with a per-document query (`to_values`).
        self.docs
            .iter()
            .flat_map(|node| edit::source_slices(&self.source, node, path))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edikt_core::parse as parse_expr;
    use edikt_core::{Document, eval};

    const SAMPLE: &str = "# services\nweb:\n  image: nginx:1.25   # pinned\n  ports:\n    - 80\n    - 443\n  replicas: 3\ndebug: false\n";

    fn q(src: &str, expr: &str) -> Vec<Value> {
        eval(&parse_expr(expr).unwrap(), &parse(src).unwrap().to_value()).unwrap()
    }

    /// Apply a mutation program to `src` and return the resulting source.
    fn edit(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        doc.apply(&parse_expr(expr).unwrap()).unwrap();
        doc.to_source()
    }

    fn cedit(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        edikt_core::apply_comment_mutation(&mut doc, &parse_expr(expr).unwrap()).unwrap();
        doc.to_source()
    }

    /// Query mapped over every document, concatenated (as the CLI does).
    fn qall(src: &str, expr: &str) -> Vec<Value> {
        let e = parse_expr(expr).unwrap();
        parse(src)
            .unwrap()
            .to_values()
            .iter()
            .flat_map(|v| eval(&e, v).unwrap())
            .collect()
    }

    const STREAM: &str =
        "---\nkind: Deployment\nspec:\n  replicas: 2\n---\nkind: Service\nspec:\n  port: 80\n";

    #[test]
    fn multidoc_round_trip_is_identity() {
        // A no-op edit (delete a missing key) leaves every document byte-identical.
        assert_eq!(edit(STREAM, "del(.nope)"), STREAM);
        // And a stream with comments/framing survives too.
        let s = "# head\n---\nkind: A   # x\nn: 1\n---\nkind: B\n...\n";
        assert_eq!(edit(s, "del(.nope)"), s);
    }

    #[test]
    fn multidoc_query_yields_one_result_per_document() {
        assert_eq!(
            qall(STREAM, ".kind"),
            vec![
                Value::Str("Deployment".into()),
                Value::Str("Service".into())
            ]
        );
    }

    #[test]
    fn multidoc_edit_maps_over_all_documents() {
        // `.spec` exists in both, so the key is set/created in both (the brief's
        // "label them all" semantics).
        let out = edit(STREAM, ".spec.replicas = 5");
        assert_eq!(
            out,
            "---\nkind: Deployment\nspec:\n  replicas: 5\n---\nkind: Service\nspec:\n  port: 80\n  replicas: 5\n"
        );
    }

    #[test]
    fn multidoc_edit_skips_documents_without_the_parent() {
        // Doc B has no `.meta`, so it is a no-op there; doc A is edited.
        let s = "---\nkind: A\nmeta:\n  name: foo\n---\nkind: B\n";
        assert_eq!(
            edit(s, r#".meta.name = "bar""#),
            "---\nkind: A\nmeta:\n  name: bar\n---\nkind: B\n"
        );
    }

    #[test]
    fn multidoc_select_targets_by_content() {
        // Only the Service document is edited.
        let out = edit(STREAM, r#"select(.kind == "Service") | .spec.port = 443"#);
        assert_eq!(
            out,
            "---\nkind: Deployment\nspec:\n  replicas: 2\n---\nkind: Service\nspec:\n  port: 443\n"
        );
        // A non-matching predicate touches nothing.
        assert_eq!(
            edit(STREAM, r#"select(.kind == "Ingress") | .x = 1"#),
            STREAM
        );
    }

    #[test]
    fn multidoc_del_maps_over_documents() {
        // `kind` exists in both; del removes it from each.
        let out = edit(STREAM, "del(.kind)");
        assert_eq!(out, "---\nspec:\n  replicas: 2\n---\nspec:\n  port: 80\n");
    }

    #[test]
    fn anchors_do_not_cross_document_boundaries() {
        // `*x` in doc 2 references an anchor defined only in doc 1. Anchor scope
        // is per-document, so it does not resolve to 1; it composes to null
        // (each document gets a fresh anchor scope). If scopes leaked, `.b`
        // would be 1.
        assert_eq!(qall("---\na: &x 1\n---\nb: *x\n", ".b"), vec![Value::Null]);
        // Within a document, aliases still resolve normally.
        let s = "---\nx: &a 1\ny: *a\n---\nz: &b two\nw: *b\n";
        assert_eq!(qall(s, ".y"), vec![Value::Int(1)]);
        assert_eq!(qall(s, ".w"), vec![Value::Str("two".into())]);
    }

    #[test]
    fn single_document_behavior_is_unchanged() {
        // No `---`: strict single-doc semantics; a missing path still errors.
        let mut doc = parse("a: 1\n").unwrap();
        assert!(
            doc.apply(&parse_expr(".missing.deep = 2").unwrap())
                .is_err()
        );
    }

    #[test]
    fn multidoc_positional_select_edits_one_document() {
        // `^d1` edits only the second document.
        assert_eq!(
            edit(STREAM, "^d1 | .spec.port = 9090"),
            "---\nkind: Deployment\nspec:\n  replicas: 2\n---\nkind: Service\nspec:\n  port: 9090\n"
        );
        // `^d0` edits only the first.
        assert_eq!(
            edit(STREAM, "^d0.spec.replicas = 9"),
            "---\nkind: Deployment\nspec:\n  replicas: 9\n---\nkind: Service\nspec:\n  port: 80\n"
        );
    }

    #[test]
    fn multidoc_positional_select_is_strict() {
        // A named document is strict: a missing path errors (you asked for that
        // document specifically), unlike the lenient map-over-all default.
        let mut doc = parse(STREAM).unwrap();
        assert!(
            doc.apply(&parse_expr("^d1 | .no.such = 1").unwrap())
                .is_err()
        );
    }

    #[test]
    fn multidoc_positional_out_of_range_errors() {
        let mut doc = parse(STREAM).unwrap();
        let e = doc
            .apply(&parse_expr("^d5 | .x = 1").unwrap())
            .unwrap_err()
            .to_string();
        assert!(e.contains("out of range"), "{e}");
    }

    #[test]
    fn multidoc_update_add_assign_noop_on_missing_path() {
        // `|=` and `+=` are no-ops on documents lacking the path (lenient), not
        // errors, across a multi-document stream.
        let s = "---\nreplicas: 2\n---\nkind: Service\n";
        assert_eq!(
            edit(s, ".replicas |= . + 1"),
            "---\nreplicas: 3\n---\nkind: Service\n"
        );
        let s2 = "---\nxs:\n  - 1\n---\nkind: Service\n";
        assert_eq!(
            edit(s2, ".xs += [2]"),
            "---\nxs:\n  - 1\n  - 2\n---\nkind: Service\n"
        );
    }

    #[test]
    fn multidoc_select_chained_edits() {
        // A left-associated pipe chain after `select` applies every stage to the
        // matching documents only.
        let s =
            "---\nkind: Service\nspec:\n  a: 1\n  b: 1\n---\nkind: Pod\nspec:\n  a: 1\n  b: 1\n";
        let out = edit(
            s,
            r#"select(.kind == "Service") | .spec.a = 9 | .spec.b = 8"#,
        );
        assert_eq!(
            out,
            "---\nkind: Service\nspec:\n  a: 9\n  b: 8\n---\nkind: Pod\nspec:\n  a: 1\n  b: 1\n"
        );
    }

    #[test]
    fn multidoc_update_and_add_assign_map_over_docs() {
        // `|=` bumps replicas in the doc that has it; `+=` appends to the
        // sequence in the doc that has it. Both no-op where absent.
        let s = "---\nreplicas: 2\n---\nreplicas: 5\n";
        assert_eq!(
            edit(s, ".replicas |= . + 1"),
            "---\nreplicas: 3\n---\nreplicas: 6\n"
        );
        let s2 = "---\nxs:\n  - 1\n---\nys:\n  - 9\n";
        assert_eq!(
            edit(s2, ".xs += [2]"),
            "---\nxs:\n  - 1\n  - 2\n---\nys:\n  - 9\n"
        );
    }

    #[test]
    fn comment_mutation_block_yaml() {
        // Head above a mapping entry.
        assert_eq!(
            cedit(
                "web:\n  image: nginx\n  replicas: 3\n",
                ".web.replicas.# = \"scale\""
            ),
            "web:\n  image: nginx\n  # scale\n  replicas: 3\n"
        );
        // Inline on a scalar value.
        assert_eq!(
            cedit("web:\n  image: nginx\n", ".web.image.#.inline = \"pinned\""),
            "web:\n  image: nginx  # pinned\n"
        );
        // Inline on a container key (the block mapping).
        assert_eq!(
            cedit("web:\n  image: nginx\n", ".web.#.inline = \"svc\""),
            "web:  # svc\n  image: nginx\n"
        );
        // Head above a block-sequence item, at the dash's indent.
        assert_eq!(
            cedit("ports:\n  - 80\n  - 443\n", ".ports[1].# = \"https\""),
            "ports:\n  - 80\n  # https\n  - 443\n"
        );
        // Editing a sibling leaves an existing inline comment untouched.
        assert_eq!(
            cedit(
                "# stack\nweb:\n  image: nginx   # keep\n  replicas: 3\n",
                ".web.replicas.# = \"count\""
            ),
            "# stack\nweb:\n  image: nginx   # keep\n  # count\n  replicas: 3\n"
        );
        // Delete, and the result re-parses byte-for-byte to the original.
        assert_eq!(cedit("a: 1\n# note\nb: 2\n", "del(.b.#)"), "a: 1\nb: 2\n");
    }

    #[test]
    fn comment_on_flow_collection_defers_to_reflow() {
        let mut doc = parse("flags: [ssl, verify]\n").unwrap();
        let err = edikt_core::apply_comment_mutation(
            &mut doc,
            &parse_expr(".flags[0].# = \"x\"").unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("block-style expansion"), "got: {err}");
    }

    #[test]
    fn round_trips_byte_identical() {
        assert_eq!(parse(SAMPLE).unwrap().to_source(), SAMPLE);
    }

    #[test]
    fn queries_typed_values() {
        assert_eq!(
            q(SAMPLE, ".web.image"),
            vec![Value::Str("nginx:1.25".into())]
        );
        assert_eq!(q(SAMPLE, ".web.replicas"), vec![Value::Int(3)]);
        assert_eq!(q(SAMPLE, ".debug"), vec![Value::Bool(false)]);
        assert_eq!(
            q(SAMPLE, ".web.ports[]"),
            vec![Value::Int(80), Value::Int(443)]
        );
        assert_eq!(q(SAMPLE, ".web.ports | length"), vec![Value::Int(2)]);
        assert_eq!(q(SAMPLE, ".web.ports[-1]"), vec![Value::Int(443)]);
    }

    #[test]
    fn set_scalar_touches_only_that_value() {
        // Change one scalar; every other byte (comments, indent, the pinned
        // comment on the same line) stays put.
        let out = edit(SAMPLE, ".web.replicas = 5");
        assert_eq!(
            out,
            "# services\nweb:\n  image: nginx:1.25   # pinned\n  ports:\n    - 80\n    - 443\n  replicas: 5\ndebug: false\n"
        );
    }

    #[test]
    fn set_preserves_inline_comment() {
        let out = edit(SAMPLE, ".web.image = \"nginx:1.27\"");
        // The `# pinned` inline comment and its spacing survive.
        assert!(out.contains("image: nginx:1.27   # pinned"));
    }

    #[test]
    fn set_string_that_looks_numeric_is_quoted() {
        let out = edit(SAMPLE, ".web.replicas = \"3\"");
        assert!(out.contains("replicas: \"3\""));
        // And it re-reads as a string, not an int.
        assert_eq!(q(&out, ".web.replicas"), vec![Value::Str("3".into())]);
    }

    #[test]
    fn update_assign_sees_current() {
        let out = edit(SAMPLE, ".web.replicas |= . + 1");
        assert!(out.contains("replicas: 4"));
    }

    #[test]
    fn append_to_block_sequence() {
        let out = edit(SAMPLE, ".web.ports += [8080]");
        assert_eq!(
            out,
            "# services\nweb:\n  image: nginx:1.25   # pinned\n  ports:\n    - 80\n    - 443\n    - 8080\n  replicas: 3\ndebug: false\n"
        );
    }

    #[test]
    fn delete_mapping_entry() {
        let out = edit(SAMPLE, "del(.debug)");
        assert_eq!(
            out,
            "# services\nweb:\n  image: nginx:1.25   # pinned\n  ports:\n    - 80\n    - 443\n  replicas: 3\n"
        );
    }

    #[test]
    fn delete_nested_entry_keeps_siblings() {
        let out = edit(SAMPLE, "del(.web.replicas)");
        assert_eq!(
            out,
            "# services\nweb:\n  image: nginx:1.25   # pinned\n  ports:\n    - 80\n    - 443\ndebug: false\n"
        );
    }

    #[test]
    fn delete_sequence_item() {
        let out = edit(SAMPLE, "del(.web.ports[0])");
        assert_eq!(
            out,
            "# services\nweb:\n  image: nginx:1.25   # pinned\n  ports:\n    - 443\n  replicas: 3\ndebug: false\n"
        );
    }

    #[test]
    fn new_leaf_key_matches_indent() {
        let out = edit(SAMPLE, ".web.user = \"nginx\"");
        assert!(out.contains("\n  user: nginx\n"));
        // Inserted inside `web`, before `debug`.
        assert!(out.contains("  replicas: 3\n  user: nginx\ndebug: false\n"));
    }

    #[test]
    fn new_root_key_at_column_zero() {
        let out = edit(SAMPLE, ".name = \"stack\"");
        assert!(out.ends_with("debug: false\nname: stack\n"));
    }

    #[test]
    fn edit_edge_cases() {
        // Update-assign and add-assign compute over the current value.
        assert!(edit("n: 10\n", ".n |= . / 2").contains("n: 5"));
        assert!(edit("n: 1\n", ".n += 4").contains("n: 5"));
        // Piped edits both land.
        assert_eq!(edit("a: 1\nb: 2\n", ".a = 9 | .b = 8"), "a: 9\nb: 8\n");
        // Delete a missing key is a no-op; delete a nested key keeps siblings.
        assert_eq!(edit("a: 1\n", "del(.nope)"), "a: 1\n");
        assert_eq!(edit("m:\n  x: 1\n  y: 2\n", "del(.m.x)"), "m:\n  y: 2\n");
        // Index into a sequence to set an element.
        assert_eq!(
            edit("xs:\n  - 1\n  - 2\n", ".xs[1] = 9"),
            "xs:\n  - 1\n  - 9\n"
        );
        // A negative index counts from the end.
        assert_eq!(
            edit("xs:\n  - 1\n  - 2\n", ".xs[-1] = 9"),
            "xs:\n  - 1\n  - 9\n"
        );
    }

    #[test]
    fn edit_errors_are_clean() {
        // Creating through an array index is refused, not a panic.
        let mut doc = parse("a: 1\n").unwrap();
        assert!(doc.apply(&parse_expr(".a[0] = 1").unwrap()).is_err());
        // Add-assign on a missing key errors.
        let mut doc2 = parse("a: 1\n").unwrap();
        assert!(doc2.apply(&parse_expr(".nope += 1").unwrap()).is_err());
    }

    #[test]
    fn iterate_assignment_maps_over_elements() {
        // `.a[] |= f` maps over elements; `.a[] += x` is per-element `. + x`;
        // `.a[] = x` sets every element. Comments beside elements survive.
        assert_eq!(
            edit("a:\n  - 1\n  - 2\n", ".a[] |= . * 2"),
            "a:\n  - 2\n  - 4\n"
        );
        assert_eq!(
            edit("a:\n  - 1  # one\n  - 2\n", ".a[] += 10"),
            "a:\n  - 11  # one\n  - 12\n"
        );
        assert_eq!(edit("a:\n  - 1\n  - 2\n", ".a[] = 9"), "a:\n  - 9\n  - 9\n");
        // Object iterate fans out over a mapping's values.
        assert_eq!(
            edit("o:\n  x: 1\n  y: 2\n", ".o[] += 5"),
            "o:\n  x: 6\n  y: 7\n"
        );
        // Empty iterate is a no-op for update forms.
        assert_eq!(edit("xs: []\n", ".xs[] |= . + 1"), "xs: []\n");
    }

    #[test]
    fn delete_missing_is_a_noop_like_the_other_formats() {
        // Regression: YAML `del` of a missing key / OOB index used to error;
        // jq semantics (and every other format) make it a silent no-op.
        assert_eq!(edit("a: 1\nb: 2\n", "del(.nope)"), "a: 1\nb: 2\n");
        assert_eq!(edit("xs:\n  - 1\n", "del(.xs[9])"), "xs:\n  - 1\n");
        assert_eq!(edit("a: 1\n", "del(.deep.miss)"), "a: 1\n");
    }

    #[test]
    fn delete_iterate_fans_out() {
        // `del(.a[])` empties a sequence or mapping to its inline empty form,
        // jq's `del(.[]) -> []`/`{}`; comments inside the emptied region go.
        assert_eq!(edit("a:\n  - 1\n  - 2\n", "del(.a[])"), "a: []\n");
        assert_eq!(edit("o:\n  x: 1\n  y: 2\n", "del(.o[])"), "o: {}\n");
        assert_eq!(
            edit("a:\n  - 1  # one\n  - 2  # two\n", "del(.a[])"),
            "a: []\n"
        );
        assert_eq!(edit("a: [1, 2]\n", "del(.a[])"), "a: []\n");
        assert_eq!(edit("- 1\n- 2\n", "del(.[])"), "[]\n");
        // A nested iterate composes the per-item deletes (YAML block semantics:
        // deleting a subentry removes the item line).
        assert_eq!(edit("a:\n  - b: 1\n  - b: 2\n", "del(.a[].b)"), "a:\n");
        // Missing target / empty collection is a no-op.
        assert_eq!(edit("a: []\n", "del(.a[])"), "a: []\n");
        assert_eq!(edit("a: 1\n", "del(.nope[])"), "a: 1\n");
        // Multi-document: a `[]` fan-out maps over every selected document.
        assert_eq!(
            edit("a:\n  - 1\n  - 2\n---\na:\n  - 3\n", "del(.a[])"),
            "a: []\n---\na: []\n"
        );
    }

    #[test]
    fn refuses_to_replace_a_mapping() {
        let mut doc = parse(SAMPLE).unwrap();
        let err = doc
            .apply(&parse_expr(".web = 1").unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("mapping or sequence"), "got: {err}");
    }

    #[test]
    fn resolves_anchors_aliases_and_merge_keys() {
        let src = "base: &b\n  timeout: 30\n  retries: 3\nprod:\n  <<: *b\n  retries: 5\n";
        // The anchored mapping is directly queryable.
        assert_eq!(q(src, ".base.timeout"), vec![Value::Int(30)]);
        // A merge key (`<<: *b`) pulls the anchor's keys into `prod`...
        assert_eq!(q(src, ".prod.timeout"), vec![Value::Int(30)]);
        // ...but an explicit key wins over the merged one.
        assert_eq!(q(src, ".prod.retries"), vec![Value::Int(5)]);
    }

    #[test]
    fn anchors_fixture_round_trips_and_edits_surgically() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/yaml");
        let src = std::fs::read_to_string(dir.join("anchors.yaml")).unwrap();
        // Byte-identical round-trip over anchors, flow, folded scalars, comments.
        assert_eq!(parse(&src).unwrap().to_source(), src);
        // Merge-key resolution: development inherits `adapter` from &defaults.
        assert_eq!(
            q(&src, ".development.adapter"),
            vec![Value::Str("postgres".into())]
        );
        // Flow collections query.
        assert_eq!(
            q(&src, ".production.flags[0]"),
            vec![Value::Str("ssl".into())]
        );
        assert_eq!(
            q(&src, ".production.meta.owner"),
            vec![Value::Str("ops".into())]
        );
        // A surgical edit changes exactly one line.
        let out = edit(&src, ".production.pool = 50");
        let diff: Vec<_> = src
            .lines()
            .zip(out.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(diff, vec![("  pool: 25", "  pool: 50")]);
    }

    #[test]
    fn crlf_edits_preserve_line_endings() {
        // set: only the scalar bytes change; CRLF endings untouched.
        let src = "a: 1\r\nb: 2\r\n";
        assert_eq!(edit(src, ".a = 5"), "a: 5\r\nb: 2\r\n");
        // new key: the inserted line uses CRLF too, not a lone \n.
        assert_eq!(edit(src, ".c = 9"), "a: 1\r\nb: 2\r\nc: 9\r\n");
        // append: same.
        let seq = "xs:\r\n  - 1\r\n  - 2\r\n";
        assert_eq!(
            edit(seq, ".xs += [3]"),
            "xs:\r\n  - 1\r\n  - 2\r\n  - 3\r\n"
        );
    }

    #[test]
    fn append_after_block_scalar_item() {
        // The last item is a block literal; its span starts at the content, not
        // the dash. Appending must still work (derive the dash from the seq mark).
        let src = "items:\n  - |\n    literal\n  - plain\n";
        let out = edit(src, ".items += [\"x\"]");
        assert_eq!(out, "items:\n  - |\n    literal\n  - plain\n  - x\n");
    }

    #[test]
    fn block_scalar_set_refused_clearly_del_works() {
        let src = "x: |\n  line1\n  line2\nother: 1\n";
        // Setting a block scalar in place is refused with a clear message (not an
        // opaque re-parse error), and the document is left untouched.
        let mut doc = parse(src).unwrap();
        let err = doc
            .apply(&parse_expr(".x = \"new\"").unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("multi-line"), "got: {err}");
        assert_eq!(doc.to_source(), src);
        // Deleting a block scalar entry works.
        assert_eq!(edit(src, "del(.x)"), "other: 1\n");
    }

    #[test]
    fn unrelated_edit_preserves_scalar_spelling() {
        // `True` is core-schema bool, but editing a *different* key must not
        // re-spell it; to_source returns spliced bytes, not a re-emit.
        let src = "e: True\no: 1\n";
        let out = edit(src, ".o = 2");
        assert_eq!(out, "e: True\no: 2\n");
    }

    #[test]
    fn deleting_shadow_key_reexposes_merged_value() {
        // prod.b is an explicit override of base.b (merged via `<<`). Deleting the
        // physical override lets the merged value show through again.
        let src = "base: &b\n  a: 1\n  b: 2\nprod:\n  <<: *b\n  b: 3\n";
        assert_eq!(q(src, ".prod.b"), vec![Value::Int(3)]);
        let out = edit(src, "del(.prod.b)");
        assert_eq!(out, "base: &b\n  a: 1\n  b: 2\nprod:\n  <<: *b\n");
        // With the override gone, the merged value (2) is what remains.
        assert_eq!(q(&out, ".prod.b"), vec![Value::Int(2)]);
    }

    #[test]
    fn setting_merged_key_creates_physical_override() {
        // `.prod.a` exists only via the merge; setting it inserts a real key.
        let src = "base: &b\n  a: 1\nprod:\n  <<: *b\n";
        let out = edit(src, ".prod.a = 9");
        assert_eq!(out, "base: &b\n  a: 1\nprod:\n  <<: *b\n  a: 9\n");
        assert_eq!(q(&out, ".prod.a"), vec![Value::Int(9)]);
    }

    #[test]
    fn source_slice_block_dedents_and_flow_is_verbatim() {
        let doc = parse(SAMPLE).unwrap();
        let slice = |p: &str| doc.source_slice(parse_expr(p).unwrap().as_path().unwrap());
        // A block mapping is returned dedented to the margin (valid standalone
        // YAML), with its inline comment intact.
        assert_eq!(
            slice(".web"),
            vec!["image: nginx:1.25   # pinned\nports:\n  - 80\n  - 443\nreplicas: 3"]
        );
        // A block sequence, dedented.
        assert_eq!(slice(".web.ports"), vec!["- 80\n- 443"]);
        // A scalar is its exact bytes.
        assert_eq!(slice(".web.image"), vec!["nginx:1.25"]);
        // Iterate yields one slice per element.
        assert_eq!(slice(".web.ports[]"), vec!["80", "443"]);
    }

    #[test]
    fn source_slice_flow_collection_is_verbatim() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/yaml");
        let src = std::fs::read_to_string(dir.join("anchors.yaml")).unwrap();
        let doc = parse(&src).unwrap();
        let slice = |p: &str| doc.source_slice(parse_expr(p).unwrap().as_path().unwrap());
        // A flow sequence/mapping comes back verbatim (already self-contained).
        assert_eq!(slice(".production.flags"), vec!["[ssl, verify, fast]"]);
        assert_eq!(slice(".production.meta"), vec!["{ owner: ops, tier: 1 }"]);
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
        // `# services` is the banner above the first key.
        assert_eq!(top[0].0, "web");
        assert_eq!(top[0].1.comments.head, vec!["services"]);
        let edikt_core::CommentedNode::Object(web) = &top[0].1.node else {
            panic!("expected mapping");
        };
        // `image: nginx:1.25   # pinned`
        assert_eq!(web[0].1.comments.inline.as_deref(), Some("pinned"));
    }

    #[test]
    fn extracts_container_entry_inline_and_item_comments() {
        let src = "web: # svc\n  # first port\n  ports:\n    - 80 # http\n    - 443\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let edikt_core::CommentedNode::Object(top) = &c.node else {
            panic!("expected object");
        };
        // Inline on a key whose value is a container.
        assert_eq!(top[0].1.comments.inline.as_deref(), Some("svc"));
        let edikt_core::CommentedNode::Object(web) = &top[0].1.node else {
            panic!("expected mapping");
        };
        assert_eq!(web[0].1.comments.head, vec!["first port"]);
        let edikt_core::CommentedNode::Array(ports) = &web[0].1.node else {
            panic!("expected sequence");
        };
        assert_eq!(ports[0].comments.inline.as_deref(), Some("http"));
    }

    #[test]
    fn extracts_trailing_foot_and_comment_only_doc() {
        let c = parse("a: 1\n# the end\n").unwrap().to_commented().unwrap();
        assert_eq!(c.comments.foot, vec!["the end"]);
        // A comment-only document keeps its comments (as the null's head).
        let c2 = parse("# just a comment\n").unwrap().to_commented().unwrap();
        assert_eq!(c2.comments.head, vec!["just a comment"]);
    }

    #[test]
    fn commented_emit_places_all_kinds() {
        let c = parse(SAMPLE).unwrap().to_commented().unwrap();
        let (out, warnings) = emit_commented(&c).unwrap();
        assert!(warnings.is_empty());
        // (Sequence items sit at their key's indent; libyaml's block style,
        // same as the plain emitter.)
        assert_eq!(
            out,
            "# services\nweb:\n  image: nginx:1.25 # pinned\n  ports:\n  - 80\n  - 443\n  replicas: 3\ndebug: false\n"
        );
        // The emitted YAML re-parses with the same comments and values.
        let again = parse(&out).unwrap().to_commented().unwrap();
        assert_eq!(again, c);
    }

    #[test]
    fn commented_emit_matches_plain_emit_without_comments() {
        let value = parse(SAMPLE).unwrap().to_value();
        let (plain, _) = emit(&value).unwrap();
        let (commented, _) = emit_commented(&edikt_core::Commented::from_value(&value)).unwrap();
        assert_eq!(plain, commented, "no comments -> byte-identical to today");
    }

    #[test]
    fn compose_fixture_comments_survive_extraction_and_emit() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/yaml");
        let src = std::fs::read_to_string(dir.join("compose.yaml")).unwrap();
        let doc = parse(&src).unwrap();
        let c = doc.to_commented().unwrap();
        assert_eq!(c.to_value(), doc.to_value(), "shapes must match");
        assert!(c.has_comments());
        let (out, _) = emit_commented(&c).unwrap();
        // The emitted YAML re-reads to the same value and keeps the comments.
        assert_eq!(parse(&out).unwrap().to_value(), doc.to_value());
        assert!(parse(&out).unwrap().to_commented().unwrap().has_comments());
    }

    #[test]
    fn emits_yaml_round_trippable_through_value() {
        let value = parse(SAMPLE).unwrap().to_value();
        let (yaml, warnings) = emit(&value).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(parse(&yaml).unwrap().to_value(), value);
    }

    #[test]
    fn has_comments_detected() {
        assert!(parse(SAMPLE).unwrap().has_comments());
        assert!(!parse("a: 1\n").unwrap().has_comments());
    }

    #[test]
    fn empty_document_is_null() {
        assert_eq!(parse("").unwrap().to_value(), Value::Null);
        assert_eq!(parse("# just a comment\n").unwrap().to_value(), Value::Null);
    }

    #[test]
    fn queries_compose_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/yaml");
        let src = std::fs::read_to_string(dir.join("compose.yaml")).unwrap();
        // Fixture round-trips byte-identically.
        assert_eq!(parse(&src).unwrap().to_source(), src);
        assert_eq!(
            q(&src, ".services.web.image"),
            vec![Value::Str("nginx:1.25".into())]
        );
        assert_eq!(
            q(&src, ".services | keys"),
            vec![Value::Array(vec![
                Value::Str("db".into()),
                Value::Str("web".into()),
            ])]
        );
    }

    // --- emit: null / float / nested collections ---------------------------

    #[test]
    fn emit_covers_null_float_and_nested_collections() {
        let value = Value::Object(vec![
            ("nothing".into(), Value::Null),
            ("ratio".into(), Value::Float(1.5)),
            (
                "list".into(),
                Value::Array(vec![Value::Null, Value::Int(1)]),
            ),
        ]);
        let (yaml, warnings) = emit(&value).unwrap();
        assert!(warnings.is_empty());
        // Round-trips through the value model.
        assert_eq!(parse(&yaml).unwrap().to_value(), value);
        // Null emits plain; a float keeps its decimal point.
        assert!(yaml.contains("nothing: null"), "got: {yaml}");
        assert!(yaml.contains("ratio: 1.5"), "got: {yaml}");
    }

    // --- edit: error and edge paths ----------------------------------------

    #[test]
    fn edit_error_paths_are_clean() {
        let e = |src: &str, expr: &str| {
            let mut d = parse(src).unwrap();
            d.apply(&parse_expr(expr).unwrap()).unwrap_err().to_string()
        };
        // A bare query isn't a mutation.
        assert!(e("a: 1\n", ".a").contains("expected an assignment"));
        // `del` with the wrong arity, and `del` of the whole document.
        assert!(e("a: 1\n", "del(.a; .b)").contains("one path"));
        assert!(e("a: 1\n", "del(.)").contains("whole document"));
        // Set through a missing intermediate key (Field on a mapping, no key),
        // a Field into a non-mapping, and an out-of-range index all report
        // "path not found". Creating through an iterate is refused outright; a
        // comment step is not a value path.
        assert!(e("a: 1\n", ".missing.child = 1").contains("path not found"));
        assert!(e("xs:\n  - 1\n", ".xs.name = 1").contains("path not found"));
        assert!(e("xs:\n  - 1\n", ".xs[5] = 9").contains("path not found"));
        assert!(e("a: 1\n", ".xs[] = 1").contains("cannot create through `[]`"));
        assert!(e("a: 1\n", ".a.# = \"x\"").contains("path not found"));
        // A new key on an empty (flow) mapping can't match an entry's indent.
        assert!(e("foo: {}\n", ".foo.bar = 1").contains("empty or flow"));
        // Append onto empty / flow sequences is refused cleanly.
        assert!(e("xs: []\n", ".xs += [1]").contains("empty sequence"));
        assert!(e("xs: [1, 2]\n", ".xs += [3]").contains("flow sequence"));
    }

    #[test]
    fn append_to_aliased_sequence_is_refused() {
        // An alias resolves to a `Value::Array` held in a *scalar* node, so `+=`
        // can't splice into it as a block sequence; refuse rather than reflow.
        let src = "base: &b\n  - 1\n  - 2\nprod: *b\n";
        let mut d = parse(src).unwrap();
        let err = d
            .apply(&parse_expr(".prod += [3]").unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs a sequence"), "got: {err}");
    }

    #[test]
    fn edits_without_trailing_newline() {
        // New key: a fresh line is started before the added entry; the file's
        // lack of a trailing newline is preserved.
        assert_eq!(edit("a: 1", ".b = 2"), "a: 1\nb: 2");
        // Append: the inserted item leads with a newline and drops the trailer,
        // so the no-trailing-newline shape survives.
        assert_eq!(edit("xs:\n  - 1", ".xs += [2]"), "xs:\n  - 1\n  - 2");
    }

    #[test]
    fn delete_empty_collections_and_mismatched_paths() {
        // Deleting an entry whose value is an empty flow collection (block_end
        // over an empty sequence / mapping).
        assert_eq!(edit("xs: []\nother: 1\n", "del(.xs)"), "other: 1\n");
        assert_eq!(edit("foo: {}\nother: 1\n", "del(.foo)"), "other: 1\n");
        // A path shape that can't address a deletable node (field of a
        // sequence) is a silent no-op.
        assert_eq!(edit("xs:\n  - 1\n", "del(.xs.name)"), "xs:\n  - 1\n");
    }

    #[test]
    fn source_slice_index_iterate_and_comment_steps() {
        let doc = parse(SAMPLE).unwrap();
        let slice = |p: &str| doc.source_slice(parse_expr(p).unwrap().as_path().unwrap());
        // A specific (and negative) index selects one element's exact bytes.
        assert_eq!(slice(".web.ports[0]"), vec!["80"]);
        assert_eq!(slice(".web.ports[-1]"), vec!["443"]);
        // Iterating a mapping yields one slice per value (a scalar is its exact
        // bytes (the inline comment is trivia outside the scalar's span)).
        assert_eq!(slice(".web[]"), vec!["nginx:1.25", "- 80\n- 443", "3"]);
        // Iterating a scalar, or a trailing comment step, resolves nothing.
        assert_eq!(slice(".web.image[]"), Vec::<String>::new());
        assert_eq!(slice(".web.image.#"), Vec::<String>::new());
    }

    // --- comments: error paths, delete edges, extraction, emit -------------

    #[test]
    fn comment_edit_error_paths() {
        let cerr = |src: &str, expr: &str| {
            let mut d = parse(src).unwrap();
            edikt_core::apply_comment_mutation(&mut d, &parse_expr(expr).unwrap())
                .unwrap_err()
                .to_string()
        };
        // Document-level (`.#`) comment editing is a follow-up.
        assert!(cerr("a: 1\n", ".# = \"x\"").contains("document-level"));
        // A field comment whose container isn't a mapping.
        assert!(cerr("a: 1\n", ".a.b.# = \"x\"").contains("not a mapping key"));
        // An index comment whose container isn't a sequence.
        assert!(
            cerr("a:\n  x: 1\n", ".a[0].# = \"x\"").contains("not a sequence element"),
            "container-not-sequence"
        );
        // A sequence index out of range.
        assert!(cerr("ports:\n  - 80\n", ".ports[5].# = \"x\"").contains("out of range"));
        // A comment on an iterate step (neither a key nor an element).
        assert!(
            cerr("ports:\n  - 80\n", ".ports[].# = \"x\"")
                .contains("mapping keys or sequence elements")
        );
        // A comment path that doesn't resolve through an intermediate.
        assert!(cerr("a: 1\n", ".a.b.c.# = \"x\"").contains("does not resolve to a node"));
    }

    #[test]
    fn comment_delete_edge_paths() {
        // Deleting a comment on a missing path is a silent no-op.
        assert_eq!(cedit("a: 1\n", "del(.nope.#)"), "a: 1\n");
        // Deleting an inline comment keeps the value; result re-parses clean.
        assert_eq!(
            cedit(
                "web:\n  image: nginx  # pinned\n",
                "del(.web.image.#.inline)"
            ),
            "web:\n  image: nginx\n"
        );
        // Deleting a comment on a flow-collection element is a no-op (no own
        // line to drop a comment from).
        assert_eq!(
            cedit("flags: [ssl, verify]\n", "del(.flags[0].#)"),
            "flags: [ssl, verify]\n"
        );
    }

    #[test]
    fn comment_inline_set_preserves_crlf() {
        // Setting an inline comment on a CRLF line keeps the `\r` with the tail.
        assert_eq!(
            cedit("a: 1\r\nb: 2\r\n", ".a.#.inline = \"note\""),
            "a: 1  # note\r\nb: 2\r\n"
        );
    }

    #[test]
    fn comment_set_descends_through_sequence_index() {
        // The comment target's parent path runs through a sequence index.
        assert_eq!(
            cedit(
                "items:\n  - name: web\n    port: 80\n",
                ".items[0].name.# = \"note\""
            ),
            "items:\n  # note\n  - name: web\n    port: 80\n"
        );
    }

    #[test]
    fn scalar_root_comments_extract_and_emit() {
        // A bare-scalar document carries its head/inline on the scalar itself.
        let c = parse("# lead\n42  # yep\n")
            .unwrap()
            .to_commented()
            .unwrap();
        assert_eq!(c.comments.head, vec!["lead"]);
        assert_eq!(c.comments.inline.as_deref(), Some("yep"));
        assert_eq!(c.to_value(), Value::Int(42));
        // ...and they re-emit around the scalar.
        let (out, _) = emit_commented(&c).unwrap();
        assert_eq!(out, "# lead\n42 # yep\n");
    }

    #[test]
    fn document_foot_emits() {
        let c = parse("a: 1\n# the end\n").unwrap().to_commented().unwrap();
        assert_eq!(c.comments.foot, vec!["the end"]);
        let (out, _) = emit_commented(&c).unwrap();
        assert_eq!(out, "a: 1\n# the end\n");
    }

    #[test]
    fn commented_resolves_merge_keys() {
        let src = "base: &b\n  a: 1\n  b: 2\nprod:\n  <<: *b\n  b: 3\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        // The commented projection matches the value projection (`<<` resolved).
        assert_eq!(c.to_value(), parse(src).unwrap().to_value());
        let edikt_core::CommentedNode::Object(top) = &c.node else {
            panic!("expected object");
        };
        let (_, prod) = top.iter().find(|(k, _)| k == "prod").unwrap();
        let edikt_core::CommentedNode::Object(pentries) = &prod.node else {
            panic!("expected mapping");
        };
        // `a` is merged in (no physical `a` under prod); `b` is the override.
        assert!(pentries.iter().any(|(k, _)| k == "a"));
        assert_eq!(
            pentries
                .iter()
                .find(|(k, _)| k == "b")
                .map(|(_, v)| v.to_value()),
            Some(Value::Int(3))
        );
    }

    #[test]
    fn commented_emit_places_item_inline_and_head() {
        // A scalar item's inline stays on its line; an own-line comment above a
        // later item becomes that item's head.
        let src = "ports:\n  - 80 # http\n  # note\n  - 443\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let (out, _) = emit_commented(&c).unwrap();
        assert_eq!(out, "ports:\n- 80 # http\n# note\n- 443\n");
    }

    #[test]
    fn commented_emit_places_nested_foot_and_container_item_inline() {
        use edikt_core::{Commented, CommentedNode, Comments};
        // A foot comment on a mapping entry's value lands after that entry.
        let entry_with_foot = {
            let mut cc = Commented::scalar(Value::Int(1));
            cc.comments.foot = vec!["after a".into()];
            cc
        };
        let c = Commented {
            comments: Comments::default(),
            node: CommentedNode::Object(vec![
                ("a".into(), entry_with_foot),
                ("b".into(), Commented::scalar(Value::Int(2))),
            ]),
        };
        let (out, _) = emit_commented(&c).unwrap();
        assert_eq!(out, "a: 1\n# after a\nb: 2\n");

        // A foot on a scalar sequence item lands after that item.
        let item0 = {
            let mut cc = Commented::scalar(Value::Int(1));
            cc.comments.foot = vec!["mid".into()];
            cc
        };
        let seq = Commented {
            comments: Comments::default(),
            node: CommentedNode::Array(vec![item0, Commented::scalar(Value::Int(2))]),
        };
        let c2 = Commented {
            comments: Comments::default(),
            node: CommentedNode::Object(vec![("xs".into(), seq)]),
        };
        let (out2, _) = emit_commented(&c2).unwrap();
        assert_eq!(out2, "xs:\n- 1\n# mid\n- 2\n");

        // An inline on a *container* sequence item has no own line to trail, so
        // it joins the head above the item.
        let obj_item = {
            let mut cc = Commented {
                comments: Comments::default(),
                node: CommentedNode::Object(vec![(
                    "name".into(),
                    Commented::scalar(Value::Str("web".into())),
                )]),
            };
            cc.comments.inline = Some("primary".into());
            cc
        };
        let c3 = Commented {
            comments: Comments::default(),
            node: CommentedNode::Object(vec![(
                "servers".into(),
                Commented {
                    comments: Comments::default(),
                    node: CommentedNode::Array(vec![obj_item]),
                },
            )]),
        };
        let (out3, _) = emit_commented(&c3).unwrap();
        assert_eq!(out3, "servers:\n# primary\n- name: web\n");
    }

    #[test]
    fn commented_emit_places_container_key_inline() {
        // An inline comment on a key whose value opens on the next line hangs
        // after the key (the plan's non-same-line branch).
        let src = "web: # svc\n  image: nginx\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let (out, _) = emit_commented(&c).unwrap();
        assert_eq!(out, "web: # svc\n  image: nginx\n");
    }
}
