//! Byte extents in the source: the line a node ends on, where an insertion
//! after it goes, and the original text a structural query returns.

use crate::block::BlockScalar;
use crate::compose::{Node, NodeKind};
use crate::layout::line_start;
use edikt_core::{Step, normalize_index};

/// The newline style the document uses, so inserted lines match it (a lone `\n`
/// spliced into a CRLF file would leave observably mixed line endings): the
/// dominant one, so a mostly-LF file with one stray CRLF stays LF.
pub(crate) fn newline(source: &str) -> &'static str {
    edikt_core::text::dominant(source)
}

/// Whether the source already ends with a line break (`\n`, hence also `\r\n`).
pub(crate) fn ends_with_newline(source: &str) -> bool {
    source.ends_with('\n')
}

/// The byte offset just after the newline that ends the line at/after `end`.
///
/// libyaml lands scalar end-marks mid-line (right after the text) but collection
/// end-marks at the *next* line's start; this normalizes both to "start of the
/// following line" (or EOF).
pub(crate) fn line_after(source: &str, end: usize) -> usize {
    let bytes = source.as_bytes();
    if end == 0 || bytes.get(end - 1) == Some(&b'\n') {
        return end;
    }
    match source[end..].find('\n') {
        Some(i) => end + i + 1,
        None => source.len(),
    }
}

/// `end` with the line break just before it (`\n` or `\r\n`) excluded.
pub(crate) fn trim_newline(source: &str, end: usize) -> usize {
    let s = &source[..end];
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.strip_suffix('\r').unwrap_or(s).len()
}

/// The byte offset just after the last physical line of `node`.
///
/// A collection's own end-mark is unreliable for line math; libyaml lands it on
/// the *next sibling's* text (past that sibling's indent), which would overshoot.
/// So we drill to the node's deepest last scalar and take the line after *it*.
pub(crate) fn block_end(source: &str, node: &Node) -> usize {
    match &node.kind {
        NodeKind::Scalar(_) => line_after(source, node.span.end),
        NodeKind::Sequence(items) => match items.last() {
            Some(last) => block_end(source, last),
            None => line_after(source, node.span.end),
        },
        NodeKind::Mapping(entries) => match entries.last() {
            Some(last) => block_end(source, &last.value),
            None => line_after(source, node.span.end),
        },
    }
}

/// Where a key or item added after `node` goes: [`block_end`], except that a
/// block scalar ending the node leaves its trailing blank lines after the
/// insertion, where they keep separating it from what follows (#90). Only a
/// keep-chomped (`+`) scalar owns those lines, so an insertion goes past them.
pub(crate) fn insert_end(source: &str, node: &Node) -> usize {
    match &node.kind {
        NodeKind::Scalar(_) => {
            BlockScalar::of(source, node).map_or_else(|| block_end(source, node), |b| b.insert_at())
        }
        NodeKind::Sequence(items) => items
            .last()
            .map_or_else(|| block_end(source, node), |n| insert_end(source, n)),
        NodeKind::Mapping(entries) => entries
            .last()
            .map_or_else(|| block_end(source, node), |e| insert_end(source, &e.value)),
    }
}

/// The original source text of each node selected by `path`, in document order
/// (aligned with the evaluator). See [`slice_of`] for the per-node form.
pub(crate) fn source_slices(source: &str, root: &Node, path: &[Step]) -> Vec<String> {
    let mut current: Vec<&Node> = vec![root];
    for step in path {
        let mut next: Vec<&Node> = Vec::new();
        for node in &current {
            match step {
                Step::Field(k) => {
                    if let NodeKind::Mapping(entries) = &node.kind
                        && let Some(e) = entries.iter().find(|e| &e.key == k)
                    {
                        next.push(&e.value);
                    }
                }
                Step::Index(i) => {
                    if let NodeKind::Sequence(items) = &node.kind
                        && let Some(item) =
                            normalize_index(*i, items.len()).and_then(|n| items.get(n))
                    {
                        next.push(item);
                    }
                }
                Step::Iterate => match &node.kind {
                    NodeKind::Sequence(items) => next.extend(items.iter()),
                    NodeKind::Mapping(entries) => next.extend(entries.iter().map(|e| &e.value)),
                    _ => {}
                },
                // A comment addresses no value node; source slices never
                // resolve one (the CLI reads comments via `to_commented`).
                Step::Comment(_) => {}
            }
        }
        current = next;
    }
    current.iter().map(|n| slice_of(source, n)).collect()
}

/// The source form of one node: a scalar or flow collection (`[...]`/`{...}`) is
/// returned verbatim; a block collection is returned as its full-line region,
/// dedented to the left margin so the fragment is valid standalone YAML.
pub(crate) fn slice_of(source: &str, node: &Node) -> String {
    match &node.kind {
        NodeKind::Scalar(_) => source[node.span.clone()].to_string(),
        _ if matches!(source.as_bytes().get(node.span.start), Some(b'[' | b'{')) => {
            source[node.span.clone()].to_string()
        }
        _ => {
            let start = line_start(source, node.span.start);
            let end = block_end(source, node);
            dedent(source[start..end].trim_end_matches(['\n', '\r']))
        }
    }
}

/// Strip the common leading whitespace of every non-blank line.
pub(crate) fn dedent(text: &str) -> String {
    let min = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    text.lines()
        .map(|l| if l.len() >= min { &l[min..] } else { l })
        .collect::<Vec<_>>()
        .join("\n")
}
