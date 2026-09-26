//! The format-preserving get: a structural query's result as the file's own
//! text rather than a re-emit (`Document::source_slice`, jhheider/edikt#101).
//!
//! TOML spreads one table over the file: a `[a]` header's own `key = value`
//! lines, then any number of `[a.b]` / `[[a.c]]` sub-table sections, possibly
//! interleaved with unrelated tables. So a table's slice is its own body
//! followed by each descendant section in source order, every byte verbatim
//! except that a descendant's header is **re-rooted** (the selected table's
//! own keys dropped from its front) so the fragment stands alone as a TOML
//! document for the selected value.
//!
//! Anything that isn't a standalone document (an array, an inline table, a
//! whole array of tables) or can't be cut out with header re-rooting alone (a
//! dotted-key table, whose lines carry its key as a prefix) is not sliced; the
//! caller then emits the value instead.

use edikt_core::{Step, resolve_index, text};
use toml_edit::{ArrayOfTables, ImDocument, Item, Key, Table, Value as TomlValue};

/// The source text of each node `path` selects, in evaluator order, or empty
/// when any selected structural node can't be sliced (see the module docs).
/// A scalar's entry is its value text, a placeholder the caller never prints.
pub(crate) fn source_slices(src: &str, path: &[Step]) -> Vec<String> {
    let Ok(doc) = ImDocument::parse(src) else {
        return Vec::new();
    };
    let mut headers = Vec::new();
    collect_headers(doc.as_table(), &mut Vec::new(), &mut headers);
    headers.sort_by_key(|h| h.start);
    let file = File {
        src,
        headers,
        eol: text::dominant(src),
    };
    let mut slices = Vec::new();
    for node in select(doc.as_table(), path) {
        match file.slice(&node) {
            Some(s) => slices.push(s),
            None => return Vec::new(),
        }
    }
    slices
}

/// One `[table]` or `[[array.element]]` header line, and the section it opens.
struct Header {
    /// The logical path of the table it defines (an array-of-tables element
    /// as `Field` then `Index`).
    path: Vec<Step>,
    /// Byte offset of its opening `[`.
    start: usize,
    /// Where its section begins: the start of the comments and blank lines
    /// before the header (`toml_edit` gives those to the header, as its head
    /// comment).
    prefix_start: usize,
}

/// Every explicit header under `table`, with its logical path.
fn collect_headers(table: &Table, path: &mut Vec<Step>, out: &mut Vec<Header>) {
    if table.position().is_some()
        && !table.is_implicit()
        && !table.is_dotted()
        && let Some(span) = table.span()
    {
        let prefix_start = table
            .decor()
            .prefix()
            .and_then(|p| p.span())
            .map_or(span.start, |p| p.start);
        out.push(Header {
            path: path.clone(),
            start: span.start,
            prefix_start,
        });
    }
    for (key, item) in table.iter() {
        path.push(Step::Field(key.to_string()));
        match item {
            Item::Table(sub) => collect_headers(sub, path, out),
            Item::ArrayOfTables(aot) => {
                for (i, element) in aot.iter().enumerate() {
                    path.push(Step::Index(i as i64));
                    collect_headers(element, path, out);
                    path.pop();
                }
            }
            Item::Value(_) | Item::None => {}
        }
        path.pop();
    }
}

