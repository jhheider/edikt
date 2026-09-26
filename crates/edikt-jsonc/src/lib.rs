//! edikt JSONC/JSON5/JSON format module.
//!
//! A lossless `rowan` + `logos` CST (the day-zero spike, productionized): parse
//! JSONC into a tree that round-trips byte-for-byte, project it to
//! [`edikt_core::Value`] for querying, and edit it in place touching only the
//! targeted nodes. Structurally malformed input is rejected at [`parse`] with
//! a line and column, so an edit never splices into a broken tree. `.json` is read by the same parser (it is a subset
//! with no comments to preserve).
//!
//! Everything needed to drive a document is reachable from this crate alone -
//! no direct `edikt-core` dependency required (jhheider/edikt#66):
//!
//! ```
//! use edikt_jsonc::{Document, Step, json, parse};
//!
//! let mut doc = parse("{\n  // keep me\n  \"a\": 1,\n}\n").unwrap();
//! doc.set(&[Step::Field("a".into())], &json!({ "nested": [true, null] }))
//!     .unwrap();
//!
//! // The comment, the indent and the trailing comma all survive.
//! assert_eq!(
//!     doc.to_source(),
//!     "{\n  // keep me\n  \"a\": {\"nested\":[true,null]},\n}\n"
//! );
//! ```

mod comments;
mod edit;
mod lexer;
mod parser;
mod project;
mod syntax;

pub use comments::emit_commented;
pub use edit::apply;

// The edikt-core types that appear in this crate's own public API, re-exported
// so a dependent can call these methods without also taking a direct
// edikt-core dependency (jhheider/edikt#66). `parse` is aliased because this
// crate's own `parse` is the document parser.
pub use edikt_core::{
    CommentKind, Commented, Document, EditError, Expr, Feature, Step, Value, json,
    parse as parse_expr,
};
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
    /// Whether the source uses any spelling strict JSON forbids (JSON5's
    /// unquoted keys, single-quoted strings, `+`/hex/leading-dot numbers,
    /// `Infinity`/`NaN`, line continuations). Gates what bytes a **freshly
    /// inserted** value may use: a JSON5 document can take an `Infinity`
    /// literal; a strict-JSON document cannot (it errors rather than writing
    /// `null`). Untouched regions round-trip regardless - this only decides the
    /// spelling of new nodes.
    json5: bool,
    /// The source opened with a UTF-8 byte-order mark: kept out of the tree
    /// (it is not JSON) and restored by `to_source`.
    bom: bool,
}

impl Jsonc {
    /// Access the underlying syntax tree.
    pub fn syntax(&self) -> &SyntaxNode {
        &self.root
    }

    /// The file's dominant line ending, for lines an insertion adds.
    fn eol(&self) -> &'static str {
        edikt_core::text::dominant(&edikt_syntax::to_source(&self.root))
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
            container.replace_with(edit::replacement_green(&container, value, self.json5)?)
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
            let text =
                edit::insert_into_object(&object, key, &member_value, self.json5, self.eol())?;
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
        let new_text = edit::insert_into_array(&array, items, self.json5, self.eol())?;
        let new_root = array.replace_with(edit::array_green_from_text(&new_text));
        self.root = SyntaxNode::new_root(new_root);
        Ok(())
    }

    /// Delete the value at `path`, format-preserving: the member's or element's
    /// line is removed cleanly (no dangling comma or blank line). A missing key
    /// or out-of-range index is a no-op (jq semantics). A `[]` in `path`
    /// deletes **every** iterated element/member (jq's `del(.a[])` empties the
    /// collection), one clean splice each.
    pub fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        // Fan-out delete: resolve the iterate to concrete index/key paths and
        // delete each through the ordinary single-target machinery, back-to-
        // front so indices stay valid as the collection shrinks.
        if path.contains(&Step::Iterate) {
            let whole = self.to_value();
            let paths = edikt_core::expand_delete_paths(path, &whole)?;
            for p in &paths {
                self.delete(p)?;
            }
            return Ok(());
        }
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
                        let idx = edikt_core::normalize_index(*i, values.len())?;
                        values.into_iter().nth(idx)
                    });
                if let Some(value) = value {
                    edit::delete_element(&value);
                    self.root = SyntaxNode::new_root(root.green().into_owned());
                }
                Ok(())
            }
            Step::Iterate => unreachable!("`[]` fanned out above"),
            Step::Comment(_) => Err(EditError::new(
                "deleting comments (`#`): the comment step must end the path and be \
                 deleted on its own, e.g. `del(.foo.#)`",
            )),
        }
    }
}

/// Parse JSONC source into a [`Jsonc`] document.
///
/// Malformed input is rejected with the first problem's line and column: an
/// unrecognized character, a missing `:` or `,`, a stray token where a key or
/// value belongs, an unclosed container, or anything but whitespace and
/// comments after the top-level value. Editing a structurally broken document
/// would splice into a tree that does not mean what the bytes say.
pub fn parse(src: &str) -> Result<Jsonc, ParseError> {
    let (bom, src) = edikt_core::text::split_bom(src);
    let (green, errors) = parser::build_checked(src);
    let root = SyntaxNode::new_root(green);

    if let Some(err) = errors.iter().min_by_key(|e| e.offset) {
        let (line, col) = line_col(src, err.offset);
        let mut msg = format!("invalid JSONC at line {line}, column {col}: {}", err.msg);
        if let Some(open) = err.opened {
            let (l, c) = line_col(src, open);
            let bracket = &src[open..open + 1];
            msg.push_str(&format!(" (unclosed `{bracket}` at line {l}, column {c})"));
        }
        return Err(ParseError { msg });
    }

    if !top_value_present(&root) {
        return Err(ParseError {
            msg: "invalid JSONC: no value found".to_string(),
        });
    }

    let json5 = detect_json5(&root);
    Ok(Jsonc { root, json5, bom })
}

/// Does the source use a spelling that strict JSON forbids? That is the JSON5
/// test: unquoted keys (`Ident`), single-quoted strings, `+`/hex/leading-dot
/// numbers, `Infinity`/`NaN`, and backslash-newline string continuations.
/// Comments and trailing commas are JSONC, not JSON5, and do not count - so a
/// commented-but-otherwise-strict `.jsonc` still refuses a non-finite insert
/// rather than writing bytes VS Code-style consumers reject.
fn detect_json5(root: &SyntaxNode) -> bool {
    edikt_syntax::tokens(root).any(|t| match t.kind() {
        Sk::SingleStr | Sk::Ident => true,
        Sk::Str => t.text().to_string().contains("\\\n"),
        Sk::Num => {
            let s = t.text().to_string();
            let u = if s.starts_with('+') || s.starts_with('-') {
                &s[1..]
            } else {
                &s
            };
            s.starts_with('+')
                || u.starts_with("0x")
                || u.starts_with("0X")
                || u.starts_with('.')
                || u.ends_with('.')
                || s.ends_with("Infinity")
                || s == "NaN"
        }
        _ => false,
    })
}

/// The 1-based line and column (in characters) of byte `offset` in `src`.
fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let before = &src[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = edikt_core::text::line_start(src, offset);
    (line, before[line_start..].chars().count() + 1)
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
        edikt_core::text::with_bom(self.bom, edikt_syntax::to_source(&self.root))
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
        edikt_syntax::tokens(&self.root)
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
mod tests;
