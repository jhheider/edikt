//! Laying out a **new** mapping or sequence in the file's own style
//! (jhheider/edikt#83).
//!
//! Replacing a value or adding a key splices fresh bytes into the source, and
//! a collection needs a layout: block or flow, and, for block, how deep each
//! level indents. Nothing here reads the bytes an edit replaces; it reads the
//! rest of the document for its conventions, so a new block looks like its
//! neighbours: a two-space file gets two-space levels, a four-space file four,
//! and a file that writes `key:\n- item` (sequences at the key's own column)
//! keeps doing so.

use edikt_core::{EditError, Value};

use crate::compose::{Node, NodeKind};
use crate::scalar::{
    QuoteStyle, emit_key, emit_scalar_inline, emit_scalar_styled, split_properties,
};

/// A document's block indentation, inferred from how its existing blocks nest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Indent {
    /// Columns a nested mapping sits past its parent key.
    pub unit: usize,
    /// Columns a block sequence's `-` sits past the key that owns it: `0` for
    /// the indentless `key:\n- item` style, usually `unit` otherwise.
    pub seq: usize,
}

impl Indent {
    /// The file's convention: the most common offset among its existing
    /// mapping-in-mapping and sequence-under-key blocks, earliest first on a
    /// tie. A document with nothing nested to learn from gets two spaces and
    /// indented sequences, the common YAML house style.
    pub(crate) fn infer(source: &str, docs: &[Node]) -> Self {
        let mut units = Vec::new();
        let mut seqs = Vec::new();
        for doc in docs {
            sample(source, doc, &mut units, &mut seqs);
        }
        let unit = mode(&units)
            .or_else(|| mode(&seqs.iter().copied().filter(|&s| s > 0).collect::<Vec<_>>()))
            .unwrap_or(2);
        let seq = mode(&seqs).unwrap_or(unit);
        Self { unit, seq }
    }
}

/// Collect the indentation offsets of every block collection under a key.
fn sample(source: &str, node: &Node, units: &mut Vec<usize>, seqs: &mut Vec<usize>) {
    match &node.kind {
        NodeKind::Scalar(_) => {}
        NodeKind::Sequence(items) => {
            for item in items {
                sample(source, item, units, seqs);
            }
        }
        NodeKind::Mapping(entries) => {
            for e in entries {
                if !is_flow(source, &e.value)
                    && let Some(key_col) = indent_column(source, e.key_span.start)
                {
                    match &e.value.kind {
                        NodeKind::Mapping(_) => {
                            if let Some(col) =
                                indent_column(source, content_start(source, &e.value))
                                && col > key_col
                            {
                                units.push(col - key_col);
                            }
                        }
                        NodeKind::Sequence(_) => {
                            if let Some(col) =
                                indent_column(source, content_start(source, &e.value))
                                && col >= key_col
                            {
                                seqs.push(col - key_col);
                            }
                        }
                        NodeKind::Scalar(_) => {}
                    }
                }
                sample(source, &e.value, units, seqs);
            }
        }
    }
}

/// The most frequent value, the earliest seen on a tie.
fn mode(xs: &[usize]) -> Option<usize> {
    // Distinct offsets are few (a file uses one or two), so a small table in
    // first-seen order is enough.
    let mut counts: Vec<(usize, usize)> = Vec::new();
    for &x in xs {
        match counts.iter_mut().find(|(v, _)| *v == x) {
            Some((_, n)) => *n += 1,
            None => counts.push((x, 1)),
        }
    }
    let mut best: Option<(usize, usize)> = None;
    for (x, n) in counts {
        if best.is_none_or(|(_, bn)| n > bn) {
            best = Some((x, n));
        }
    }
    best.map(|(x, _)| x)
}

/// The column of `pos` when everything before it on its line is indentation
/// or block-sequence dashes (`  - - key`), else `None`: a position after
/// other content (a flow collection, a `? ` key) says nothing about indent.
fn indent_column(source: &str, pos: usize) -> Option<usize> {
    let prefix = &source[line_start(source, pos)..pos];
    prefix
        .bytes()
        .all(|b| b == b' ' || b == b'-')
        .then_some(prefix.len())
}

