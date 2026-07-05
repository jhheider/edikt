//! edikt YAML format module — **lossless in-place edit, query, and conversion**,
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
//! mapping/sequence, or creating nested keys) is refused rather than reflowed —
//! edikt never rewrites what it didn't target.

mod comments;
mod compose;
mod edit;
mod emit;
mod scalar;

use compose::{Node, node_to_value};
use edikt_core::{CommentKind, Document, EditError, Expr, Feature, Value};

pub use comments::emit_commented;
pub use emit::emit;

/// Comment kinds this format supports (empty ⇒ none); the comment
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

/// A parsed YAML document: the original source plus its span tree.
pub struct Yaml {
    pub(crate) source: String,
    pub(crate) doc: Node,
}

/// Parse YAML `src` into a [`Yaml`] document.
pub fn parse(src: &str) -> Result<Yaml, ParseError> {
    let doc = compose::compose_source(src).map_err(|msg| ParseError { msg })?;
    Ok(Yaml {
        source: src.to_string(),
        doc,
    })
}

impl Document for Yaml {
    fn to_source(&self) -> String {
        self.source.clone()
    }
    fn to_value(&self) -> Value {
        node_to_value(&self.doc)
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<(), EditError> {
        edit::apply(self, expr)
    }
    fn has_comments(&self) -> bool {
        // A `#` at line start or after whitespace opens a comment. This can
        // false-positive on a `#` inside a quoted scalar, which only ever
        // over-warns on conversion — acceptable, and never wrong the other way.
        self.source
            .lines()
            .any(|l| l.trim_start().starts_with('#') || l.contains(" #"))
    }
    fn to_commented(&self) -> Option<edikt_core::Commented> {
        Some(comments::to_commented(&self.source, &self.doc))
    }
    fn set_comment(
        &mut self,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        let (source, warnings) =
            comments::set_node_comment(&self.source, &self.doc, path, kind, text)?;
        *self = parse(&source).map_err(|e| EditError::new(e.msg))?;
        Ok(warnings)
    }
    fn delete_comment(
        &mut self,
        path: &[edikt_core::Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        let source = comments::delete_node_comment(&self.source, &self.doc, path, kind)?;
        *self = parse(&source).map_err(|e| EditError::new(e.msg))?;
        Ok(())
    }
    fn source_slice(&self, path: &[edikt_core::Step]) -> Vec<String> {
        edit::source_slices(&self.source, &self.doc, path)
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
        // Change one scalar; every other byte — comments, indent, the pinned
        // comment on the same line — stays put.
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
        // The last item is a block literal — its span starts at the content, not
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
        // re-spell it — to_source returns spliced bytes, not a re-emit.
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
        // (Sequence items sit at their key's indent — libyaml's block style,
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
        assert_eq!(plain, commented, "no comments → byte-identical to today");
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
}