/// A node a path step can land on. Tables carry their logical path, which is
/// how their descendant headers are found.
enum Node<'a> {
    /// The root, a `[header]` table, an implicit or dotted one, or an
    /// array-of-tables element.
    Table(&'a Table, Vec<Step>),
    Tables(&'a ArrayOfTables, Vec<Step>),
    Value(&'a TomlValue),
}

impl<'a> Node<'a> {
    fn of(item: &'a Item, path: Vec<Step>) -> Option<Self> {
        match item {
            Item::Table(t) => Some(Node::Table(t, path)),
            Item::ArrayOfTables(a) => Some(Node::Tables(a, path)),
            Item::Value(v) => Some(Node::Value(v)),
            Item::None => None,
        }
    }

    fn element(aot: &'a ArrayOfTables, path: &[Step], i: usize) -> Option<Self> {
        let mut p = path.to_vec();
        p.push(Step::Index(i as i64));
        aot.get(i).map(|t| Node::Table(t, p))
    }
}

/// The nodes `path` selects, aligned with the evaluator over the projection
/// (same iteration order, same index resolution).
fn select<'a>(root: &'a Table, path: &[Step]) -> Vec<Node<'a>> {
    let mut current = vec![Node::Table(root, Vec::new())];
    for step in path {
        let mut next = Vec::new();
        for node in &current {
            match (step, node) {
                (Step::Field(k), Node::Table(t, p)) => {
                    let mut p = p.clone();
                    p.push(Step::Field(k.clone()));
                    next.extend(t.get(k).and_then(|item| Node::of(item, p)));
                }
                (Step::Field(k), Node::Value(TomlValue::InlineTable(t))) => {
                    next.extend(t.get(k).map(Node::Value));
                }
                (Step::Index(i), Node::Tables(a, p)) => {
                    next.extend(resolve_index(*i, a.len()).and_then(|n| Node::element(a, p, n)));
                }
                (Step::Index(i), Node::Value(TomlValue::Array(a))) => {
                    next.extend(
                        resolve_index(*i, a.len())
                            .and_then(|n| a.get(n))
                            .map(Node::Value),
                    );
                }
                (Step::Iterate, Node::Table(t, p)) => {
                    for (k, item) in t.iter() {
                        let mut p = p.clone();
                        p.push(Step::Field(k.to_string()));
                        next.extend(Node::of(item, p));
                    }
                }
                (Step::Iterate, Node::Tables(a, p)) => {
                    next.extend((0..a.len()).filter_map(|n| Node::element(a, p, n)));
                }
                (Step::Iterate, Node::Value(TomlValue::Array(a))) => {
                    next.extend(a.iter().map(Node::Value));
                }
                (Step::Iterate, Node::Value(TomlValue::InlineTable(t))) => {
                    next.extend(t.iter().map(|(_, v)| Node::Value(v)));
                }
                // A comment step addresses no value node (comments are read
                // through `to_commented`); anything else is a miss.
                _ => {}
            }
        }
        current = next;
    }
    current
}

/// The source, its headers in source order, and its dominant line ending.
struct File<'s> {
    src: &'s str,
    headers: Vec<Header>,
    eol: &'static str,
}

impl File<'_> {
    /// The standalone text of one selected node, or `None` when it has none
    /// (see the module docs).
    fn slice(&self, node: &Node) -> Option<String> {
        match node {
            // Scalars render raw; the caller never reads this entry, but the
            // result stays aligned 1:1 with the evaluator's.
            Node::Value(v) if !matches!(v, TomlValue::Array(_) | TomlValue::InlineTable(_)) => {
                Some(v.to_string().trim().to_string())
            }
            // An array or inline table is a value, not a document, and TOML
            // has no top-level array: there is no standalone slice.
            Node::Value(_) | Node::Tables(..) => None,
            // A dotted table's lines spell its key (`edition.workspace = 1`);
            // cutting them loose would mean rewriting every one.
            Node::Table(t, _) if t.is_dotted() => None,
            Node::Table(_, path) => self.table(path),
        }
    }

    /// A table's own body, then each descendant section re-rooted.
    fn table(&self, path: &[Step]) -> Option<String> {
        let mut out = String::new();
        if path.is_empty() {
            // The root's own keys come before the first header.
            out.push_str(&self.src[..self.section_start(0)]);
        } else if let Some(i) = self.headers.iter().position(|h| h.path == path) {
            let h = &self.headers[i];
            let (_, close) = reroot(&self.src[h.start..], &[])?;
            let line_end = self.src[h.start + close..]
                .find('\n')
                .map_or(self.src.len(), |n| h.start + close + n + 1);
            out.push_str(&self.src[line_end..self.section_start(i + 1)]);
        }
        // An implicit table (only ever named by its sub-tables' headers) has
        // no body of its own.
        let names: Vec<&str> = path
            .iter()
            .filter_map(|s| match s {
                Step::Field(k) => Some(k.as_str()),
                _ => None,
            })
            .collect();
        for (i, h) in self.headers.iter().enumerate() {
            if h.path.len() <= path.len() || !h.path.starts_with(path) {
                continue;
            }
            let (header, close) = reroot(&self.src[h.start..], &names)?;
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str(self.eol);
            }
            out.push_str(&self.src[h.prefix_start..h.start]);
            out.push_str(&header);
            out.push_str(&self.src[h.start + close..self.section_start(i + 1)]);
        }
        Some(trim_blank_edges(&out).to_string())
    }

    /// Where the `i`th header's section begins (the end of the file past the
    /// last one).
    fn section_start(&self, i: usize) -> usize {
        self.headers
            .get(i)
            .map_or(self.src.len(), |h| h.prefix_start)
    }
}

