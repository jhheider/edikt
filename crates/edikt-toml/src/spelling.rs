//! Key spellings `toml_edit` cannot keep (jhheider/edikt#104).
//!
//! `toml_edit` stores one `Key` (its quoting and the spacing around its dots)
//! per table entry, and writes every header and dotted key path from those
//! stored keys. A key the file spells more than one way therefore comes back
//! in one spelling everywhere: `["pkg".a]` and `[pkg.b]` share the implicit
//! table `pkg`, so both re-emit as `"pkg"`; `[[ bin ]]` and `[[bin]]` share
//! the key `bin`; `"a".b = 1` and `a.c = 2` share the dotted table `a`. An
//! edit anywhere in such a file would rewrite those lines, which the moat
//! forbids.
//!
//! The fix works on the output. Every place a key path is written (a table
//! header, or the keys in front of a `key = value`) is an **occurrence** with
//! a structural identity that survives an edit: the header's table (by the
//! position `toml_edit` gave it at parse, which moves with the table), and a
//! key path's keys under that header, or under the value holding its inline
//! table. The unedited document is rendered once to find the occurrences
//! `toml_edit` respells; after an edit, each of those is written back with the
//! source's own text. Nothing else is touched, so the rest of the output is
//! `toml_edit`'s as before.
//!
//! One identity is weaker: an inline table inside an array is found by its
//! index, which an edit to the array can shift. A respelled occurrence under
//! an array is restored only while the element holding it keeps its value
//! and the array hasn't shrunk (only a removal shifts an element), so the
//! restored text is that element's own; an edit that changes the element or
//! removes from the array is refused rather than respelled.

use std::collections::HashMap;
use std::ops::Range;

use edikt_core::{EditError, Value};
use toml_edit::{ImDocument, InlineTable, Item, Key, Table, Value as TomlValue};

/// One step from a container to an occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Seg {
    Key(String),
    Index(usize),
}

/// Where an occurrence's text belongs: the root's body, or a header's table
/// (its `toml_edit` parse position).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Container {
    Root,
    Header(usize),
}

/// An occurrence's identity: the header itself (`keys` empty and `at`
/// empty), or the key path `keys` written in the table at `at` (the container
/// itself, or the inline table held by the value at `at`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Id {
    container: Container,
    at: Vec<Seg>,
    keys: Vec<String>,
}

struct Occurrence {
    id: Id,
    /// The header (brackets included) or the key path (first key through
    /// the last), as a byte range of the walked text.
    span: Range<usize>,
    /// The element of the outermost array this occurrence sits in, as an
    /// index into [`Walk::guards`].
    guard: Option<usize>,
}

/// An element of an array, as far as restoring a spelling inside it goes.
#[derive(Debug, Clone)]
struct Guard {
    len: usize,
    element: Value,
}

impl Guard {
    /// Does `now` still hold this element at its index? Only an array that
    /// lost elements can have shifted one into another's place (an append
    /// can't), and then the element's value tells them apart.
    fn holds(&self, now: &Guard) -> bool {
        now.len >= self.len && self.element.identical(&now.element)
    }
}

/// The source's own text for each occurrence `toml_edit` respells.
#[derive(Default)]
pub(crate) struct Respelled(HashMap<Id, Kept>);

struct Kept {
    text: String,
    /// The array element the occurrence sits in, when it does.
    guard: Option<Guard>,
}

