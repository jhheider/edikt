//! edikt frontmatter lens - **losslessly edit the metadata block at the top of
//! a Markdown file, body byte-for-byte untouched.**
//!
//! Frontmatter is structured config embedded in prose: a YAML/TOML/JSON block
//! fenced off at the very start of a document. This crate is a thin *lens*, not
//! a new parser - it splits the file into three parts,
//!
//! ```text
//!   prefix   the opening fence line (opaque)
//!   block    the metadata, parsed by edikt's existing YAML/TOML/JSON engine
//!   suffix   the closing fence plus the entire document body (opaque)
//! ```
//!
//! and wraps the parsed block in a [`Document`] whose only non-delegating method
//! is [`Document::to_source`], which re-splices `prefix + block + suffix`. Every
//! query, assignment, and conversion edikt already does inside a config file
//! works on the block; the body is never touched.
//!
//! ## Supported containers (v1)
//!
//! - **YAML** - `---` fence, closed by `---` or `...` (Pandoc).
//! - **TOML** - `+++` fence (Hugo, Zola).
//! - **Tagged** - `---yaml` / `---toml` / `---json` opening fence, closed by `---`.
//! - **JSON (bare brace)** - a `{ ... }` object at byte 0 (Hugo); the closing
//!   brace is found by string-aware matching.
//!
//! Commented host-language frontmatter (PEP 723 `# /// script`, scriptbox, uv)
//! is a deliberate follow-up: its block is TOML only after stripping a per-line
//! comment prefix, a different mechanism from fenced blocks.

use edikt_core::{CommentKind, Commented, Document, EditError, Expr, Feature, Step, Value};

/// Capabilities reported statically for the lens. The live per-document set is
/// delegated to the inner block (see [`Document::features`]); this superset is
/// only for the CLI's output-format suggestions, and every inner container
/// (YAML/TOML/JSON) sits within it.
pub const FEATURES: &[Feature] = &[
    Feature::Comments,
    Feature::Nesting,
    Feature::Arrays,
    Feature::TypedScalars,
];

/// A parse failure: no frontmatter block, an unterminated fence, an unknown
/// language tag, or a failure parsing the block itself.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg}")]
pub struct ParseError {
    pub msg: String,
}

impl ParseError {
    fn new(msg: impl Into<String>) -> Self {
        ParseError { msg: msg.into() }
    }
}

/// The inner serialization of a frontmatter block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lang {
    Yaml,
    Toml,
    Json,
}

/// A Markdown document viewed through its frontmatter block.
pub struct Frontmatter {
    prefix: String,
    inner: Box<dyn Document>,
    suffix: String,
}

/// Parse `src` as a frontmatter-bearing document: split off the block, parse it
/// with the matching engine, keep the rest opaque.
pub fn parse(src: &str) -> Result<Frontmatter, ParseError> {
    let Split {
        prefix,
        block,
        suffix,
        lang,
    } = split(src)?;
    let inner: Box<dyn Document> = match lang {
        Lang::Yaml => {
            Box::new(edikt_yaml::parse(block).map_err(|e| ParseError::new(e.to_string()))?)
        }
        Lang::Toml => {
            Box::new(edikt_toml::parse(block).map_err(|e| ParseError::new(e.to_string()))?)
        }
        Lang::Json => {
            Box::new(edikt_jsonc::parse(block).map_err(|e| ParseError::new(e.to_string()))?)
        }
    };
    Ok(Frontmatter {
        prefix: prefix.to_string(),
        inner,
        suffix: suffix.to_string(),
    })
}

impl Document for Frontmatter {
    fn to_source(&self) -> String {
        // The only non-delegating method: re-splice the edited (or untouched)
        // block between the opaque prefix and suffix. This is the whole moat -
        // the body's bytes are the `suffix` we captured verbatim at parse time.
        format!("{}{}{}", self.prefix, self.inner.to_source(), self.suffix)
    }
    fn to_value(&self) -> Value {
        self.inner.to_value()
    }
    fn features(&self) -> &'static [Feature] {
        self.inner.features()
    }
    fn apply(&mut self, expr: &Expr) -> Result<(), EditError> {
        self.inner.apply(expr)
    }
    fn has_comments(&self) -> bool {
        self.inner.has_comments()
    }
    fn to_commented(&self) -> Option<Commented> {
        self.inner.to_commented()
    }
    fn source_slice(&self, path: &[Step]) -> Vec<String> {
        self.inner.source_slice(path)
    }
    fn set_comment(
        &mut self,
        path: &[Step],
        kind: CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        self.inner.set_comment(path, kind, text)
    }
    fn delete_comment(&mut self, path: &[Step], kind: CommentKind) -> Result<(), EditError> {
        self.inner.delete_comment(path, kind)
    }
}

