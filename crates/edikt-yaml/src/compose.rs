//! Compose libyaml-safer's event stream into a **span tree**: the document
//! structure with every scalar/collection's byte range in the original source.
//!
//! This is the whole trick. One parse pass yields both the data model (fold the
//! tree to a [`Value`] for query/convert) and the byte marks the lossless splice
//! rides on (edit a node -> replace exactly its bytes, leave every other byte
//! untouched). No separate CST, no second parser.

use edikt_core::Value;
use libyaml_safer::{EventData, Parser, ScalarStyle};
use std::collections::HashMap;
use std::ops::Range;

use crate::scalar::resolve_scalar;

/// A node in the span tree: its byte range in the source plus its shape.
pub(crate) struct Node {
    /// The node's byte span in the source. For a scalar this is exactly the
    /// scalar text (quotes included); for a collection it spans the whole block.
    pub span: Range<usize>,
    pub kind: NodeKind,
}

pub(crate) enum NodeKind {
    Scalar(Value),
    Sequence(Vec<Node>),
    Mapping(Vec<Entry>),
}

/// One key/value pair of a mapping, with the key's own byte span (needed to
/// delete or insert around the entry line).
pub(crate) struct Entry {
    pub key: String,
    pub key_span: Range<usize>,
    pub value: Node,
}

/// A libyaml event flattened to just what composition needs: its shape and marks.
struct Ev {
    tok: Tok,
    start: usize,
    end: usize,
}

enum Tok {
    StreamStart,
    StreamEnd,
    DocStart,
    DocEnd,
    Alias(String),
    Scalar {
        value: String,
        style: ScalarStyle,
        tag: Option<String>,
        anchor: Option<String>,
    },
    SeqStart(Option<String>),
    SeqEnd,
    MapStart(Option<String>),
    MapEnd,
}

/// Scan `src` into the flat event list (with byte marks), or a parse error string.
fn parse_events(src: &str) -> Result<Vec<Ev>, String> {
    let mut bytes = src.as_bytes();
    let mut parser = Parser::new();
    parser.set_input(&mut bytes);

    let mut evs = Vec::new();
    loop {
        let event = parser.parse().map_err(|e| e.to_string())?;
        let start = event.start_mark.index as usize;
        let end = event.end_mark.index as usize;
        let done = matches!(event.data, EventData::StreamEnd);
        let tok = match event.data {
            EventData::StreamStart { .. } => Tok::StreamStart,
            EventData::StreamEnd => Tok::StreamEnd,
            EventData::DocumentStart { .. } => Tok::DocStart,
            EventData::DocumentEnd { .. } => Tok::DocEnd,
            EventData::Alias { anchor } => Tok::Alias(anchor),
            EventData::Scalar {
                value,
                style,
                tag,
                anchor,
                ..
            } => Tok::Scalar {
                value,
                style,
                tag,
                anchor,
            },
            EventData::SequenceStart { anchor, .. } => Tok::SeqStart(anchor),
            EventData::SequenceEnd => Tok::SeqEnd,
            EventData::MappingStart { anchor, .. } => Tok::MapStart(anchor),
            EventData::MappingEnd => Tok::MapEnd,
        };
        // The whole edit path slices `source[..]` at these marks. Validate them
        // against the source up front so a bad mark (libyaml bug, adversarial
        // input) becomes a clean parse error, never a slice panic. Marks must be
        // in bounds and on UTF-8 char boundaries.
        if start > src.len()
            || end > src.len()
            || !src.is_char_boundary(start)
            || !src.is_char_boundary(end)
        {
            return Err(format!(
                "parser produced an out-of-range span {start}..{end} for a {}-byte document",
                src.len()
            ));
        }
        evs.push(Ev { tok, start, end });
        if done {
            break;
        }
    }
    Ok(evs)
}

/// Parse `src` and compose the first document's span tree. An empty stream (no
/// document, or only comments) composes to a null scalar spanning nothing.
pub(crate) fn compose_source(src: &str) -> Result<Node, String> {
    Ok(compose_all(src)?.pop_first_or_null())
}

/// The composed documents of a YAML stream, in order. A single-document stream
/// yields one; a `---`-separated stream yields one per document.
pub(crate) struct Docs {
    docs: Vec<Node>,
}

impl Docs {
    pub(crate) fn into_vec(self) -> Vec<Node> {
        self.docs
    }
    /// The first document (single-doc callers), or a null node for an empty
    /// stream, preserving the old single-root contract.
    fn pop_first_or_null(mut self) -> Node {
        if self.docs.is_empty() {
            null_node()
        } else {
            self.docs.remove(0)
        }
    }
}

/// A null document (an empty or comment-only stream). Substituted so callers
/// always have at least one document to operate on.
pub(crate) fn null_node() -> Node {
    Node {
        span: 0..0,
        kind: NodeKind::Scalar(Value::Null),
    }
}

