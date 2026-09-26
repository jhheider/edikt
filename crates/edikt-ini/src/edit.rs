//! Format-preserving edits for INI.
//!
//! Entries carry their value in a `Value` node, so `set` is a `replace_with` of
//! just that node (key/separator/spacing untouched). Each `Entry` spans its whole
//! line including the terminator, so `del` is a single `detach`.
//!
//! INI values are scalars: setting an array or object errors (the format has no
//! nesting or arrays; its `Feature` set says so).

use crate::syntax::{Sk, SyntaxNode};
use crate::{Ini, project};
use edikt_core::{Document, EditError, Expr, Mutable, MutationKind, Step, Value, eval};
use rowan::NodeOrToken;

pub fn apply(doc: &mut Ini, expr: &Expr) -> Result<(), EditError> {
    edikt_core::apply_mutation(doc, expr)
}

impl Mutable for Ini {
    /// INI is flat: there is nothing to fan `[]` out over but scalars.
    const FANS_OUT: bool = false;

    fn whole(&self) -> Value {
        self.to_value()
    }
    fn value_at(&self, path: &[Step]) -> Option<Value> {
        Ini::value_at(self, path)
    }
    fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        Ini::set(self, path, value)
    }
    fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        Ini::delete(self, path)
    }
    /// `[]` is the array family, which this flat format lacks; the update
    /// forms resolve through `value_at` and would otherwise misreport it as
    /// "path not found".
    fn check_path(&self, path: &[Step], kind: MutationKind) -> Result<(), EditError> {
        match kind {
            MutationKind::Update | MutationKind::Add if path.contains(&Step::Iterate) => {
                no_iterate(self, path)
            }
            _ => Ok(()),
        }
    }
}

/// INI's answer to `[]` in an update/append path: the precise type error when
/// the iterate would land on a value (e.g. `cannot iterate over string`), or a
/// clear unsupported hint for a section iterate - never the misleading
/// `path not found` that `value_at` would produce for any `[]` path.
fn no_iterate(doc: &Ini, steps: &[Step]) -> Result<(), EditError> {
    eval(&Expr::Path(steps.to_vec()), &doc.to_value())?;
    Err(EditError::new(
        "`[]` in an assignment is not supported for INI: it is flat key-value, \
         with nothing to iterate but scalars",
    ))
}

/// Resolve `.key` (preamble) or `.section.key` to its `Entry` node.
pub(crate) fn resolve_entry(root: &SyntaxNode, path: &[Step]) -> Option<SyntaxNode> {
    match path {
        [Step::Field(key)] => find_entry(&preamble(root)?, key),
        [Step::Field(section), Step::Field(key)] => find_entry(&named_section(root, section)?, key),
        _ => None,
    }
}

fn preamble(root: &SyntaxNode) -> Option<SyntaxNode> {
    root.children()
        .filter(|n| n.kind() == Sk::Section)
        .find(|s| s.children().all(|c| c.kind() != Sk::Header))
}

fn named_section(root: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    root.children()
        .filter(|n| n.kind() == Sk::Section)
        .find(|s| {
            s.children()
                .find(|c| c.kind() == Sk::Header)
                .map(|h| project::section_name(&h))
                .as_deref()
                == Some(name)
        })
}

fn find_entry(section: &SyntaxNode, key: &str) -> Option<SyntaxNode> {
    section
        .children()
        .filter(|n| n.kind() == Sk::Entry)
        .find(|e| project::entry_key(e) == key)
}