impl Respelled {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Find what rendering `src` (a whole TOML document, without a BOM) with
/// `toml_edit` would respell.
pub(crate) fn respelled(src: &str) -> Respelled {
    let Ok(orig) = ImDocument::parse(src) else {
        return Respelled::default();
    };
    let rendered = orig.clone().into_mut().to_string();
    if rendered == src {
        return Respelled::default();
    }
    let Ok(re) = ImDocument::parse(rendered.as_str()) else {
        return Respelled::default();
    };
    let tree = orig.as_table();
    let ours = Walk::run(src, tree, tree);
    let theirs = Walk::run(&rendered, re.as_table(), tree);
    let rendered_text: HashMap<&Id, &str> = theirs
        .found
        .iter()
        .map(|o| (&o.id, &rendered[o.span.clone()]))
        .collect();
    let mut kept = HashMap::new();
    for o in ours.found {
        let text = &src[o.span.clone()];
        if rendered_text.get(&o.id).is_some_and(|r| *r != text) {
            let guard = o.guard.map(|g| ours.guards[g].clone());
            kept.insert(
                o.id,
                Kept {
                    text: text.to_string(),
                    guard,
                },
            );
        }
    }
    Respelled(kept)
}

/// `out`, the rendering of the (possibly edited) document `tree`, with each
/// respelled occurrence written back as the source spelled it. Errors when
/// an occurrence can't be restored with certainty (see the module docs).
pub(crate) fn restore(out: String, tree: &Table, kept: &Respelled) -> Result<String, EditError> {
    if kept.is_empty() {
        return Ok(out);
    }
    let doc = ImDocument::parse(out.as_str())
        .map_err(|e| EditError::new(format!("cannot keep this file's key spellings: {e}")))?;
    let walk = Walk::run(&out, doc.as_table(), tree);
    let mut edits: Vec<(Range<usize>, &str)> = Vec::new();
    for o in &walk.found {
        let Some(k) = kept.0.get(&o.id) else {
            continue;
        };
        let now = &out[o.span.clone()];
        let same_array = match (&k.guard, o.guard) {
            (None, None) => true,
            (Some(was), Some(g)) => was.holds(&walk.guards[g]),
            _ => false,
        };
        let same_keys = matches!((names(now), names(&k.text)), (Some(a), Some(b)) if a == b);
        if !same_array || !same_keys {
            let place = if same_array {
                ""
            } else {
                " in an array this edit changes"
            };
            return Err(EditError::new(format!(
                "cannot keep the spelling of `{}`{place}: this file spells that key \
                 more than one way, and toml_edit keeps one spelling per key (spell \
                 it one way to edit here)",
                k.text
            )));
        }
        if now != k.text {
            edits.push((o.span.clone(), &k.text));
        }
    }
    let mut out = out;
    edits.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    for (span, text) in edits {
        out.replace_range(span, text);
    }
    Ok(out)
}

/// The key names a header (`[a."b"]`, `[[a]]`) or a key path (`a . "b"`)
/// spells, or `None` when it doesn't parse.
fn names(text: &str) -> Option<Vec<String>> {
    let inner = text.trim_start_matches('[').trim_end_matches(']');
    let keys = Key::parse(inner).ok()?;
    Some(keys.iter().map(|k| k.get().to_string()).collect())
}

/// Every occurrence in a parsed text.
struct Walk<'a> {
    src: &'a str,
    /// The document whose table positions name the headers: the one `src`
    /// was rendered from (for the source itself, its own parse).
    tree: &'a Table,
    found: Vec<Occurrence>,
    guards: Vec<Guard>,
}

impl<'a> Walk<'a> {
    fn run(src: &'a str, doc: &Table, tree: &'a Table) -> Self {
        let mut walk = Walk {
            src,
            tree,
            found: Vec::new(),
            guards: Vec::new(),
        };
        walk.section(doc, &mut Vec::new());
        walk
    }

    /// A table that owns its text: the root, or one with its own header. An
    /// implicit table has neither a header nor a body; a table with no
    /// position in `tree` is new, with no source text to keep.
    fn section(&mut self, table: &Table, path: &mut Vec<Seg>) {
        let container = if path.is_empty() {
            Some(Container::Root)
        } else if table.is_implicit() {
            None
        } else {
            table_at(self.tree, path)
                .and_then(Table::position)
                .map(Container::Header)
        };
        if let (Some(Container::Header(p)), Some(span)) = (&container, table.span())
            && let Some(len) = crate::slice::header_len(&self.src[span.start..])
        {
            self.found.push(Occurrence {
                id: Id {
                    container: Container::Header(*p),
                    at: Vec::new(),
                    keys: Vec::new(),
                },
                span: span.start..span.start + len,
                guard: None,
            });
        }
        self.body(table, container.as_ref(), &[], path);
    }

    /// The entries of `table`, written under `container` with the key path
    /// prefix `keys` (non-empty inside a dotted table).
    fn body(
        &mut self,
        table: &Table,
        container: Option<&Container>,
        keys: &[String],
        path: &mut Vec<Seg>,
    ) {
        for (k, item) in table.iter() {
            let mut keys = keys.to_vec();
            keys.push(k.to_string());
            path.push(Seg::Key(k.to_string()));
            match item {
                Item::Value(v) => {
                    if let Some(c) = container
                        && let Some((leaf, _)) = table.get_key_value(k)
                    {
                        self.key_path(c, &[], &keys, leaf, None);
                        let at: Vec<Seg> = keys.iter().cloned().map(Seg::Key).collect();
                        self.value(v, c, at, None);
                    }
                }
                Item::Table(t) if t.is_dotted() => self.body(t, container, &keys, path),
                Item::Table(t) => self.section(t, path),
                Item::ArrayOfTables(a) => {
                    for (i, t) in a.iter().enumerate() {
                        path.push(Seg::Index(i));
                        self.section(t, path);
                        path.pop();
                    }
                }
                Item::None => {}
            }
            path.pop();
        }
    }

