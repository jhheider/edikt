//! Folding new `>` text to the width a file fills its prose to (#108).
//!
//! A folded scalar's line break between two lines of text reads as a space,
//! so the source's line breaks there are layout, and a file that wraps its
//! prose at, say, 100 columns wants new text wrapped the same way. The
//! width is inferred, not configured: from every `>` scalar in the file,
//! and only when the text shows it was filled greedily to one width.

use crate::block::BlockScalar;
use crate::compose::{Node, NodeKind};

/// Below this width (chars, indent included) the evidence is too thin to
/// fold at: a short value that happens to wrap (`old` / `text`) is not a
/// file's prose width.
pub(crate) const MIN_WIDTH: usize = 40;

/// What one `>` scalar shows of the width its text was filled to.
struct Fill {
    /// Its longest text line, in chars, indent included.
    longest: usize,
    /// For each folded break: the width of the line before it (indent
    /// included) and of the first word on the line after it.
    breaks: Vec<(usize, usize)>,
}

impl Fill {
    /// The folded breaks of `block`, whose content indent is `indent`, or
    /// `None` when it has none (its text never wrapped).
    fn of(block: &BlockScalar, source: &str, indent: usize) -> Option<Self> {
        let mut longest = 0;
        let mut breaks = Vec::new();
        let mut prev: Option<usize> = None;
        for line in block.content_lines(source) {
            // A text line sits at the indent; a more-indented one (spaces
            // or a tab past it) keeps its line breaks, and a blank one ends
            // the paragraph.
            let text = !line.is_empty()
                && line.len() - line.trim_start_matches(' ').len() == indent
                && !line[indent..].starts_with('\t');
            if !text {
                prev = None;
                continue;
            }
            let width = line.chars().count();
            longest = longest.max(width);
            if let Some(prev) = prev {
                breaks.push((prev, first_word(&line[indent..])));
            }
            prev = Some(width);
        }
        (!breaks.is_empty()).then_some(Self { longest, breaks })
    }
}

/// The width `fills` were filled to: their longest text line, provided no
/// break had room for the next line's first word before it.
fn filled<'a>(fills: impl IntoIterator<Item = &'a Fill> + Clone) -> Option<usize> {
    let width = fills.clone().into_iter().map(|f| f.longest).max()?;
    fills
        .into_iter()
        .flat_map(|f| &f.breaks)
        .all(|&(line, word)| line + 1 + word > width)
        .then_some(width)
}

/// The width to fold new text for `block` (content indent `indent`) at, or
/// `None` to write each line of it as one line. `block`'s own text must
/// have wrapped. The width is then the one every other `>` scalar in `docs`
/// was filled to (`block`'s old layout, about to be replaced, doesn't get
/// a say, so a short value that wrapped can't veto the file's width), or
/// failing that the one `block` alone was, and at least [`MIN_WIDTH`].
pub(crate) fn width(
    source: &str,
    docs: &[Node],
    block: &BlockScalar,
    indent: usize,
) -> Option<usize> {
    let own = Fill::of(block, source, indent)?;
    let mut others = Vec::new();
    for doc in docs {
        collect(source, doc, block.header.start, &mut others);
    }
    let sane = |w: &usize| *w >= MIN_WIDTH;
    filled(&others)
        .filter(sane)
        .or_else(|| filled([&own]).filter(sane))
}

/// Every `>` scalar under `node` with a folded break, as evidence, but the
/// one whose header starts at `skip`.
fn collect(source: &str, node: &Node, skip: usize, out: &mut Vec<Fill>) {
    match &node.kind {
        NodeKind::Scalar(_) => {
            if let Some(block) = BlockScalar::of(source, node)
                && block.header.start != skip
                && block.is_folded(source)
                && let Some(indent) = block.seen_indent(source)
                && let Some(fill) = Fill::of(&block, source, indent)
            {
                out.push(fill);
            }
        }
        NodeKind::Sequence(items) => items.iter().for_each(|n| collect(source, n, skip, out)),
        NodeKind::Mapping(entries) => entries
            .iter()
            .for_each(|e| collect(source, &e.value, skip, out)),
    }
}

/// The spaces in `text` a folded line may break at: a single space between
/// two non-blank chars, which `>` folds back to that space. A run of spaces,
/// a tab, or a space at either end is never one.
fn break_points(text: &str) -> impl Iterator<Item = usize> + '_ {
    let blank = |c: Option<char>| c.is_none_or(|c| c == ' ' || c == '\t');
    text.char_indices()
        .filter(move |&(i, c)| {
            c == ' '
                && !blank(text[..i].chars().next_back())
                && !blank(text[i + 1..].chars().next())
        })
        .map(|(i, _)| i)
}

/// The width in chars of the first word of `text`: up to its first break
/// point.
fn first_word(text: &str) -> usize {
    let end = break_points(text).next().unwrap_or(text.len());
    text[..end].chars().count()
}

/// `text`, one line of a folded scalar, broken greedily into lines of at
/// most `room` chars where it can be, only at its break points, so a word
/// longer than `room` gets a line of its own rather than being split.
pub(crate) fn fold_line(text: &str, room: usize) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut line = 0; // where the current line starts
    let mut word = 0; // where the next word starts
    let mut width = 0; // the current line's width in chars
    for end in break_points(text).chain(std::iter::once(text.len())) {
        let chars = text[word..end].chars().count();
        if word > line && width + 1 + chars > room {
            lines.push(&text[line..word - 1]);
            line = word;
            width = chars;
        } else {
            width += usize::from(word > line) + chars;
        }
        word = end + 1;
    }
    lines.push(&text[line..]);
    lines
}