/// Insert a `key = value` entry at the right place: after the last line of
/// content (entries, comments) in the named section, creating the section at
/// EOF if absent, or in the preamble. Returns the new source; the caller
/// reparses it.
pub(crate) fn insert_entry(
    root: &SyntaxNode,
    section: Option<&str>,
    key: &str,
    value: &str,
) -> String {
    let src = edikt_syntax::to_source(root);
    // New lines end the way most of the file's lines do.
    let eol = edikt_core::text::dominant(&src);
    let new_line = format!("{key} = {value}{eol}");
    let target = match section {
        None => preamble(root),
        Some(sec) => named_section(root, sec),
    };
    let Some(target) = target else {
        // Section absent: append `[section]\nkey = value\n` at EOF. (The
        // preamble always exists, possibly empty.)
        let mut out = src;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        if !out.is_empty() {
            out.push_str(eol);
        }
        out.push_str(&format!("[{}]{eol}{new_line}", section.unwrap_or_default()));
        return out;
    };
    let at = content_end(&target);
    let (before, after) = src.split_at(at);
    let mut out = String::with_capacity(src.len() + new_line.len() + eol.len());
    out.push_str(before);
    if !before.is_empty() && !before.ends_with('\n') {
        out.push_str(eol);
    }
    out.push_str(&new_line);
    out.push_str(after);
    out
}

/// The byte offset just past a section's last line of content (its header,
/// an entry, or a comment line, with that line's terminator), so trailing
/// blank lines stay after a new entry. An empty section's own start.
fn content_end(section: &SyntaxNode) -> usize {
    let mut end = section.text_range().start();
    let mut after_comment = false;
    for el in section.children_with_tokens() {
        match &el {
            NodeOrToken::Node(n) => {
                end = n.text_range().end();
                after_comment = false;
            }
            NodeOrToken::Token(t) if t.kind() == Sk::Comment => {
                end = t.text_range().end();
                after_comment = true;
            }
            NodeOrToken::Token(t) if after_comment && t.kind() == Sk::Newline => {
                end = t.text_range().end();
                after_comment = false;
            }
            NodeOrToken::Token(_) => {}
        }
    }
    end.into()
}

/// Refuse a value INI cannot hold: the scanner would read the line back
/// differently. INI has no quoting (quotes are part of the value), so there
/// is nothing to escape with.
pub(crate) fn check_value(text: &str) -> Result<(), EditError> {
    let why = if text.contains(['\n', '\r']) {
        Some("a line break would split the line")
    } else if text.trim() != text {
        Some("leading or trailing whitespace reads back trimmed")
    } else if text.starts_with([';', '#'])
        || text
            .as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_whitespace() && matches!(w[1], b';' | b'#'))
    {
        Some("a `;` or `#` at the start or after whitespace starts an inline comment")
    } else {
        None
    };
    match why {
        Some(why) => Err(EditError::new(format!(
            "INI can't hold the value {text:?}: {why}, and INI has no quoting"
        ))),
        None => Ok(()),
    }
}

/// Refuse a new key (or section name) INI cannot hold; an existing one is
/// already known to read back as itself.
pub(crate) fn check_key(key: &str) -> Result<(), EditError> {
    let why = if key.contains(['\n', '\r']) {
        Some("a line break would split the line")
    } else if key.trim() != key {
        Some("leading or trailing whitespace reads back trimmed")
    } else if key.starts_with([';', '#']) {
        Some("a line starting with `;` or `#` is a comment")
    } else if key.starts_with('[') {
        Some("a line starting with `[` is a section header")
    } else if key.contains(['=', ':']) {
        Some("`=` or `:` would end the key early")
    } else {
        None
    };
    match why {
        Some(why) => Err(EditError::new(format!(
            "INI can't hold the key {key:?}: {why}, and INI has no quoting"
        ))),
        None => Ok(()),
    }
}

pub(crate) fn check_section(name: &str) -> Result<(), EditError> {
    let why = if name.contains(['\n', '\r']) {
        Some("a line break would split the header")
    } else if name.contains(']') {
        Some("`]` would close the header early")
    } else {
        None
    };
    match why {
        Some(why) => Err(EditError::new(format!(
            "INI can't hold the section name {name:?}: {why}, and INI has no quoting"
        ))),
        None => Ok(()),
    }
}