/// The column (in characters) of byte `pos` on its line.
pub(crate) fn column(source: &str, pos: usize) -> usize {
    source[line_start(source, pos)..pos].chars().count()
}

/// The byte offset of the start of the line containing `pos`.
pub(crate) fn line_start(source: &str, pos: usize) -> usize {
    source[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// The byte offset where the line containing `pos` ends: its `\n`, or the
/// `\r` of a `\r\n`, or EOF.
pub(crate) fn line_end(source: &str, pos: usize) -> usize {
    let end = source[pos..].find('\n').map_or(source.len(), |i| pos + i);
    if end > pos && source.as_bytes()[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

/// Is `node` a flow collection (`[...]`/`{...}`), anchor or tag aside?
pub(crate) fn is_flow(source: &str, node: &Node) -> bool {
    !matches!(node.kind, NodeKind::Scalar(_))
        && split_properties(&source[node.span.clone()])
            .1
            .starts_with(['[', '{'])
}

/// Where a node's own content begins: past its anchor/tag, and for a block
/// collection, at its first key or first `-` (a property can sit on the line
/// above, and libyaml starts the node's span there).
pub(crate) fn content_start(source: &str, node: &Node) -> usize {
    match &node.kind {
        NodeKind::Mapping(entries) if !is_flow(source, node) => entries
            .first()
            .map_or(node.span.start, |e| e.key_span.start),
        NodeKind::Sequence(_) if !is_flow(source, node) => seq_dash(source, node),
        _ => {
            let (props, _) = split_properties(&source[node.span.clone()]);
            node.span.start + props.len()
        }
    }
}

/// The first `-` of a block sequence, past any properties and comment lines
/// between them and the first item.
fn seq_dash(source: &str, node: &Node) -> usize {
    let (props, _) = split_properties(&source[node.span.clone()]);
    let mut pos = node.span.start + props.len();
    let bytes = source.as_bytes();
    while pos < node.span.end {
        match bytes[pos] {
            b'-' => return pos,
            b'#' => pos = line_end(source, pos),
            _ => pos += 1,
        }
    }
    node.span.start
}

/// The inline spelling of `value` when it has one in block context: a scalar,
/// or an empty collection (`[]`/`{}`, the only way YAML spells those). A
/// non-empty collection needs [`block_lines`] and yields `None`.
pub(crate) fn inline_text(value: &Value) -> Option<Result<String, EditError>> {
    match value {
        Value::Array(a) if a.is_empty() => Some(Ok("[]".into())),
        Value::Object(o) if o.is_empty() => Some(Ok("{}".into())),
        Value::Array(_) | Value::Object(_) => None,
        scalar => Some(emit_scalar_inline(scalar)),
    }
}

/// `value` as a single-line flow node (`[a, b]`, `{k: v}`), for a slot inside
/// a flow collection or one that already was a flow collection.
pub(crate) fn flow_text(value: &Value) -> Result<String, EditError> {
    Ok(match value {
        Value::Array(items) => {
            let items: Result<Vec<_>, _> = items.iter().map(flow_text).collect();
            format!("[{}]", items?.join(", "))
        }
        Value::Object(entries) => {
            let mut parts = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                parts.push(format!(
                    "{}: {}",
                    flow_scalar(&Value::Str(k.clone()))?,
                    flow_text(v)?
                ));
            }
            format!("{{{}}}", parts.join(", "))
        }
        scalar => flow_scalar(scalar)?,
    })
}

/// A scalar spelled for flow context, where `,[]{}` would end a plain scalar.
fn flow_scalar(value: &Value) -> Result<String, EditError> {
    emit_scalar_styled(value, QuoteStyle::Plain, true, "")
}

/// Render a non-empty collection as block lines whose content starts at
/// column `col`, each line carrying its full indentation. A sequence item
/// holding a collection opens on its dash line (`- key: v`, `- - x`), the
/// compact form, so nested items sit two columns past their dash.
pub(crate) fn block_lines(
    value: &Value,
    col: usize,
    indent: Indent,
    out: &mut Vec<String>,
) -> Result<(), EditError> {
    let pad = " ".repeat(col);
    match value {
        Value::Object(entries) => {
            for (k, v) in entries {
                let key = emit_key(k);
                match inline_text(v) {
                    Some(text) => out.push(format!("{pad}{key}: {}", text?)),
                    None => {
                        out.push(format!("{pad}{key}:"));
                        block_lines(v, col + child_offset(v, indent), indent, out)?;
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match inline_text(item) {
                    Some(text) => out.push(format!("{pad}- {}", text?)),
                    None => {
                        let first = out.len();
                        block_lines(item, col + 2, indent, out)?;
                        out[first] = format!("{pad}- {}", &out[first][col + 2..]);
                    }
                }
            }
        }
        scalar => out.push(format!("{pad}{}", emit_scalar_inline(scalar)?)),
    }
    Ok(())
}

/// How far a block value sits past the key that owns it.
pub(crate) fn child_offset(value: &Value, indent: Indent) -> usize {
    match value {
        Value::Array(_) => indent.seq,
        _ => indent.unit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::compose_all;
    use edikt_core::json;

    fn infer(src: &str) -> Indent {
        Indent::infer(src, &compose_all(src).unwrap().into_vec())
    }

    fn lines(value: &Value, col: usize, indent: Indent) -> String {
        let mut out = Vec::new();
        block_lines(value, col, indent, &mut out).unwrap();
        out.join("\n")
    }

    #[test]
    fn infers_unit_and_sequence_offset() {
        assert_eq!(infer("a:\n  b: 1\n"), Indent { unit: 2, seq: 2 });
        assert_eq!(infer("a:\n    b: 1\n"), Indent { unit: 4, seq: 4 });
        assert_eq!(infer("a:\n    b:\n    - 1\n"), Indent { unit: 4, seq: 0 });
        // Only sequences to learn from: their offset is the unit too.
        assert_eq!(infer("a:\n   - 1\n"), Indent { unit: 3, seq: 3 });
        // Nothing nested: the two-space default.
        assert_eq!(infer("a: 1\n"), Indent { unit: 2, seq: 2 });
        // The majority wins over the first sample.
        assert_eq!(
            infer("a:\n    x: 1\nb:\n  y: 1\nc:\n  z: 1\n"),
            Indent { unit: 2, seq: 2 }
        );
        // A key inside a compact sequence item measures from its own column.
        assert_eq!(infer("- a:\n      b: 1\n"), Indent { unit: 4, seq: 4 });
        // Flow collections and a property on the key line don't mislead it.
        assert_eq!(
            infer("f: {x: 1}\na: &x\n   b: 1\n"),
            Indent { unit: 3, seq: 3 }
        );
    }

    #[test]
    fn renders_nested_blocks_compactly() {
        let two = Indent { unit: 2, seq: 2 };
        let v = json!({"a": [1, {"b": "x", "c": [true]}, [2, 3]], "d": {}, "e": []});
        assert_eq!(
            lines(&v, 0, two),
            "a:\n  - 1\n  - b: x\n    c:\n      - true\n  - - 2\n    - 3\nd: {}\ne: []"
        );
        // Indentless sequences sit at their key's column.
        let flat = Indent { unit: 4, seq: 0 };
        assert_eq!(
            lines(&json!({"k": {"l": ["x"]}}), 2, flat),
            "  k:\n      l:\n      - x"
        );
    }

    #[test]
    fn flow_text_quotes_flow_indicators() {
        assert_eq!(
            flow_text(&json!({"a b": ["x,y", 1, null], "c": {}})).unwrap(),
            r#"{a b: ["x,y", 1, null], c: {}}"#
        );
    }
}
