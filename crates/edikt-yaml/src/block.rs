//! Block scalars (`|` literal, `>` folded): where their header, content, and
//! trailing blank lines sit in the source.
//!
//! libyaml ends a block scalar's span after the blank lines that follow its
//! content, whatever its chomping. Under strip (`-`) and clip (the default)
//! chomping those lines are not part of the value; they separate the scalar
//! from what comes next, so an insertion goes before them (jhheider/edikt#90).
//! Under keep (`+`) they are the value's trailing newlines.

use std::ops::Range;

use crate::compose::{Node, NodeKind};
use crate::scalar::split_properties;

/// How a block scalar treats its trailing line breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Chomp {
    /// `-`: no trailing line break.
    Strip,
    /// No indicator: one trailing line break.
    Clip,
    /// `+`: every trailing line break, blank lines included.
    Keep,
}

/// The parts of a block scalar in the source.
pub(crate) struct BlockScalar {
    /// The chomping indicator.
    pub chomp: Chomp,
    /// Where the content lines start (the line after the header) and end:
    /// just past the line break of the last content line, before any
    /// trailing blank lines.
    pub content: Range<usize>,
    /// The end of the scalar's span, trailing blank lines included.
    pub end: usize,
}

impl BlockScalar {
    /// The block scalar `node` is, or `None` for any other node.
    pub(crate) fn of(source: &str, node: &Node) -> Option<Self> {
        if !matches!(node.kind, NodeKind::Scalar(_)) {
            return None;
        }
        let token = &source[node.span.clone()];
        let (props, body) = split_properties(token);
        if !body.starts_with(['|', '>']) {
            return None;
        }
        let start = node.span.start + props.len();
        let header_end = start + body.find(['\r', '\n']).unwrap_or(body.len());
        let indicators = source[start + 1..header_end]
            .split([' ', '\t'])
            .next()
            .unwrap_or("");
        let chomp = if indicators.contains('-') {
            Chomp::Strip
        } else if indicators.contains('+') {
            Chomp::Keep
        } else {
            Chomp::Clip
        };
        let first = source[header_end..]
            .find('\n')
            .map_or(source.len(), |i| header_end + i + 1);
        let end = node.span.end.max(first);
        // The content indent is at most the indent of the least indented
        // non-blank line; a whitespace-only line indented past it is content.
        let lines = || lines_in(source, first..end);
        let indent = lines()
            .filter(|l| !source[l.clone()].trim().is_empty())
            .map(|l| leading_spaces(&source[l]))
            .min();
        let content_end = match indent {
            None => first,
            Some(indent) => lines()
                .filter(|l| {
                    let text = source[l.clone()].trim_end_matches(['\r', '\n']);
                    !text.trim().is_empty() || leading_spaces(text) > indent
                })
                .last()
                .map_or(first, |l| l.end),
        };
        Some(Self {
            chomp,
            content: first..content_end,
            end,
        })
    }

    /// Where a key or item added after this scalar goes: past its content,
    /// before the blank lines that separate it from what follows, unless it
    /// keeps those lines as part of its value.
    pub(crate) fn insert_at(&self) -> usize {
        match self.chomp {
            Chomp::Keep => self.end,
            Chomp::Strip | Chomp::Clip => self.content.end,
        }
    }
}

/// The lines in `range`, each with its line break.
fn lines_in(source: &str, range: Range<usize>) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut at = range.start;
    std::iter::from_fn(move || {
        if at >= range.end {
            return None;
        }
        let end = source[at..range.end]
            .find('\n')
            .map_or(range.end, |i| at + i + 1);
        let line = at..end;
        at = end;
        Some(line)
    })
}

/// How many spaces `line` starts with.
fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}
