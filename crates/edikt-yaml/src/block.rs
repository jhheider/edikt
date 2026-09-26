//! Block scalars (`|` literal, `>` folded): where their header, content, and
//! trailing blank lines sit in the source, and how a new string is written
//! into one in place (jhheider/edikt#89).
//!
//! libyaml ends a block scalar's span after the blank lines that follow its
//! content, whatever its chomping. Under strip (`-`) and clip (the default)
//! chomping those lines are not part of the value; they separate the scalar
//! from what comes next, so an insertion goes before them (jhheider/edikt#90)
//! and a new value leaves them where they are. Under keep (`+`) they are the
//! value's trailing newlines.

use std::ops::Range;

use crate::compose::{Node, NodeKind};
use crate::scalar::{needs_escape, split_properties};

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

impl Chomp {
    /// Whether this chomping reads back a value made of `body` (no trailing
    /// line breaks) followed by `breaks` line breaks.
    fn fits(self, body: &str, breaks: usize) -> bool {
        match self {
            Chomp::Strip => breaks == 0,
            Chomp::Clip => (breaks == 1 && !body.is_empty()) || (breaks == 0 && body.is_empty()),
            Chomp::Keep => breaks >= 1 || body.is_empty(),
        }
    }

    fn indicator(self) -> &'static str {
        match self {
            Chomp::Strip => "-",
            Chomp::Clip => "",
            Chomp::Keep => "+",
        }
    }
}

/// The parts of a block scalar in the source.
pub(crate) struct BlockScalar {
    /// The header line from the style indicator to its end (a comment
    /// included), line break excluded.
    pub header: Range<usize>,
    /// The length of the header's indicators: the style character, then an
    /// indentation digit and a chomping sign in either order.
    indicators: usize,
    /// The indentation indicator, if the header has one.
    digit: Option<usize>,
    /// The chomping indicator.
    pub chomp: Chomp,
    /// Where the content lines start (the line after the header) and end:
    /// just past the line break of the last content line, before any
    /// trailing blank lines.
    pub content: Range<usize>,
    /// The column of the first non-blank content line: the content indent
    /// when there is no indentation indicator.
    first_indent: Option<usize>,
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
            .split([' ', '\t', '#'])
            .next()
            .unwrap_or("");
        let chomp = if indicators.contains('-') {
            Chomp::Strip
        } else if indicators.contains('+') {
            Chomp::Keep
        } else {
            Chomp::Clip
        };
        let digit = indicators
            .chars()
            .find_map(|c| c.to_digit(10))
            .map(|d| d as usize);
        let first = source[header_end..]
            .find('\n')
            .map_or(source.len(), |i| header_end + i + 1);
        let end = node.span.end.max(first);
        let lines = || lines_in(source, first..end);
        let text_lines = || lines().filter(|l| !source[l.clone()].trim().is_empty());
        let first_indent = text_lines().next().map(|l| leading_spaces(&source[l]));
        // The content indent is at most the indent of the least indented
        // non-blank line; a whitespace-only line indented past it is content.
        let content_end = match text_lines().map(|l| leading_spaces(&source[l])).min() {
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
            header: start..header_end,
            indicators: 1 + indicators.len(),
            digit,
            chomp,
            content: first..content_end,
            first_indent,
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

    /// The header past its indicators: the spacing and comment after them.
    pub(crate) fn header_rest<'a>(&self, source: &'a str) -> &'a str {
        &source[self.header.start + self.indicators..self.header.end]
    }

    /// Write `value` into this block scalar in place, keeping its style,
    /// content indent, and header. `parent` is the indentation of the
    /// collection holding it (its key's or dash's column; `0` at the root),
    /// which an indentation indicator counts from, and `offset` the file's
    /// nesting width, for a block that has no content to learn an indent
    /// from. The chomping indicator changes only when it can't read back
    /// `value`'s trailing line breaks.
    ///
    /// `None` when `value` can't be spelled as a block scalar here: it holds
    /// a character with no literal spelling, or needs an indentation
    /// indicator that `parent` can't supply. The caller checks the splice
    /// reads back as `value` anyway, since libyaml is the judge.
    pub(crate) fn respell(
        &self,
        source: &str,
        value: &str,
        parent: Option<usize>,
        offset: usize,
        nl: &str,
    ) -> Option<(Range<usize>, String)> {
        if value
            .chars()
            .any(|c| c != '\n' && c != '\t' && needs_escape(c))
        {
            return None;
        }
        let body = value.trim_end_matches('\n');
        let breaks = value.len() - body.len();
        let chomp = if self.chomp.fits(body, breaks) {
            self.chomp
        } else if breaks == 0 {
            Chomp::Strip
        } else if breaks == 1 && !body.is_empty() {
            Chomp::Clip
        } else {
            Chomp::Keep
        };
        let indent = match (self.digit, self.first_indent) {
            (Some(d), _) => parent? + d,
            (None, Some(i)) => i,
            (None, None) => parent? + offset,
        };
        let header = &source[self.header.clone()];
        let style = &header[..1];
        let spaced = |line: &str| line.starts_with([' ', '\t']);
        let first_text = body.split('\n').find(|l| !l.is_empty());
        // Text starting with a space or tab would read as indentation.
        let digit = match self.digit {
            Some(d) => Some(d),
            None if first_text.is_some_and(spaced) => {
                let d = indent.checked_sub(parent?)?;
                if !(1..=9).contains(&d) {
                    return None;
                }
                Some(d)
            }
            None => None,
        };
        let header = if chomp == self.chomp && digit == self.digit {
            header.to_string()
        } else {
            let digit = digit.map(|d| d.to_string()).unwrap_or_default();
            let rest = self.header_rest(source);
            format!("{style}{digit}{}{rest}", chomp.indicator())
        };

        let mut lines: Vec<&str> = Vec::new();
        if !body.is_empty() {
            let folded = style == ">";
            let mut prev: Option<&str> = None;
            let mut empties = 0;
            for segment in body.split('\n') {
                if segment.is_empty() {
                    empties += 1;
                    continue;
                }
                // A folded line break between two lines of text reads as a
                // space, so each break there is written as a blank line;
                // next to a more-indented line, breaks are kept as they are.
                let blank = match prev {
                    Some(p) if folded && !spaced(p) && !spaced(segment) => empties + 1,
                    _ => empties,
                };
                lines.extend(std::iter::repeat_n("", blank));
                lines.push(segment);
                prev = Some(segment);
                empties = 0;
            }
        }
        // Past the last line of text, only `+` spells more line breaks.
        let trailing = if body.is_empty() {
            breaks
        } else {
            breaks.saturating_sub(1)
        };
        if chomp == Chomp::Keep {
            lines.extend(std::iter::repeat_n("", trailing));
        }

        let pad = " ".repeat(indent);
        let mut text = header;
        text.push_str(nl);
        for line in lines {
            if !line.is_empty() {
                text.push_str(&pad);
                text.push_str(line);
            }
            text.push_str(nl);
        }
        // A `+` scalar owns every blank line after it; any other keeps the
        // old one's blank lines after it, as separators.
        let end = match chomp {
            Chomp::Keep => self.end,
            Chomp::Strip | Chomp::Clip => self.content.end,
        };
        Some((self.header.start..end, text))
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
