//! edikt YAML format module — **query + conversion** for now.
//!
//! YAML is backed by a data-model parser (`serde_yaml`), so edikt can *read* YAML
//! (query it) and *convert* to/from it (`-T`), which is the completeness the
//! format-conversion story needs. **Lossless in-place YAML editing is deferred**:
//! there is no pure-Rust comment-preserving YAML CST to lean on (the way
//! `toml_edit` backs TOML), so rather than ship a comment-clobbering YAML editor
//! that betrays the whole point of edikt, `apply` errors and points at query /
//! conversion. The eventual edit backend is an explicit future decision — an
//! opt-in `yqlib` FFI, or a greenfield lossless CST.

use edikt_core::{Document, EditError, Expr, Feature, Value};
use serde_yaml::Value as YamlValue;

/// Capabilities of YAML.
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

/// A parsed YAML document (data-model view + original source).
pub struct Yaml {
    value: Value,
    source: String,
    had_comments: bool,
}

/// Parse YAML source into a [`Yaml`] document.
pub fn parse(src: &str) -> Result<Yaml, ParseError> {
    let yaml: YamlValue =
        serde_yaml::from_str(src).map_err(|e| ParseError { msg: e.to_string() })?;
    let had_comments = src
        .lines()
        .any(|l| l.trim_start().starts_with('#') || l.contains(" #"));
    Ok(Yaml {
        value: yaml_to_value(yaml),
        source: src.to_string(),
        had_comments,
    })
}

impl Document for Yaml {
    fn to_source(&self) -> String {
        self.source.clone()
    }
    fn to_value(&self) -> Value {
        self.value.clone()
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, _expr: &Expr) -> Result<(), EditError> {
        Err(EditError::new(
            "edikt does not losslessly edit YAML yet — query it, or convert with -T",
        ))
    }
    fn has_comments(&self) -> bool {
        self.had_comments
    }
}

/// Emit a value as YAML (for conversion targets). YAML holds nesting, arrays, and
/// typed scalars, so there is nothing to flatten.
pub fn emit(value: &Value) -> Result<(String, Vec<String>), EditError> {
    let yaml = value_to_yaml(value);
    let text = serde_yaml::to_string(&yaml).map_err(|e| EditError::new(e.to_string()))?;
    Ok((text, Vec::new()))
}

fn yaml_to_value(yaml: YamlValue) -> Value {
    match yaml {
        YamlValue::Null => Value::Null,
        YamlValue::Bool(b) => Value::Bool(b),
        YamlValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        YamlValue::String(s) => Value::Str(s),
        YamlValue::Sequence(seq) => Value::Array(seq.into_iter().map(yaml_to_value).collect()),
        YamlValue::Mapping(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (yaml_key_to_string(k), yaml_to_value(v)))
                .collect(),
        ),
        YamlValue::Tagged(tagged) => yaml_to_value(tagged.value),
    }
}

/// Non-string YAML keys are rendered to their scalar text (config keys are
/// almost always strings; this keeps the flat key model honest for the rest).
fn yaml_key_to_string(key: YamlValue) -> String {
    match key {
        YamlValue::String(s) => s,
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Number(n) => n.to_string(),
        YamlValue::Null => "null".to_string(),
        other => serde_yaml::to_string(&other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

fn value_to_yaml(value: &Value) -> YamlValue {
    match value {
        Value::Null => YamlValue::Null,
        Value::Bool(b) => YamlValue::Bool(*b),
        Value::Int(i) => YamlValue::Number((*i).into()),
        Value::Float(f) => YamlValue::Number((*f).into()),
        Value::Str(s) => YamlValue::String(s.clone()),
        Value::Array(a) => YamlValue::Sequence(a.iter().map(value_to_yaml).collect()),
        Value::Object(m) => YamlValue::Mapping(
            m.iter()
                .map(|(k, v)| (YamlValue::String(k.clone()), value_to_yaml(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edikt_core::eval;
    use edikt_core::parse as parse_expr;

    const SAMPLE: &str = "# services\nweb:\n  image: nginx:1.25   # pinned\n  ports:\n    - 80\n    - 443\n  replicas: 3\ndebug: false\n";

    fn q(src: &str, expr: &str) -> Vec<Value> {
        eval(&parse_expr(expr).unwrap(), &parse(src).unwrap().to_value()).unwrap()
    }

    #[test]
    fn queries_yaml() {
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
    }

    #[test]
    fn to_source_is_the_original() {
        assert_eq!(parse(SAMPLE).unwrap().to_source(), SAMPLE);
    }

    #[test]
    fn edit_is_refused_clearly() {
        let mut doc = parse(SAMPLE).unwrap();
        let err = doc
            .apply(&parse_expr(".debug = true").unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("YAML"));
    }

    #[test]
    fn emits_yaml_roundtrippable_through_value() {
        let value = parse(SAMPLE).unwrap().to_value();
        let (yaml, warnings) = emit(&value).unwrap();
        assert!(warnings.is_empty());
        // Re-parsing the emitted YAML yields the same value model.
        assert_eq!(parse(&yaml).unwrap().to_value(), value);
    }

    #[test]
    fn has_comments_detected() {
        assert!(parse(SAMPLE).unwrap().has_comments());
        assert!(!parse("a: 1\n").unwrap().has_comments());
    }

    #[test]
    fn queries_compose_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/yaml");
        let src = std::fs::read_to_string(dir.join("compose.yaml")).unwrap();
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
        assert_eq!(
            q(&src, ".services.web.ports[]"),
            vec![Value::Str("80:80".into()), Value::Str("443:443".into())]
        );
    }
}
