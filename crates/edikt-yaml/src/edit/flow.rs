//! Deleting one element of a flow collection (`[...]`/`{...}`) in place
//! (jhheider/edikt#111).
//!
//! The element goes with one separator and the collection keeps its layout:
//! `[1, 2, 3]` loses `2, ` or `, 3`, a trailing comma stays trailing, and in a
//! one-element-per-line collection the element's line goes (with the comment
//! on it) while comment lines around it stay. The only element leaves `[]` or
//! `{}`, unless comments of their own sit inside, which stay.
//!
//! The text between elements holds only whitespace, commas and comments, so
//! it can be scanned without a YAML lexer; anything else there (a `?`
//! explicit key, say) is refused rather than guessed at.

use super::extent::line_after;
use crate::compose::{Entry, Node, NodeKind};
use crate::layout::{is_flow, line_end, line_start};
use crate::scalar::split_properties;
use edikt_core::EditError;
use std::ops::Range;

/// The splice that deletes element `pos` of the flow collection `node`.
pub(crate) fn delete(
    source: &str,
    node: &Node,
    pos: usize,
) -> Result<(Range<usize>, String), EditError> {
    let refuse =
        || EditError::new("cannot delete from this flow collection in place (unexpected layout)");
    let (props, _) = split_properties(&source[node.span.clone()]);
    let open = node.span.start + props.len();
    let close = node.span.end.checked_sub(1).ok_or_else(refuse)?;
    let pair = match source.as_bytes()[open] {
        b'[' => b']',
        b'{' => b'}',
        _ => return Err(refuse()),
    };
    if source.as_bytes()[close] != pair {
        return Err(refuse());
    }
    let elems = elements(source, node);
    let n = elems.len();
    // The gaps around the elements, each holding its commas: none before
    // the first, one between two, at most one (trailing) after the last.
    let mut gaps = Vec::with_capacity(n + 1);
    let mut from = open + 1;
    for (i, e) in elems.iter().chain([&(close..close)]).enumerate() {
        let gap = scan(source, from, e.start).ok_or_else(refuse)?;
        let fits = match i {
            0 => gap.is_empty(),
            i if i == n => gap.len() <= 1,
            _ => gap.len() == 1,
        };
        if !fits {
            return Err(refuse());
        }
        gaps.push(gap);
        from = e.end;
    }
    let Range { start, end } = elems[pos].clone();
    // The comma that follows the element: the separator, or a trailing one.
    let comma = gaps[pos + 1].first().copied();
    let through = comma.map_or(end, |c| c + 1);
    let own_line = source[line_start(source, start)..start].trim().is_empty();
    let lines = own_line && rest_blank(source, through);
    let line_span = || line_start(source, start)..line_after(source, through);

    if n == 1 {
        let range = if lines { line_span() } else { start..through };
        let left = format!(
            "{}{}",
            &source[open + 1..range.start],
            &source[range.end..close]
        );
        return Ok(if left.contains('#') {
            (range, String::new())
        } else {
            (open + 1..close, String::new())
        });
    }
    if pos + 1 < n {
        let after = through;
        if own_line {
            if rest_blank(source, after) {
                return Ok((line_span(), String::new()));
            }
            return Ok((start..skip_spaces(source, after), String::new()));
        }
        if pos > 0 {
            // ` x,` after the previous comma, which stays with its element.
            let prev = gaps[pos][0];
            return Ok((prev + 1..after, String::new()));
        }
        // First on the opening bracket's line: `[x, ` goes, but spaces
        // before a comment stay (`[ # c`), since `[#` would not parse.
        let rest = &source[after..line_end(source, after)];
        let end = if rest.trim_start().is_empty() || rest.trim_start().starts_with('#') {
            after
        } else {
            skip_spaces(source, after)
        };
        return Ok((start..end, String::new()));
    }
    // The last of several: its separator is the previous comma.
    let prev = gaps[pos][0];
    let trailing = comma.is_some();
    if lines {
        if trailing {
            // The previous comma becomes the trailing one.
            return Ok((line_span(), String::new()));
        }
        // Drop the previous comma too, keeping whatever followed it (a
        // comment) up to the deleted line.
        let keep = &source[prev + 1..line_start(source, start)];
        return Ok((prev..line_after(source, through), keep.to_string()));
    }
    // `, x` goes, with any padding before the comma (`[ x , y ]`); a
    // trailing comma after it stays, as does a comment between the comma
    // and the element.
    let between = &source[prev + 1..start];
    let keep = if between.contains('#') {
        between.trim_end_matches([' ', '\t']).to_string()
    } else {
        String::new()
    };
    // Not into the previous element: an implicit null's `b: ` ends after
    // its space.
    let from = source[..prev]
        .trim_end_matches([' ', '\t'])
        .len()
        .max(elems[pos - 1].end);
    Ok((from..end, keep))
}

/// Where a node inside a flow collection ends: its closing bracket, its
/// scalar's last byte, or, for a single-pair `[k: v]` mapping (whose span
/// libyaml runs on past the comma), the end of its value.
pub(crate) fn content_end(source: &str, node: &Node) -> usize {
    match &node.kind {
        NodeKind::Mapping(entries) if !is_flow(source, node) => entries
            .iter()
            .map(|e| entry_end(source, e))
            .max()
            .unwrap_or(node.span.end),
        _ => node.span.end,
    }
}

/// Where a mapping entry ends: its value's end, or its key's when the value
/// is an implicit null.
fn entry_end(source: &str, e: &Entry) -> usize {
    e.key_span.end.max(content_end(source, &e.value))
}

/// Each element's byte range: a sequence item's text, or a mapping entry
/// from its key to the end of its value.
fn elements(source: &str, node: &Node) -> Vec<Range<usize>> {
    match &node.kind {
        NodeKind::Sequence(items) => items
            .iter()
            .map(|n| n.span.start..content_end(source, n))
            .collect(),
        NodeKind::Mapping(entries) => entries
            .iter()
            .map(|e| e.key_span.start..entry_end(source, e))
            .collect(),
        NodeKind::Scalar(_) => Vec::new(),
    }
}

/// Where the commas in `from..to` are, if it holds only whitespace, commas,
/// and comments.
fn scan(source: &str, from: usize, to: usize) -> Option<Vec<usize>> {
    if from > to {
        return None;
    }
    let bytes = source.as_bytes();
    let mut commas = Vec::new();
    let mut i = from;
    while i < to {
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b',' => {
                commas.push(i);
                i += 1;
            }
            b'#' => i = line_end(source, i).min(to),
            _ => return None,
        }
    }
    Some(commas)
}

/// Whether the rest of the line from `at` is blank or a comment.
fn rest_blank(source: &str, at: usize) -> bool {
    let rest = source[at..line_end(source, at)].trim_start();
    rest.is_empty() || rest.starts_with('#')
}

/// `at` moved past spaces and tabs.
fn skip_spaces(source: &str, at: usize) -> usize {
    let rest = &source[at..];
    at + rest.len() - rest.trim_start_matches([' ', '\t']).len()
}