struct Split<'a> {
    prefix: &'a str,
    block: &'a str,
    suffix: &'a str,
    lang: Lang,
}

const YAML_CLOSE: &[&str] = &["---", "..."];
const TOML_CLOSE: &[&str] = &["+++"];
const DASH_CLOSE: &[&str] = &["---"];

/// Locate the frontmatter block at the very start of `src`.
fn split(src: &str) -> Result<Split<'_>, ParseError> {
    // Bare-brace JSON (Hugo): the first non-whitespace byte is `{`, and the
    // block is the object itself, body following its matching `}`.
    if src.trim_start().starts_with('{') {
        let start = src.len() - src.trim_start().len();
        let end = match_braces(src, start)
            .ok_or_else(|| ParseError::new("unterminated JSON frontmatter (no matching `}`)"))?;
        return Ok(Split {
            prefix: &src[..start],
            block: &src[start..=end],
            suffix: &src[end + 1..],
            lang: Lang::Json,
        });
    }

    // Fenced: the first line is the opening fence.
    let Some(first_end) = src.find('\n').map(|i| i + 1) else {
        return Err(ParseError::new(
            "no frontmatter block at the start of the document",
        ));
    };
    let opener = src[..first_end].trim_end();
    let (close, lang): (&[&str], Lang) = if opener == "---" {
        (YAML_CLOSE, Lang::Yaml)
    } else if opener == "+++" {
        (TOML_CLOSE, Lang::Toml)
    } else if let Some(tag) = opener.strip_prefix("---") {
        match tag.trim() {
            "yaml" | "yml" => (YAML_CLOSE, Lang::Yaml),
            "toml" => (DASH_CLOSE, Lang::Toml),
            "json" | "json5" | "jsonc" => (DASH_CLOSE, Lang::Json),
            other => {
                return Err(ParseError::new(format!(
                    "unsupported frontmatter language tag `{other}` \
                     (expected yaml, toml, or json)"
                )));
            }
        }
    } else {
        return Err(ParseError::new(
            "no frontmatter block at the start of the document",
        ));
    };

    // Scan the lines after the opener for the closing fence.
    let mut offset = first_end;
    while offset < src.len() {
        let line_end = src[offset..]
            .find('\n')
            .map(|i| offset + i + 1)
            .unwrap_or(src.len());
        if close.contains(&src[offset..line_end].trim_end()) {
            return Ok(Split {
                prefix: &src[..first_end],
                block: &src[first_end..offset],
                suffix: &src[offset..],
                lang,
            });
        }
        offset = line_end;
    }
    Err(ParseError::new(format!(
        "unterminated frontmatter (opened with `{opener}`, no closing fence)"
    )))
}

