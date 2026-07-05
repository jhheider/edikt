//! edikt JSONC/JSON5/JSON format module.
//!
//! A lossless `rowan` + `logos` CST (the day-zero spike, productionized): parse
//! JSONC into a tree that round-trips byte-for-byte, project it to
//! [`edikt_core::Value`] for querying, and — with M2 — edit it in place touching
//! only the targeted nodes. `.json` is read by the same parser (it is a subset
//! with no comments to preserve).

mod lexer;
mod parser;
mod project;
mod syntax;

use edikt_core::{Document, Feature, Value};
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
    /// Access the underlying syntax tree (needed by the M2 edit path).
    pub fn syntax(&self) -> &SyntaxNode {
        &self.root
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
}
