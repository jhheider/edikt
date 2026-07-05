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

mod compose;
mod edit;
mod emit;
mod scalar;

use compose::{Node, node_to_value};
use edikt_core::{Document, EditError, Expr, Feature, Value};

pub use emit::emit;

/// Capabilities of YAML: everything but sections.
pub const FEATURES: &[Feature] = &[
    Feature::Comments,
    Feature::Nesting,
    Feature::Arrays,
    Feature::TypedScalars,
];

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