/// The byte length of the `[a.b]` or `[[a.b]]` header at the start of `text`,
/// brackets included.
pub(crate) fn header_len(text: &str) -> Option<usize> {
    reroot(text, &[]).map(|(_, close)| close)
}

/// Re-root the header at the start of `text` (`[a.b.c]` or `[[a.b.c]]`) by
/// dropping its leading keys, which must spell `drop` (`["a"]` gives `[b.c]`):
/// the rest of the header, spacing and quoting included, is kept. Returns the
/// new header and the byte length of the old one. `None` when the header
/// doesn't parse or doesn't start with `drop`, or when nothing would be left.
/// An empty `drop` just measures the header.
fn reroot(text: &str, drop: &[&str]) -> Option<(String, usize)> {
    let brackets = if text.starts_with("[[") {
        2
    } else if text.starts_with('[') {
        1
    } else {
        return None;
    };
    let bytes = text.as_bytes();
    let skip_ws = |mut i: usize| {
        while matches!(bytes.get(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        i
    };
    let keys_start = skip_ws(brackets);
    let mut i = keys_start;
    // End of the last dropped key, and where the kept keys begin.
    let (mut dropped_end, mut kept_start) = (keys_start, keys_start);
    let mut count = 0;
    loop {
        i = key_end(bytes, i)?;
        count += 1;
        if count == drop.len() {
            dropped_end = i;
        }
        i = skip_ws(i);
        match bytes.get(i) {
            Some(b'.') => {
                i = skip_ws(i + 1);
                if count == drop.len() {
                    kept_start = i;
                }
            }
            Some(b']') => break,
            _ => return None,
        }
    }
    let close = i + brackets;
    if text.get(i..close)? != &"]]"[..brackets] {
        return None;
    }
    if drop.is_empty() {
        return Some((text[..close].to_string(), close));
    }
    if count <= drop.len() {
        return None;
    }
    // The dropped keys must be the selected table's own names (a quoted key
    // compares by its value, so `"a"` matches `a`).
    let dropped = Key::parse(&text[keys_start..dropped_end]).ok()?;
    if dropped.len() != drop.len() || dropped.iter().zip(drop).any(|(k, d)| k.get() != *d) {
        return None;
    }
    let mut header = String::with_capacity(close);
    header.push_str(&text[..keys_start]);
    header.push_str(&text[kept_start..close]);
    Some((header, close))
}

/// The end of the simple key (bare, `"basic"`, or `'literal'`) starting at
/// `i`, or `None` when there is none there.
fn key_end(bytes: &[u8], i: usize) -> Option<usize> {
    match bytes.get(i)? {
        b'"' => {
            let mut j = i + 1;
            loop {
                match bytes.get(j)? {
                    b'\\' => j += 2,
                    b'"' => return Some(j + 1),
                    b'\n' => return None,
                    _ => j += 1,
                }
            }
        }
        b'\'' => {
            let len = bytes[i + 1..]
                .iter()
                .position(|&b| b == b'\'' || b == b'\n')?;
            (bytes[i + 1 + len] == b'\'').then_some(i + len + 2)
        }
        _ => {
            let len = bytes[i..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                .count();
            (len > 0).then_some(i + len)
        }
    }
}

/// Drop the blank lines before the first content line and every line ending
/// after the last, so the fragment stands alone.
fn trim_blank_edges(s: &str) -> &str {
    let Some(first) = s.find(|c: char| !c.is_whitespace()) else {
        return "";
    };
    let s = &s[text::line_start(s, first)..];
    s.trim_end_matches(['\n', '\r'])
}

#[cfg(test)]
mod tests;