/// Byte index of the `}` matching the `{` at `start`, honoring JSON strings and
/// escapes. Only ASCII bytes are inspected, so the index is always a valid char
/// boundary.
fn match_braces(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else {
            match b {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use edikt_core::parse as parse_expr;

    fn src_of(input: &str, expr: &str) -> String {
        let mut doc = parse(input).expect("parse frontmatter");
        doc.apply(&parse_expr(expr).unwrap()).expect("apply");
        doc.to_source()
    }

    fn query(input: &str, path: &str) -> Vec<Value> {
        let doc = parse(input).expect("parse frontmatter");
        edikt_core::eval(&parse_expr(path).unwrap(), &doc.to_value()).unwrap()
    }

    const YAML_DOC: &str = "---\ntitle: Hello\nstatus: Drafted\n---\n# Body\n\nProse here.\n";

    #[test]
    fn round_trips_untouched() {
        let doc = parse(YAML_DOC).unwrap();
        assert_eq!(doc.to_source(), YAML_DOC, "unedited round-trip is identity");
    }

    #[test]
    fn edits_only_the_block_body_survives() {
        let out = src_of(YAML_DOC, r#".status = "Shipped""#);
        assert_eq!(
            out,
            "---\ntitle: Hello\nstatus: Shipped\n---\n# Body\n\nProse here.\n"
        );
        // The body after the closing fence is byte-for-byte intact.
        assert!(out.ends_with("# Body\n\nProse here.\n"));
    }

    #[test]
    fn queries_the_block() {
        assert_eq!(query(YAML_DOC, ".title"), vec![Value::Str("Hello".into())]);
    }

    #[test]
    fn toml_fence() {
        let doc = "+++\ntitle = \"T\"\n+++\nbody\n";
        assert_eq!(
            src_of(doc, r#".title = "U""#),
            "+++\ntitle = \"U\"\n+++\nbody\n"
        );
    }

    #[test]
    fn tagged_json_fence() {
        let doc = "---json\n{\"a\": 1}\n---\nbody\n";
        assert_eq!(query(doc, ".a"), vec![Value::Int(1)]);
        assert_eq!(src_of(doc, ".a = 2"), "---json\n{\"a\": 2}\n---\nbody\n");
    }

    #[test]
    fn yaml_closed_by_dots() {
        let doc = "---\nk: 1\n...\nbody\n";
        let out = src_of(doc, ".k = 2");
        assert_eq!(out, "---\nk: 2\n...\nbody\n", "`...` close is preserved");
    }

    #[test]
    fn bare_brace_json() {
        let doc = "{\n  \"title\": \"Hugo\"\n}\n\nBody text.\n";
        assert_eq!(query(doc, ".title"), vec![Value::Str("Hugo".into())]);
        let out = src_of(doc, r#".title = "Edited""#);
        assert_eq!(out, "{\n  \"title\": \"Edited\"\n}\n\nBody text.\n");
    }

    #[test]
    fn bare_brace_ignores_braces_in_strings() {
        // A `}` inside a string value must not end the block early.
        let doc = "{\"re\": \"a}b\", \"n\": 1}\nbody\n";
        let d = parse(doc).unwrap();
        assert_eq!(d.to_source(), doc);
        assert_eq!(query(doc, ".n"), vec![Value::Int(1)]);
    }

    #[test]
    fn crlf_body_preserved() {
        let doc = "---\r\ntitle: X\r\n---\r\nbody\r\n";
        let out = src_of(doc, r#".title = "Y""#);
        assert!(
            out.ends_with("---\r\nbody\r\n"),
            "CRLF body intact: {out:?}"
        );
    }

    #[test]
    fn no_frontmatter_errors() {
        let e = parse("# Just a heading\n\nno block here\n").err().unwrap();
        assert!(e.to_string().contains("no frontmatter block"), "{e}");
    }

    #[test]
    fn unterminated_errors() {
        let e = parse("---\ntitle: X\n").err().unwrap();
        assert!(e.to_string().contains("unterminated"), "{e}");
    }

    #[test]
    fn unknown_tag_errors() {
        let e = parse("---xml\n<a/>\n---\n").err().unwrap();
        assert!(
            e.to_string()
                .contains("unsupported frontmatter language tag"),
            "{e}"
        );
    }

    #[test]
    fn roundtrips_every_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/markdown");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("fixtures/markdown directory") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
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
        assert!(count >= 3, "expected markdown fixtures, found {count}");
    }

    #[test]
    fn mid_document_rule_is_not_a_fence() {
        // A `---` inside the body (a horizontal rule) must not be mistaken for
        // the closing fence; only the first one after the opener closes it.
        let doc = "---\nk: 1\n---\n\nBefore rule.\n\n---\n\nAfter rule.\n";
        let out = src_of(doc, ".k = 2");
        assert_eq!(
            out,
            "---\nk: 2\n---\n\nBefore rule.\n\n---\n\nAfter rule.\n"
        );
    }

    #[test]
    fn preserves_block_comments_on_edit() {
        // A YAML comment inside the block survives an unrelated edit (the inner
        // engine's job; the lens must not disturb it).
        let doc = "---\ntitle: X  # keep me\nn: 1\n---\nbody\n";
        let out = src_of(doc, ".n = 2");
        assert!(out.contains("# keep me"), "block comment kept: {out}");
    }
}
