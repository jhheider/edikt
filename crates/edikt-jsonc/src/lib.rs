//! edikt JSONC/JSON5/JSON format module.
//!
//! A lossless `rowan` + `logos` CST (the day-zero spike, productionized): parse
//! JSONC into a tree that round-trips byte-for-byte, project it to
//! [`edikt_core::Value`] for querying, and - with M2 - edit it in place touching
//! only the targeted nodes. `.json` is read by the same parser (it is a subset
//! with no comments to preserve).

mod comments;
mod edit;
mod lexer;
mod parser;
mod project;
mod syntax;

pub use comments::emit_commented;
pub use edikt_core::EditError;
pub use edit::apply;

use edikt_core::{CommentKind, Document, Expr, Feature, Step, Value};
use syntax::{Sk, SyntaxNode};

/// Comment kinds this format supports (empty => none); the comment
/// capability, subsuming the boolean `Feature::Comments`.
pub const COMMENT_KINDS: &[CommentKind] =
    &[CommentKind::Head, CommentKind::Inline, CommentKind::Foot];

/// Capabilities of the JSONC/JSON5 family.
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

/// A parsed JSONC document, backed by a lossless CST.
pub struct Jsonc {
    root: SyntaxNode,
}

impl Jsonc {
    /// Access the underlying syntax tree.
    pub fn syntax(&self) -> &SyntaxNode {
        &self.root
    }

    /// Set the value at `path` to `value`, format-preserving. If the path
    /// already resolves, only that value node's bytes change. If a trailing part
    /// of the path is missing, a new member is inserted into the deepest existing
    /// object (matching its indent/comma style), creating intermediate objects as
    /// needed. Creating through an array index or `[]` is not supported.
    pub fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        let top = self
            .root
            .children()
            .find(|n| n.kind() == Sk::Value)
            .ok_or_else(|| EditError::new("empty document"))?;
        let (container, remaining) = edit::walk_partial(top, path);
        let new_root = if remaining.is_empty() {
            container.replace_with(edit::value_green(value))
        } else {
            let Step::Field(key) = &remaining[0] else {
                return Err(EditError::new(
                    "can only create object keys, not array indices",
                ));
            };
            let object = container
                .children()
                .find(|n| n.kind() == Sk::Object)
                .ok_or_else(|| EditError::new("cannot create a key inside a non-object"))?;
            let member_value = edit::nest_value(&remaining[1..], value)?;
            let text = edit::insert_into_object(&object.text().to_string(), key, &member_value);
            object.replace_with(edit::object_green_from_text(&text))
        };
        self.root = SyntaxNode::new_root(new_root);
        Ok(())
    }

    /// The value at `path`, projected to the value model, or `None` if absent.
    pub fn value_at(&self, path: &[Step]) -> Option<Value> {
        edit::resolve_value_node(&self.root, path).map(|n| project::value_node(&n))
    }

    /// Append `items` to the array at `path`, format-preserving: existing
    /// elements and layout are untouched; new elements match the array's indent
    /// and comma style.
    pub fn append(&mut self, path: &[Step], items: &[Value]) -> Result<(), EditError> {
        let value_node = edit::resolve_value_node(&self.root, path)
            .ok_or_else(|| EditError::new("path not found"))?;
        let array = value_node
            .children()
            .find(|n| n.kind() == Sk::Array)
            .ok_or_else(|| EditError::new("`+= [..]` target is not an array"))?;
        let new_text = edit::insert_into_array(&array.text().to_string(), items);
        let new_root = array.replace_with(edit::array_green_from_text(&new_text));
        self.root = SyntaxNode::new_root(new_root);
        Ok(())
    }

    /// Delete the value at `path`, format-preserving: the member's or element's
    /// line is removed cleanly (no dangling comma or blank line). A missing key
    /// or out-of-range index is a no-op (jq semantics).
    pub fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        let Some((last, parent)) = path.split_last() else {
            return Err(EditError::new("del(.) is not allowed"));
        };
        let root = self.root.clone_for_update();
        let Some(container) = edit::resolve_value_node(&root, parent) else {
            return Ok(()); // parent path absent -> nothing to delete
        };
        match last {
            Step::Field(k) => {
                let member = container
                    .children()
                    .find(|n| n.kind() == Sk::Object)
                    .and_then(|object| edit::find_member(&object, k));
                if let Some(member) = member {
                    edit::delete_member(&member);
                    self.root = SyntaxNode::new_root(root.green().into_owned());
                }
                Ok(())
            }
            Step::Index(i) => {
                let value = container
                    .children()
                    .find(|n| n.kind() == Sk::Array)
                    .and_then(|array| {
                        let values: Vec<_> =
                            array.children().filter(|n| n.kind() == Sk::Value).collect();
                        let idx = if *i < 0 { values.len() as i64 + i } else { *i };
                        if idx < 0 {
                            None
                        } else {
                            values.into_iter().nth(idx as usize)
                        }
                    });
                if let Some(value) = value {
                    edit::delete_element(&value);
                    self.root = SyntaxNode::new_root(root.green().into_owned());
                }
                Ok(())
            }
            Step::Iterate => Err(EditError::new("del(.[]) is not supported yet")),
            Step::Comment(_) => Err(EditError::new(
                "deleting comments (`#`) is not supported yet (planned for v0.2)",
            )),
        }
    }
}

