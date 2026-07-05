//! edikt JSONC/JSON5/JSON format module.
//!
//! A lossless `rowan` + `logos` CST (the day-zero spike, productionized): parse
//! JSONC into a tree that round-trips byte-for-byte, project it to
//! [`edikt_core::Value`] for querying, and — with M2 — edit it in place touching
//! only the targeted nodes. `.json` is read by the same parser (it is a subset
//! with no comments to preserve).

mod edit;
mod lexer;
mod parser;
mod project;
mod syntax;

pub use edikt_core::EditError;
pub use edit::apply;

use edikt_core::{Document, Expr, Feature, Step, Value};
use syntax::{Sk, SyntaxNode};

/// Capabilities of the JSONC/JSON5 family.
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
            return Ok(()); // parent path absent → nothing to delete
        };
        match last {
            Step::Field(k) => {
                let member = container
                    .children()
                    .find(|n| n.kind() == Sk::Object)
                    .and_then(|object| edit::find_member(&object, k));
                if let Some(member) = member {
                    edit::delete_member(&member);
                    self.root = root;
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
                    self.root = root;
                }
                Ok(())
            }
            Step::Iterate => Err(EditError::new("del(.[]) is not supported yet")),
        }
    }
}

/// Parse JSONC source into a [`Jsonc`] document.
pub fn parse(src: &str) -> Result<Jsonc, ParseError> {
    let green = parser::build(src);
    let root = SyntaxNode::new_root(green);

    // An unrecognized byte is lexed as an error token — reject rather than
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
    fn apply(&mut self, expr: &Expr) -> Result<(), EditError> {
        edit::apply(self, expr)
    }
    fn has_comments(&self) -> bool {
        self.root
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| matches!(t.kind(), Sk::LineComment | Sk::BlockComment))
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
        // comments and trailing commas survive — the thing jq/yq can't do
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
        // deleting the last member leaves a (valid JSONC) trailing comma
        assert_eq!(edit_src(src, "del(.c)"), "{\n  \"a\": 1,\n  \"b\": 2,\n}\n");
    }

    #[test]
    fn del_array_elements() {
        assert_eq!(edit_src("[10, 20, 30]", "del(.[1])"), "[10, 30]");
        assert_eq!(edit_src("[10, 20, 30]", "del(.[0])"), "[20, 30]");
        assert_eq!(edit_src("[10, 20, 30]", "del(.[-1])"), "[10, 20]");
        assert_eq!(edit_src("[10]", "del(.[0])"), "[]");
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
}