    /// The inline tables inside the value at `at`.
    fn value(&mut self, v: &TomlValue, c: &Container, at: Vec<Seg>, guard: Option<usize>) {
        match v {
            TomlValue::Array(a) => {
                for (i, e) in a.iter().enumerate() {
                    let mut at = at.clone();
                    at.push(Seg::Index(i));
                    let guard = guard.or_else(|| {
                        self.guards.push(Guard {
                            len: a.len(),
                            element: crate::project::toml_value_to_value(e),
                        });
                        Some(self.guards.len() - 1)
                    });
                    self.value(e, c, at, guard);
                }
            }
            TomlValue::InlineTable(t) => self.inline(t, c, &at, &[], guard),
            _ => {}
        }
    }

    /// The entries of the inline table at `at`, with key path prefix `keys`.
    fn inline(
        &mut self,
        t: &InlineTable,
        c: &Container,
        at: &[Seg],
        keys: &[String],
        guard: Option<usize>,
    ) {
        for (k, v) in t.iter() {
            let mut keys = keys.to_vec();
            keys.push(k.to_string());
            if let TomlValue::InlineTable(sub) = v
                && sub.is_dotted()
            {
                self.inline(sub, c, at, &keys, guard);
                continue;
            }
            if let Some((leaf, _)) = t.get_key_value(k) {
                self.key_path(c, at, &keys, leaf, guard);
            }
            let mut inner = at.to_vec();
            inner.extend(keys.iter().cloned().map(Seg::Key));
            self.value(v, c, inner, guard);
        }
    }

    /// Record the key path `keys`, which ends in `leaf`.
    fn key_path(
        &mut self,
        c: &Container,
        at: &[Seg],
        keys: &[String],
        leaf: &Key,
        guard: Option<usize>,
    ) {
        let Some(span) = leaf.span() else {
            return;
        };
        let Some(start) = key_path_start(self.src.as_bytes(), span.start, keys.len()) else {
            return;
        };
        self.found.push(Occurrence {
            id: Id {
                container: c.clone(),
                at: at.to_vec(),
                keys: keys.to_vec(),
            },
            span: start..span.end,
            guard,
        });
    }
}

/// The table at `path` (keys, and indices into arrays of tables) in `root`.
fn table_at<'t>(root: &'t Table, path: &[Seg]) -> Option<&'t Table> {
    let mut table = root;
    let mut i = 0;
    while i < path.len() {
        let Seg::Key(k) = &path[i] else {
            return None;
        };
        table = match (table.get(k)?, path.get(i + 1)) {
            (Item::ArrayOfTables(a), Some(Seg::Index(n))) => {
                i += 1;
                a.get(*n)?
            }
            (Item::Table(t), _) => t,
            _ => return None,
        };
        i += 1;
    }
    Some(table)
}

/// Where the key path of `n` keys whose last key starts at `leaf` begins:
/// back over each `.` (and the spaces around it) and the key before it.
fn key_path_start(src: &[u8], leaf: usize, n: usize) -> Option<usize> {
    let back_ws = |mut i: usize| {
        while i > 0 && matches!(src[i - 1], b' ' | b'\t') {
            i -= 1;
        }
        i
    };
    let mut i = leaf;
    for _ in 1..n {
        i = back_ws(i);
        if i == 0 || src[i - 1] != b'.' {
            return None;
        }
        i = key_start(src, back_ws(i - 1))?;
    }
    Some(i)
}

/// The start of the simple key (bare, `"basic"` or `'literal'`) that ends at
/// `end`.
fn key_start(src: &[u8], end: usize) -> Option<usize> {
    match *src.get(end.checked_sub(1)?)? {
        b'\'' => {
            let open = src[..end - 1].iter().rposition(|&b| b == b'\'')?;
            (!src[open..end].contains(&b'\n')).then_some(open)
        }
        b'"' => {
            let mut j = end - 1;
            loop {
                j = src[..j].iter().rposition(|&b| b == b'"' || b == b'\n')?;
                if src[j] == b'\n' {
                    return None;
                }
                // A quote after an odd run of backslashes is escaped.
                let slashes = src[..j].iter().rev().take_while(|&&b| b == b'\\').count();
                if slashes % 2 == 0 {
                    return Some(j);
                }
            }
        }
        _ => {
            let len = src[..end]
                .iter()
                .rev()
                .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                .count();
            (len > 0).then_some(end - len)
        }
    }
}

#[cfg(test)]
mod tests;