/// Parse JSONC source into a [`Jsonc`] document.
pub fn parse(src: &str) -> Result<Jsonc, ParseError> {
    let green = parser::build(src);
    let root = SyntaxNode::new_root(green);

    // An unrecognized byte is lexed as an error token - reject rather than
    // silently editing garbage.
    let has_error = root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == Sk::Error);
    if has_error {
        return Err(ParseError {
            msg: "invalid JSONC: unexpected character".to_string(),
        });
    }

    if !top_value_present(&root) {
        return Err(ParseError {
            msg: "invalid JSONC: no value found".to_string(),
        });
    }

    Ok(Jsonc { root })
}

/// Does the document have a top-level value (not just whitespace/comments)?
fn top_value_present(root: &SyntaxNode) -> bool {
    let Some(value) = root.children().find(|n| n.kind() == Sk::Value) else {
        return false;
    };
    value
        .children()
        .any(|n| matches!(n.kind(), Sk::Object | Sk::Array))
        || value
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| project::is_value_token(t.kind()))
}

impl Document for Jsonc {
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
            .any(|t| matches!(t.kind(), Sk::LineComment | Sk::BlockComment))
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
        let (source, warnings) = comments::set_node_comment(&self.root, path, kind, text)?;
        self.root = SyntaxNode::new_root(parser::build(&source));
        Ok(warnings)
    }
    fn delete_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        let source = comments::delete_node_comment(&self.root, path, kind)?;
        self.root = SyntaxNode::new_root(parser::build(&source));
        Ok(())
    }
    fn source_slice(&self, path: &[edikt_core::Step]) -> Vec<String> {
        edit::source_slice(&self.root, path)
    }
}

