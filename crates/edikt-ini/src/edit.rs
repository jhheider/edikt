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

/// A `Value` node wrapping the string `s` (empty node if `s` is empty).
/// Insert a `key = value` entry into INI source at the right place: the end of
/// the named section's content (creating the section at EOF if absent), or the
/// preamble. The caller reparses the result.
pub(crate) fn insert_entry(src: &str, section: Option<&str>, key: &str, value: &str) -> String {
    // New lines end the way most of the file's lines do.
    let eol = edikt_core::text::dominant(src);
    let new_line = format!("{key} = {value}{eol}");
    let lines: Vec<&str> = src.split_inclusive('\n').collect();

    let (start, end) = match section {
        None => {
            let first = lines.iter().position(|l| l.trim_start().starts_with('['));
            (0, first.unwrap_or(lines.len()))
        }
        Some(sec) => match lines.iter().position(|l| header_matches(l, sec)) {
            Some(hi) => {
                let end = lines[hi + 1..]
                    .iter()
                    .position(|l| l.trim_start().starts_with('['))
                    .map(|p| hi + 1 + p)
                    .unwrap_or(lines.len());
                (hi + 1, end)
            }
            None => {
                // Section absent: append `[section]\nkey = value\n` at EOF.
                let mut out = String::from(src);
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push_str(eol);
                }
                if !out.is_empty() {
                    out.push_str(eol);
                }
                out.push_str(&format!("[{sec}]{eol}{new_line}"));
                return out;
            }
        },
    };

    // Insert after the last non-blank line within [start, end).
    let mut content_end = start;
    for (j, line) in lines.iter().enumerate().take(end).skip(start) {
        if !line.trim().is_empty() {
            content_end = j + 1;
        }
    }

    let mut out = String::new();
    for line in &lines[..content_end] {
        out.push_str(line);
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(eol);
    }
    out.push_str(&new_line);
    for line in &lines[content_end..] {
        out.push_str(line);
    }
    out
}

/// Does `line` open the section named `section` (`[section]`, ignoring
/// trailing content)?
fn header_matches(line: &str, section: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('[') && t[1..].split(']').next() == Some(section)
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