/// Compose **every** document in the stream. Each document gets a fresh anchor
/// scope (YAML anchors do not cross document boundaries), and every node's byte
/// marks are absolute offsets into the shared `src`, so per-document editing
/// splices the same source string.
pub(crate) fn compose_all(src: &str) -> Result<Docs, String> {
    let events = parse_events(src)?;
    let mut pos = 0;
    let mut docs = Vec::new();

    while let Some(tok) = events.get(pos).map(|e| &e.tok) {
        match tok {
            Tok::StreamStart | Tok::DocStart => {
                pos += 1;
            }
            Tok::DocEnd => {
                pos += 1;
            }
            Tok::StreamEnd => break,
            _ => {
                // A document's root node. Fresh anchor scope per document.
                let mut anchors: HashMap<String, Value> = HashMap::new();
                docs.push(compose_node(&events, &mut pos, &mut anchors));
            }
        }
    }
    Ok(Docs { docs })
}

/// Compose the node at `*pos`, advancing `pos` past it. Registers anchors so
/// later aliases resolve to the anchored value.
fn compose_node(events: &[Ev], pos: &mut usize, anchors: &mut HashMap<String, Value>) -> Node {
    let start = events[*pos].start;
    let end = events[*pos].end;
    match &events[*pos].tok {
        Tok::Scalar {
            value,
            style,
            tag,
            anchor,
        } => {
            let v = resolve_scalar(value, *style, tag.as_deref());
            let anchor = anchor.clone();
            *pos += 1;
            if let Some(a) = anchor {
                anchors.insert(a, v.clone());
            }
            Node {
                span: start..end,
                kind: NodeKind::Scalar(v),
            }
        }
        Tok::Alias(name) => {
            let v = anchors.get(name).cloned().unwrap_or(Value::Null);
            *pos += 1;
            Node {
                span: start..end,
                kind: NodeKind::Scalar(v),
            }
        }
        Tok::SeqStart(anchor) => {
            let anchor = anchor.clone();
            *pos += 1;
            let mut items = Vec::new();
            while !matches!(events[*pos].tok, Tok::SeqEnd) {
                items.push(compose_node(events, pos, anchors));
            }
            let seq_end = events[*pos].end;
            *pos += 1; // consume SeqEnd
            let node = Node {
                span: start..seq_end,
                kind: NodeKind::Sequence(items),
            };
            if let Some(a) = anchor {
                anchors.insert(a, node_to_value(&node));
            }
            node
        }
        Tok::MapStart(anchor) => {
            let anchor = anchor.clone();
            *pos += 1;
            let mut entries = Vec::new();
            while !matches!(events[*pos].tok, Tok::MapEnd) {
                let key_node = compose_node(events, pos, anchors);
                let key = key_string(&key_node);
                let key_span = key_node.span.clone();
                let value = compose_node(events, pos, anchors);
                entries.push(Entry {
                    key,
                    key_span,
                    value,
                });
            }
            let map_end = events[*pos].end;
            *pos += 1; // consume MapEnd
            let node = Node {
                span: start..map_end,
                kind: NodeKind::Mapping(entries),
            };
            if let Some(a) = anchor {
                anchors.insert(a, node_to_value(&node));
            }
            node
        }
        // Framing tokens never reach here (compose_source positions past them);
        // treat defensively as an empty scalar.
        _ => {
            *pos += 1;
            Node {
                span: start..end,
                kind: NodeKind::Scalar(Value::Null),
            }
        }
    }
}

/// A mapping key rendered to its string form (config keys are strings; a
/// non-string key stringifies so the flat `Value::Object` model stays honest).
fn key_string(node: &Node) -> String {
    node_to_value(node).to_raw_string()
}

/// Fold the span tree to the data-model [`Value`] (drops trivia; that is the
/// query/convert view; the source view lives in the spans).
///
/// Merge keys (`<<: *anchor`) are resolved here, so query/convert see the
/// *effective* config the way yq does: explicit keys win over merged ones, and
/// earlier merges win over later. The physical tree keeps the `<<` entry, so
/// editing still targets real bytes (a merged-in key isn't physically present -
/// setting it creates an explicit override).
pub(crate) fn node_to_value(node: &Node) -> Value {
    match &node.kind {
        NodeKind::Scalar(v) => v.clone(),
        NodeKind::Sequence(items) => Value::Array(items.iter().map(node_to_value).collect()),
        NodeKind::Mapping(entries) => {
            let mut out: Vec<(String, Value)> = Vec::new();
            let mut merged: Vec<(String, Value)> = Vec::new();
            for e in entries {
                if e.key == "<<" {
                    collect_merge(&node_to_value(&e.value), &mut merged);
                } else {
                    out.push((e.key.clone(), node_to_value(&e.value)));
                }
            }
            // Append merged keys that no explicit (or earlier-merged) key already covers.
            for (k, v) in merged {
                if !out.iter().any(|(ek, _)| ek == &k) {
                    out.push((k, v));
                }
            }
            Value::Object(out)
        }
    }
}

/// Flatten a merge target (a mapping, or a sequence of mappings) into `out`,
/// keeping first-seen precedence.
pub(crate) fn collect_merge(value: &Value, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(pairs) => {
            for (k, v) in pairs {
                if !out.iter().any(|(ek, _)| ek == k) {
                    out.push((k.clone(), v.clone()));
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_merge(item, out);
            }
        }
        _ => {} // a non-mapping merge target is ignored (invalid YAML merge)
    }
}