/// Emit a value as pretty JSON (the JSON/JSONC conversion target). JSON has no
/// comments, so nothing is dropped here beyond what the source already lost.
pub fn emit(value: &Value) -> String {
    edikt_core::convert::to_pretty_json(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use edikt_core::eval;
    use edikt_core::parse as parse_expr;

    const TSCONFIG: &str = "{\n\t// compiler settings\n\t\"compilerOptions\": {\n\t\t\"target\": \"ES2020\",   /* bump me */\n\t\t\"module\": \"commonjs\",\n\t\t\"strict\": true,\n\t\t\"lib\": [\"ES2020\", \"DOM\"],\n\t\t\"paths\": {\n\t\t\t\"@/*\": [\"./src/*\"],\n\t\t},\n\t},\n\t\"include\": [\"src/**/*\"],   // globs\n\t\"exclude\": [\n\t\t\"node_modules\",\n\t],\n}\n";

    fn roundtrips(src: &str) {
        let doc = parse(src).expect("parse");
        assert_eq!(doc.to_source(), src, "round-trip must be byte-identical");
    }

    #[test]
    fn lossless_roundtrip_corpus() {
        roundtrips(TSCONFIG);
        roundtrips("{}\n");
        roundtrips("[]");
        roundtrips("  {  \"a\" : 1 , \"b\" : [ 2 , 3 , ] }  \n");
        roundtrips("42");
        roundtrips("\"just a string\"");
        roundtrips("// leading comment\ntrue\n");
        roundtrips(
            "{\n  \"nested\": { \"deep\": { \"x\": null } },\n  \"nums\": [-1, 2.5, 1e3]\n}",
        );
        roundtrips("{ \"unicode\": \"\\u00e9\\tdone\" }");
    }

    #[test]
    fn source_slice_returns_exact_bytes() {
        let doc = parse(TSCONFIG).unwrap();
        let slice = |p: &str| doc.source_slice(parse_expr(p).unwrap().as_path().unwrap());
        // A structural result is its exact source - comment and all.
        assert_eq!(slice(".compilerOptions.lib"), vec!["[\"ES2020\", \"DOM\"]"]);
        // Iterate yields one slice per element, in order.
        assert_eq!(slice(".exclude[]"), vec!["\"node_modules\""]);
        // A nested object keeps its inner comment.
        assert!(slice(".compilerOptions")[0].contains("/* bump me */"));
    }

    #[test]
    fn projects_to_value() {
        let doc = parse(TSCONFIG).unwrap();
        let value = doc.to_value();
        // Drive it through the core evaluator to prove the projection is usable.
        let got = eval(&parse_expr(".compilerOptions.target").unwrap(), &value).unwrap();
        assert_eq!(got, vec![Value::Str("ES2020".into())]);
        let strict = eval(&parse_expr(".compilerOptions.strict").unwrap(), &value).unwrap();
        assert_eq!(strict, vec![Value::Bool(true)]);
        let lib = eval(&parse_expr(".compilerOptions.lib[]").unwrap(), &value).unwrap();
        assert_eq!(
            lib,
            vec![Value::Str("ES2020".into()), Value::Str("DOM".into())]
        );
    }

    #[test]
    fn number_and_null_projection() {
        let doc = parse("{ \"n\": -3, \"f\": 2.5, \"z\": null, \"on\": false }").unwrap();
        let v = doc.to_value();
        assert_eq!(
            eval(&parse_expr(".n").unwrap(), &v).unwrap(),
            vec![Value::Int(-3)]
        );
        assert_eq!(
            eval(&parse_expr(".f").unwrap(), &v).unwrap(),
            vec![Value::Float(2.5)]
        );
        assert_eq!(
            eval(&parse_expr(".z").unwrap(), &v).unwrap(),
            vec![Value::Null]
        );
        assert_eq!(
            eval(&parse_expr(".on").unwrap(), &v).unwrap(),
            vec![Value::Bool(false)]
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("@nope").is_err());
        assert!(parse("   \n  ").is_err()); // no value
        assert!(parse("// only a comment\n").is_err());
    }

    // --- fixture corpus (roundtrip anything in our test space) -------------

    fn fixtures_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/jsonc")
    }

    fn jsonc_fixtures() -> Vec<std::path::PathBuf> {
        let mut files: Vec<_> = std::fs::read_dir(fixtures_dir())
            .expect("fixtures/jsonc directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|x| x.to_str()),
                    Some("jsonc") | Some("json")
                )
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn roundtrips_every_fixture() {
        let files = jsonc_fixtures();
        assert!(
            files.len() >= 5,
            "expected several fixtures, found {}",
            files.len()
        );
        for path in files {
            let src = std::fs::read_to_string(&path).unwrap();
            let doc = parse(&src).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
            assert_eq!(
                doc.to_source(),
                src,
                "round-trip must be byte-identical: {}",
                path.display()
            );
        }
    }

    #[test]
    fn every_fixture_projects() {
        for path in jsonc_fixtures() {
            let src = std::fs::read_to_string(&path).unwrap();
            let value = parse(&src).unwrap().to_value();
            assert_eq!(eval(&parse_expr(".").unwrap(), &value).unwrap().len(), 1);
        }
    }

    #[test]
    fn tsconfig_queries_match_expected() {
        let src = std::fs::read_to_string(fixtures_dir().join("tsconfig.jsonc")).unwrap();
        let v = parse(&src).unwrap().to_value();
        let q = |e: &str| eval(&parse_expr(e).unwrap(), &v).unwrap();
        assert_eq!(
            q(".compilerOptions.target"),
            vec![Value::Str("ES2020".into())]
        );
        assert_eq!(q(".compilerOptions.strict"), vec![Value::Bool(true)]);
        assert_eq!(q(".include[0]"), vec![Value::Str("src/**/*".into())]);
        assert_eq!(
            q(".exclude[]"),
            vec![Value::Str("node_modules".into()), Value::Str("dist".into())]
        );
        assert_eq!(q(".compilerOptions.lib | length"), vec![Value::Int(2)]);
    }

    // --- format-preserving edits (surgical, anti-parity) ------------------

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
    fn comment_mutation_head_inline_and_element() {
        // Head above a member (only that region changes).
        assert_eq!(
            cedit(
                "{\n  \"strict\": true,\n  \"target\": \"ES2020\"\n}\n",
                ".target.# = \"level\""
            ),
            "{\n  \"strict\": true,\n  // level\n  \"target\": \"ES2020\"\n}\n"
        );
        // Inline after the value (past the comma).
        assert_eq!(
            cedit(
                "{\n  \"strict\": true,\n  \"x\": 1\n}\n",
                ".strict.#.inline = \"checks\""
            ),
            "{\n  \"strict\": true, // checks\n  \"x\": 1\n}\n"
        );
        // Head on an array element.
        assert_eq!(
            cedit(
                "{\n  \"xs\": [\n    \"a\",\n    \"b\"\n  ]\n}\n",
                ".xs[1].# = \"second\""
            ),
            "{\n  \"xs\": [\n    \"a\",\n    // second\n    \"b\"\n  ]\n}\n"
        );
        // Replace one member's head, keep a sibling's comment.
        assert_eq!(
            cedit(
                "{\n  // keep\n  \"a\": 1,\n  // old\n  \"b\": 2\n}\n",
                ".b.# |= ascii_upcase"
            ),
            "{\n  // keep\n  \"a\": 1,\n  // OLD\n  \"b\": 2\n}\n"
        );
        // Delete a head comment.
        assert_eq!(
            cedit("{\n  // drop\n  \"a\": 1\n}\n", "del(.a.#)"),
            "{\n  \"a\": 1\n}\n"
        );
    }

    #[test]
    fn comment_on_compact_object_defers_to_reflow() {
        let mut doc = parse("{ \"a\": 1, \"b\": 2 }").unwrap();
        let err =
            edikt_core::apply_comment_mutation(&mut doc, &parse_expr(".b.# = \"x\"").unwrap())
                .unwrap_err()
                .to_string();
        assert!(err.contains("layout expansion"), "got: {err}");
    }

    fn changed_lines(a: &str, b: &str) -> Vec<usize> {
        let la: Vec<_> = a.lines().collect();
        let lb: Vec<_> = b.lines().collect();
        (0..la.len().max(lb.len()))
            .filter(|&i| la.get(i) != lb.get(i))
            .collect()
    }

    #[test]
    fn set_touches_exactly_one_line_and_keeps_comments() {
        let src = std::fs::read_to_string(fixtures_dir().join("tsconfig.jsonc")).unwrap();
        let out = edit_src(&src, r#".compilerOptions.target = "ES2022""#);
        assert_eq!(
            changed_lines(&src, &out),
            vec![3],
            "only the target line should change"
        );
        assert!(out.contains(r#""target": "ES2022""#));
        // comments and trailing commas survive the edit
        assert!(out.contains("// TypeScript compiler configuration"));
        assert!(out.contains("/* language level */"));
        assert!(out.contains("\"dist\",\n\t],"));
        assert!(parse(&out).is_ok(), "edited output must re-parse");
    }

    #[test]
    fn set_scalars() {
        assert_eq!(
            edit_src("{ \"a\": true }", ".a = false"),
            "{ \"a\": false }"
        );
        assert_eq!(edit_src("{ \"a\": 1 }", ".a = 42"), "{ \"a\": 42 }");
        assert_eq!(edit_src("{ \"a\": 1 }", ".a = null"), "{ \"a\": null }");
        assert_eq!(
            edit_src("{ \"a\": \"x\" }", r#".a = "y""#),
            "{ \"a\": \"y\" }"
        );
    }

    #[test]
    fn set_into_array_index() {
        assert_eq!(edit_src("[10, 20, 30]", ".[1] = 99"), "[10, 99, 30]");
        assert_eq!(edit_src("[10, 20, 30]", ".[-1] = 99"), "[10, 20, 99]");
    }

    #[test]
    fn update_assign_over_cst() {
        assert_eq!(
            edit_src("{ \"count\": 5 }", ".count |= . + 1"),
            "{ \"count\": 6 }"
        );
        assert_eq!(
            edit_src("{ \"n\": \"edikt\" }", ".n |= ascii_upcase"),
            "{ \"n\": \"EDIKT\" }"
        );
    }

    #[test]
    fn piped_assignments() {
        assert_eq!(
            edit_src("{ \"a\": 1, \"b\": 2 }", ".a = 9 | .b = 8"),
            "{ \"a\": 9, \"b\": 8 }"
        );
    }

    #[test]
    fn adding_a_key_on_each_fixture_keeps_it_valid() {
        // Every fixture is a top-level object; adding a fresh key must keep the
        // doc valid JSONC and preserve the unrelated bytes.
        for path in jsonc_fixtures() {
            let src = std::fs::read_to_string(&path).unwrap();
            let mut doc = parse(&src).unwrap();
            apply(&mut doc, &parse_expr(r#".__added__ = "x""#).unwrap()).unwrap();
            let out = doc.to_source();
            assert!(out.contains("__added__"));
            assert!(
                parse(&out).is_ok(),
                "{}: edited output must re-parse",
                path.display()
            );
        }
    }

    #[test]
    fn non_assignment_and_bad_create_error() {
        let mut doc = parse("{ \"a\": 1 }").unwrap();
        assert!(apply(&mut doc, &parse_expr(".a").unwrap()).is_err()); // not an assignment
        assert!(apply(&mut doc, &parse_expr(".a.b = 2").unwrap()).is_err()); // create in scalar
    }

    #[test]
    fn del_object_members() {
        let src = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}\n";
        assert_eq!(edit_src(src, "del(.b)"), "{\n  \"a\": 1,\n  \"c\": 3\n}\n");
        assert_eq!(edit_src(src, "del(.a)"), "{\n  \"b\": 2,\n  \"c\": 3\n}\n");
        // Deleting the last member must NOT leave the previous member's separator
        // comma dangling before `}` - that is invalid strict JSON. This source is
        // strict (no trailing comma), so the result stays strict.
        assert_eq!(edit_src(src, "del(.c)"), "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
    }

    #[test]
    fn del_last_member_preserves_trailing_comma_style() {
        // When the object is already trailing-comma style (JSON5/JSONC), deleting
        // the last member leaves the previous member's trailing comma - that is
        // the file's own style, preserved, and still valid JSON5/JSONC.
        let src = "{\n  \"a\": 1,\n  \"b\": 2,\n}\n";
        assert_eq!(edit_src(src, "del(.b)"), "{\n  \"a\": 1,\n}\n");
    }

    #[test]
    fn del_only_member() {
        assert_eq!(edit_src("{\n  \"a\": 1\n}\n", "del(.a)"), "{\n}\n");
    }

    #[test]
    fn del_object_members_in_pipeline() {
        let src = "{\n  \"remote\": {\n    \"a\": 1,\n    \"b\": 2,\n    \"c\": 3\n  }\n}\n";
        assert_eq!(
            edit_src(src, "del(.remote.a) | del(.remote.b)"),
            "{\n  \"remote\": {\n    \"c\": 3\n  }\n}\n"
        );
    }

    #[test]
    fn del_array_elements() {
        assert_eq!(edit_src("[10, 20, 30]", "del(.[1])"), "[10, 30]");
        assert_eq!(edit_src("[10, 20, 30]", "del(.[0])"), "[20, 30]");
        assert_eq!(edit_src("[10, 20, 30]", "del(.[-1])"), "[10, 20]");
        assert_eq!(edit_src("[10]", "del(.[0])"), "[]");
    }

    #[test]
    fn del_array_elements_in_pipeline() {
        assert_eq!(edit_src("[10, 20, 30]", "del(.[0]) | del(.[0])"), "[30]");
    }

    #[test]
    fn del_missing_is_noop() {
        let src = "{ \"a\": 1 }";
        assert_eq!(edit_src(src, "del(.nope)"), src);
        assert_eq!(edit_src("[1, 2]", "del(.[9])"), "[1, 2]");
    }

    #[test]
    fn del_removes_one_line_and_keeps_comments() {
        let src = std::fs::read_to_string(fixtures_dir().join("tsconfig.jsonc")).unwrap();
        let out = edit_src(&src, "del(.compilerOptions.module)");
        assert!(!out.contains("\"module\""));
        assert_eq!(src.lines().count() - out.lines().count(), 1);
        assert!(out.contains("// TypeScript compiler configuration"));
        assert!(out.contains("/* language level */"));
        assert!(parse(&out).is_ok(), "edited output must re-parse");
    }

    #[test]
    fn add_assign_scalar_reduces_to_set() {
        assert_eq!(
            edit_src("{ \"count\": 5 }", ".count += 3"),
            "{ \"count\": 8 }"
        );
        assert_eq!(
            edit_src("{ \"s\": \"a\" }", r#".s += "b""#),
            "{ \"s\": \"ab\" }"
        );
    }

    #[test]
    fn append_single_line_array() {
        assert_eq!(
            edit_src(r#"["a", "b"]"#, r#". += ["c"]"#),
            r#"["a", "b", "c"]"#
        );
        assert_eq!(edit_src("[]", ". += [1, 2]"), "[1, 2]");
    }

    #[test]
    fn append_multiline_array_matches_indent_and_trailing_comma() {
        let src = "{\n  \"exclude\": [\n    \"node_modules\",\n    \"dist\",\n  ]\n}\n";
        let out = edit_src(src, r#".exclude += ["coverage"]"#);
        assert_eq!(
            out,
            "{\n  \"exclude\": [\n    \"node_modules\",\n    \"dist\",\n    \"coverage\",\n  ]\n}\n"
        );
    }

    #[test]
    fn append_to_fixture_lib_keeps_comments() {
        let src = std::fs::read_to_string(fixtures_dir().join("tsconfig.jsonc")).unwrap();
        let out = edit_src(&src, r#".compilerOptions.lib += ["WebWorker"]"#);
        assert!(out.contains(r#"["ES2020", "DOM", "WebWorker"]"#));
        assert!(out.contains("// TypeScript compiler configuration"));
        assert!(parse(&out).is_ok());
    }

    // --- new-key creation -------------------------------------------------

    #[test]
    fn creates_new_key_single_line() {
        assert_eq!(edit_src("{ \"a\": 1 }", ".b = 2"), "{ \"a\": 1, \"b\": 2 }");
        assert_eq!(edit_src("{}", ".a = 1"), "{\"a\": 1}");
    }

    #[test]
    fn creates_new_key_multiline_matches_style() {
        let src = "{\n  \"a\": 1,\n  \"b\": 2,\n}\n";
        assert_eq!(
            edit_src(src, ".c = 3"),
            "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3,\n}\n"
        );
    }

    #[test]
    fn creates_new_key_multiline_strict_stays_strict() {
        // The reported bug (claude_desktop_config.json): appending a new last key
        // to a strict-JSON object (no trailing comma) must not manufacture one.
        let src = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";
        assert_eq!(
            edit_src(src, ".c = 3"),
            "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}\n"
        );
    }

    #[test]
    fn append_multiline_array_strict_stays_strict() {
        let src = "{\n  \"xs\": [\n    1,\n    2\n  ]\n}\n";
        assert_eq!(
            edit_src(src, ".xs += [3]"),
            "{\n  \"xs\": [\n    1,\n    2,\n    3\n  ]\n}\n"
        );
    }

    #[test]
    fn creates_intermediate_objects() {
        assert_eq!(
            edit_src("{ \"x\": {} }", ".x.y.z = 1"),
            "{ \"x\": {\"y\": {\"z\":1}} }"
        );
    }

    #[test]
    fn creates_key_in_fixture_keeps_comments() {
        let src = std::fs::read_to_string(fixtures_dir().join("tsconfig.jsonc")).unwrap();
        let out = edit_src(&src, ".compilerOptions.noEmit = true");
        assert!(out.contains("\"noEmit\": true"));
        assert!(out.contains("// TypeScript compiler configuration"));
        assert!(out.contains("/* language level */"));
        assert!(parse(&out).is_ok());
    }

    #[test]
    fn cannot_create_key_in_a_scalar() {
        let mut doc = parse("{ \"a\": 1 }").unwrap();
        assert!(apply(&mut doc, &parse_expr(".a.b = 2").unwrap()).is_err());
    }

    // --- comment model (extraction + commented emit) -----------------------

    #[test]
    fn extracts_head_inline_and_foot_comments() {
        let src = "{\n  // section\n  \"a\": 1, // why\n  \"b\": {\n    \"c\": 2, /* note */\n    // trailing\n  },\n}\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let edikt_core::CommentedNode::Object(entries) = &c.node else {
            panic!("expected object");
        };
        assert_eq!(entries[0].0, "a");
        assert_eq!(entries[0].1.comments.head, vec!["section"]);
        assert_eq!(entries[0].1.comments.inline.as_deref(), Some("why"));
        let edikt_core::CommentedNode::Object(inner) = &entries[1].1.node else {
            panic!("expected nested object");
        };
        assert_eq!(inner[0].1.comments.inline.as_deref(), Some("note"));
        assert_eq!(inner[0].1.comments.foot, vec!["trailing"]);
        // Shape matches to_value exactly.
        assert_eq!(c.to_value(), parse(src).unwrap().to_value());
    }

    #[test]
    fn extracts_document_banner_and_trailer() {
        let src = "// banner\n{ \"a\": 1 }\n// trailer\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        assert_eq!(c.comments.head, vec!["banner"]);
        assert_eq!(c.comments.foot, vec!["trailer"]);
    }

    #[test]
    fn extracts_multiline_block_comment_as_head_lines() {
        let src = "{\n  /* one\n   * two */\n  \"a\": 1\n}";
        let c = parse(src).unwrap().to_commented().unwrap();
        let edikt_core::CommentedNode::Object(entries) = &c.node else {
            panic!("expected object");
        };
        assert_eq!(entries[0].1.comments.head, vec!["one", "two"]);
    }

    #[test]
    fn commented_emit_places_all_kinds() {
        let src = "{\n  // section\n  \"a\": 1, // why\n  \"xs\": [\n    2,\n    // last\n    3,\n  ],\n}\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let out = emit_commented(&c);
        assert_eq!(
            out,
            "{\n  // section\n  \"a\": 1, // why\n  \"xs\": [\n    2,\n    // last\n    3\n  ]\n}\n"
        );
        // The output re-parses, and the comments survive another round.
        let again = parse(&out).unwrap().to_commented().unwrap();
        assert_eq!(again, c);
    }

    #[test]
    fn tsconfig_fixture_comments_survive_extraction_and_emit() {
        let src = std::fs::read_to_string(fixtures_dir().join("tsconfig.jsonc")).unwrap();
        let doc = parse(&src).unwrap();
        let c = doc.to_commented().unwrap();
        assert!(c.has_comments());
        assert_eq!(c.to_value(), doc.to_value(), "shapes must match");
        let out = emit_commented(&c);
        assert!(out.contains("// TypeScript compiler configuration"));
        // The `/* language level */` block comment re-emits as a line comment.
        assert!(out.contains("// language level"), "got: {out}");
        assert!(parse(&out).is_ok(), "commented emit must re-parse");
    }

    // --- projection edge cases (numbers, escapes, absent values) -----------

    #[test]
    fn projects_big_int_as_float_and_unescapes_every_escape() {
        // A magnitude past i64 falls back to float instead of clamping to 0.
        let doc = parse("{ \"big\": 99999999999999999999 }").unwrap();
        assert_eq!(
            eval(&parse_expr(".big").unwrap(), &doc.to_value()).unwrap(),
            vec![Value::Float(99999999999999999999.0)]
        );
        // Every string escape the unescaper handles, including an unknown escape
        // (kept verbatim as `\q`) and an invalid `\u` (dropped).
        let doc = parse("{ \"s\": \"a\\/b\\n\\r\\b\\f\\tz\\q\\u00e9\\uzzzz\" }").unwrap();
        assert_eq!(
            eval(&parse_expr(".s").unwrap(), &doc.to_value()).unwrap(),
            vec![Value::Str("a/b\n\r\u{8}\u{c}\tz\\q\u{e9}".into())]
        );
    }

    #[test]
    fn projects_absent_member_value_as_null() {
        // A malformed member with no value must project to null, not panic - and
        // still round-trip byte-for-byte (the moat holds even on garbage input).
        let doc = parse("{\"a\":}").unwrap();
        assert_eq!(doc.to_source(), "{\"a\":}");
        assert_eq!(
            eval(&parse_expr(".a").unwrap(), &doc.to_value()).unwrap(),
            vec![Value::Null]
        );
    }

    #[test]
    fn unescape_guards_a_lone_trailing_backslash() {
        // Defensive: the lexer never yields this, but the unescaper must not
        // panic on a token whose content ends in a bare backslash.
        assert_eq!(crate::project::unescape("\"a\\\""), "a\\");
    }

    // --- source_slice / resolve edge cases --------------------------------

    #[test]
    fn source_slice_iterates_objects_and_skips_scalars() {
        let doc = parse(TSCONFIG).unwrap();
        let slice = |p: &str| doc.source_slice(parse_expr(p).unwrap().as_path().unwrap());
        // Iterating an object yields each member's value source, in order.
        assert_eq!(slice(".compilerOptions.paths[]"), vec!["[\"./src/*\"]"]);
        // Iterating a scalar yields nothing (not an error).
        assert!(slice(".compilerOptions.strict[]").is_empty());
        // An index that lands below the start of the array resolves to nothing.
        assert!(slice(".exclude[-5]").is_empty());
    }

    #[test]
    fn value_at_rejects_iterate_and_comment_steps() {
        let doc = parse(TSCONFIG).unwrap();
        let at = |p: &str| doc.value_at(parse_expr(p).unwrap().as_path().unwrap());
        // `[]` is not a single navigable value node.
        assert!(at(".compilerOptions.lib[]").is_none());
        // A `#` comment step never resolves to a value.
        assert!(at(".compilerOptions.strict.#").is_none());
    }

    // --- append / create edge cases ---------------------------------------

    #[test]
    fn append_single_line_array_with_trailing_comma() {
        // A single-line array that already carries a trailing comma keeps it.
        assert_eq!(edit_src("[1, 2,]", ". += [3]"), "[1, 2, 3,]");
    }

    fn edit_err(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        apply(&mut doc, &parse_expr(expr).unwrap())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn creating_through_index_iterate_or_comment_errors() {
        assert!(
            edit_err("{}", ".a[0] = 1").contains("cannot create array elements by index"),
            "index create"
        );
        assert!(
            edit_err("{}", ".a[] = 1").contains("cannot create through `[]`"),
            "iterate create"
        );
        assert!(
            edit_err("{}", ".a.# = 1").contains("editing comments"),
            "comment create"
        );
    }

    #[test]
    fn del_with_extra_arguments_errors() {
        assert!(
            edit_err("{ \"a\": 1 }", "del(.a; .b)").contains("one path argument"),
            "del arity"
        );
    }

    #[test]
    fn del_dot_iterate_and_comment_are_rejected_or_noop() {
        // del(.) is refused outright.
        assert!(
            edit_err("{ \"a\": 1 }", "del(.)").contains("del(.) is not allowed"),
            "del(.)"
        );
        // del(.[]) is a known-unsupported form.
        assert!(
            edit_err("[1, 2]", "del(.[])").contains("del(.[]) is not supported"),
            "del(.[])"
        );
        // del of a comment through the plain edit path is refused (comment edits
        // route elsewhere).
        assert!(
            edit_err("{ \"a\": 1 }", "del(.a.#)").contains("deleting comments"),
            "del(.a.#)"
        );
        // Deleting through an absent parent is a silent no-op.
        assert_eq!(edit_src("{ \"a\": 1 }", "del(.nope.deep)"), "{ \"a\": 1 }");
    }

    // --- comment write-back edge cases ------------------------------------

    fn cedit_err(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        edikt_core::apply_comment_mutation(&mut doc, &parse_expr(expr).unwrap())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn comment_delete_edge_cases() {
        // Deleting a comment at a missing key is a no-op.
        assert_eq!(
            cedit("{\n  \"a\": 1\n}\n", "del(.nope.#)"),
            "{\n  \"a\": 1\n}\n"
        );
        // Deleting a head/foot on a compact object has nothing to drop.
        assert_eq!(
            cedit("{ \"a\": 1, \"b\": 2 }", "del(.b.#)"),
            "{ \"a\": 1, \"b\": 2 }"
        );
        // Deleting an inline comment removes exactly its bytes.
        assert_eq!(
            cedit(
                "{\n  \"a\": 1, // note\n  \"b\": 2\n}\n",
                "del(.a.#.inline)"
            ),
            "{\n  \"a\": 1,\n  \"b\": 2\n}\n"
        );
    }

    #[test]
    fn inline_comment_layout_errors() {
        // A compact object leaves no room for a trailing inline comment.
        assert!(
            cedit_err("{ \"a\": 1 }", ".a.#.inline = \"x\"").contains("layout expansion"),
            "compact inline"
        );
        // An inline comment on a value that spans lines is not yet supported.
        assert!(
            cedit_err(
                "{\n  \"obj\": {\n    \"x\": 1\n  }\n}\n",
                ".obj.#.inline = \"n\""
            )
            .contains("multi-line"),
            "multi-line inline"
        );
    }

    #[test]
    fn comment_target_resolution_errors() {
        // The document banner `.#` is not editable for JSONC yet.
        assert!(
            cedit_err("{ \"a\": 1 }", ".# = \"x\"").contains("document-level"),
            "banner"
        );
        // An out-of-range array index is a clean error.
        assert!(
            cedit_err("{\n  \"xs\": [1, 2]\n}\n", ".xs[9].# = \"x\"").contains("out of range"),
            "index range"
        );
        // A `[]` before `#` addresses no single element.
        assert!(
            cedit_err("{\n  \"xs\": [1, 2]\n}\n", ".xs[].# = \"x\"")
                .contains("object keys or array elements"),
            "iterate comment"
        );
    }

    // --- comment extraction edge cases ------------------------------------

    #[test]
    fn extracts_member_internal_head_and_inline_comments() {
        // Comments *inside* a member: before the value (head), and after it but
        // before the comma (inline), the latter joined across two lines.
        let src = "{\"x\": /* h */ 1 /* i\nj */, \"y\": 2}";
        let c = parse(src).unwrap().to_commented().unwrap();
        let edikt_core::CommentedNode::Object(entries) = &c.node else {
            panic!("expected object");
        };
        assert_eq!(entries[0].0, "x");
        assert_eq!(entries[0].1.comments.head, vec!["h"]);
        assert_eq!(entries[0].1.comments.inline.as_deref(), Some("i j"));
    }

    #[test]
    fn empty_inline_comment_contributes_nothing() {
        let src = "{\n  \"a\": 1, //\n  \"b\": 2\n}\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        let edikt_core::CommentedNode::Object(entries) = &c.node else {
            panic!("expected object");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].1.comments.inline, None);
    }

    #[test]
    fn empty_container_keeps_its_own_foot_comment() {
        let src = "{\n  // note\n}\n";
        let c = parse(src).unwrap().to_commented().unwrap();
        assert_eq!(c.comments.foot, vec!["note"]);
        let edikt_core::CommentedNode::Object(entries) = &c.node else {
            panic!("expected object");
        };
        assert!(entries.is_empty());
    }

    #[test]
    fn extracts_top_level_inline_and_multiline_trailer() {
        // An inline comment on the top-level value.
        let c = parse("true // yes\n").unwrap().to_commented().unwrap();
        assert_eq!(c.comments.inline.as_deref(), Some("yes"));
        // A multi-line block comment after the value becomes a multi-line foot.
        let c = parse("{}\n/* a\nb */\n").unwrap().to_commented().unwrap();
        assert_eq!(c.comments.foot, vec!["a", "b"]);
    }

    #[test]
    fn stray_trailing_token_does_not_break_the_commented_projection() {
        // Trailing garbage past the top value is tolerated (parsed losslessly);
        // the commented projection just reflects the leading value.
        let c = parse("true false").unwrap().to_commented().unwrap();
        assert_eq!(c.to_value(), Value::Bool(true));
    }

    // --- commented emit edge cases ----------------------------------------

    #[test]
    fn emits_document_level_head_inline_and_foot() {
        let c = parse("// banner\ntrue // yes\n// trailer\n")
            .unwrap()
            .to_commented()
            .unwrap();
        assert_eq!(emit_commented(&c), "// banner\ntrue // yes\n// trailer\n");
    }

    #[test]
    fn emits_array_element_inline_and_foot() {
        let c = parse("[\n  1 /* one */,\n  2\n  // foot\n]\n")
            .unwrap()
            .to_commented()
            .unwrap();
        assert_eq!(emit_commented(&c), "[\n  1, // one\n  2\n  // foot\n]\n");
    }

    #[test]
    fn emits_object_member_foot() {
        let c = parse("{\n  \"a\": 1\n  // foot\n}\n")
            .unwrap()
            .to_commented()
            .unwrap();
        assert_eq!(emit_commented(&c), "{\n  \"a\": 1\n  // foot\n}\n");
    }

    // --- public API surface ------------------------------------------------

    #[test]
    fn document_trait_and_public_accessors() {
        use edikt_core::Document;
        let mut doc = parse(TSCONFIG).unwrap();
        // syntax() exposes the lossless root node.
        assert_eq!(doc.syntax().text().to_string(), TSCONFIG);
        // Capability + comment probes.
        assert!(doc.features().contains(&Feature::Comments));
        assert!(doc.has_comments());
        // A mutation driven through the trait method (not the free `apply`).
        Document::apply(
            &mut doc,
            &parse_expr(".compilerOptions.strict = false").unwrap(),
        )
        .unwrap();
        assert!(doc.to_source().contains("\"strict\": false"));
        // emit(): the pretty-JSON conversion target.
        let json = emit(&doc.to_value());
        assert!(json.contains("\"compilerOptions\""));
    }
}
