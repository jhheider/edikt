//! Format-preserving edits and conversion emit, backed by `toml_edit`.

use crate::Toml;
use edikt_core::{Document, EditError, Expr, Mutable, Step, Value};
use toml_edit::{
    Array, ArrayOfTables, DocumentMut, InlineTable, Item, RawString, Table, TableLike,
    Value as TomlValue,
};

/// Apply a mutation expression. An edit whose result would respell a key the
/// file spells more than one way is refused, leaving `doc` as it was (see the
/// `spelling` module).
pub fn apply(doc: &mut Toml, expr: &Expr) -> Result<(), EditError> {
    doc.guarded(|doc| edikt_core::apply_mutation(doc, expr))
}

impl Mutable for Toml {
    fn whole(&self) -> Value {
        self.to_value()
    }
    fn value_at(&self, path: &[Step]) -> Option<Value> {
        Toml::value_at(self, path)
    }
    fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        self.set_value(path, value)
    }
    fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        Toml::delete(self, path)
    }
    /// An array grows in its own layout (#91): each item through
    /// [`push_in_layout`], each table of an array of tables as its own
    /// `[[key]]` block. Declined (a whole-array write instead) for a path
    /// that doesn't end in a key, and for a non-table onto an array of
    /// tables.
    fn append(&mut self, path: &[Step], items: &[Value]) -> Option<Result<(), EditError>> {
        let Some((Step::Field(key), parent)) = path.split_last() else {
            return None;
        };
        let table = crate::tree::walk_tables(self.doc.as_table_mut(), parent)?;
        match table.get_mut(key)? {
            Item::ArrayOfTables(aot) if items.iter().all(|v| matches!(v, Value::Object(_))) => {
                Some(items.iter().try_for_each(|v| {
                    aot.push(value_to_table(v)?);
                    Ok(())
                }))
            }
            Item::Value(TomlValue::Array(arr)) => Some(items.iter().try_for_each(|v| {
                push_in_layout(arr, value_to_toml(v)?);
                Ok(())
            })),
            _ => None,
        }
    }
}

/// A `Value` as a TOML `Item` for setting a key (nested objects become inline
/// tables, matching "set this value here").
pub(crate) fn value_to_item(value: &Value) -> Result<Item, EditError> {
    Ok(Item::Value(value_to_toml(value)?))
}

/// An object as a dotted-key table (`version.workspace = true`): the form
/// its sibling keys use when an object replaces a non-table value (#120).
/// Nested objects stay dotted keys all the way down; anything else is a
/// plain value, as in [`value_to_item`].
pub(crate) fn value_to_dotted_item(value: &Value) -> Result<Item, EditError> {
    let Value::Object(map) = value else {
        return value_to_item(value);
    };
    let mut t = Table::new();
    t.set_implicit(true);
    t.set_dotted(true);
    for (k, v) in map {
        t.insert(k, value_to_dotted_item(v)?);
    }
    Ok(Item::Table(t))
}

pub(crate) fn value_to_toml(value: &Value) -> Result<TomlValue, EditError> {
    Ok(match value {
        Value::Null => return Err(EditError::new("TOML has no null value")),
        Value::Bool(b) => TomlValue::from(*b),
        Value::Int(i) => TomlValue::from(*i),
        Value::Float(f) => TomlValue::from(*f),
        Value::Str(s) => TomlValue::from(s.as_str()),
        Value::Array(a) => {
            let mut arr = Array::new();
            for x in a {
                arr.push(value_to_toml(x)?);
            }
            TomlValue::Array(arr)
        }
        Value::Object(m) => {
            let mut t = InlineTable::new();
            for (k, v) in m {
                t.insert(k, value_to_toml(v)?);
            }
            TomlValue::InlineTable(t)
        }
    })
}

/// Append `v` to `arr` in the array's own layout (jhheider/edikt#91).
///
/// A one-item-per-line array (its last item starts on a new line) gets the new
/// item on its own line at that item's indentation; an empty array whose `]`
/// sits on its own line gets it one level (four spaces) past the bracket.
/// Anything else is inline and stays inline, separated like its last item.
/// The trailing-comma style is kept, and what sat between the last item and
/// `]` (a same-line comment, own-line comments, the bracket's line break) stays
/// ahead of the bracket: a comment beside the old last item stays beside it.
pub(crate) fn push_in_layout(arr: &mut Array, mut v: TomlValue) {
    fn text(r: Option<&RawString>) -> String {
        r.and_then(RawString::as_str).unwrap_or("").to_owned()
    }
    let n = arr.len();
    let comma = arr.trailing_comma();
    let last_prefix = arr.get(n.wrapping_sub(1)).map(|l| text(l.decor().prefix()));
    // Everything between the last item's value (or `[`) and `]`: the array's
    // trailing text after a trailing comma, else the last item's suffix.
    let tail = match arr.get(n.wrapping_sub(1)) {
        Some(last) if !comma => text(last.decor().suffix()),
        _ => text(Some(arr.trailing())),
    };
    let indent = match &last_prefix {
        Some(p) => p.rfind('\n').map(|i| p[i + 1..].to_owned()),
        None => tail.rfind('\n').map(|i| format!("{}    ", &tail[i + 1..])),
    };
    let (prefix, rest) = match indent {
        // Multi-line: the new item opens a line after whatever preceded the
        // bracket's line, and that line (with the `]`) follows it.
        Some(indent) => {
            // (`toml_edit` drops every `\r` on output, so CRLF needs no care.)
            let i = tail.rfind('\n').unwrap_or(tail.len());
            (format!("{}\n{indent}", &tail[..i]), tail[i..].to_owned())
        }
        None => match last_prefix {
            Some(p) if n >= 2 => (p, tail),
            Some(_) => (" ".to_owned(), tail),
            // `[ ]`: pad the item the way the brackets were padded.
            None => (tail.clone(), tail),
        },
    };
    match arr.get_mut(n.wrapping_sub(1)) {
        Some(last) if !comma => {
            last.decor_mut().set_suffix("");
            v.decor_mut().set_prefix(prefix);
            v.decor_mut().set_suffix(rest);
        }
        _ => {
            v.decor_mut().set_prefix(prefix);
            v.decor_mut().set_suffix("");
            if n == 0 && rest.contains('\n') {
                // A fresh multi-line list takes a trailing comma, the style
                // that lets the next append touch one line.
                arr.set_trailing_comma(true);
            }
            arr.set_trailing(rest);
        }
    }
    arr.push_formatted(v);
}

/// `new` as the replacement for `old`, spelled the way `old` was: a string
/// keeps the old string's quote style where it can (see [`respell`]), and the
/// old value's decor (surrounding spacing, an inline comment) carries over.
pub(crate) fn replacing(old: &TomlValue, new: TomlValue) -> TomlValue {
    let mut new = match (old, &new) {
        (TomlValue::String(f), TomlValue::String(n)) => f
            .as_repr()
            .and_then(|r| r.as_raw().as_str())
            .and_then(|raw| respell(raw, n.value()))
            .unwrap_or(new),
        _ => new,
    };
    *new.decor_mut() = old.decor().clone();
    new
}

/// The old value's decor (spacing after `=`, an inline comment) across a
/// dotted-key table that replaces it (#120): the prefix opens the table's
/// first `key = value` line, the suffix closes its last.
pub(crate) fn carry_line_decor(old: &TomlValue, t: &mut Table) {
    let mut leaves = Vec::new();
    crate::comments::dotted_leaves(t, &mut Vec::new(), &mut leaves);
    let (Some(first), Some(last)) = (leaves.first(), leaves.last()) else {
        return;
    };
    if let Some(prefix) = old
        .decor()
        .prefix()
        .and_then(RawString::as_str)
        .map(str::to_owned)
        && let Some(v) = leaf_mut(t, first)
    {
        v.decor_mut().set_prefix(prefix);
    }
    if let Some(suffix) = old
        .decor()
        .suffix()
        .and_then(RawString::as_str)
        .map(str::to_owned)
        && let Some(v) = leaf_mut(t, last)
    {
        v.decor_mut().set_suffix(suffix);
    }
}

/// The value at `path` (keys from [`crate::comments::dotted_leaves`]),
/// through the nested dotted tables that spell it.
fn leaf_mut<'a>(t: &'a mut Table, path: &[String]) -> Option<&'a mut TomlValue> {
    let (k, rest) = path.split_first()?;
    match (rest.is_empty(), t.get_mut(k.as_str())) {
        (true, Some(Item::Value(v))) => Some(v),
        (false, Some(Item::Table(sub))) => leaf_mut(sub, rest),
        _ => None,
    }
}

/// `s` spelled in the style of the string token `old` (jhheider/edikt#81),
/// falling back to the nearest style that can hold it: a literal (`'`, `'''`)
/// has no escapes, so a value it can't carry verbatim goes basic (`"`,
/// `"""`), staying multi-line if the old token was. Basic strings spell
/// anything.
fn respell(old: &str, s: &str) -> Option<TomlValue> {
    let text = if old.starts_with("'''") {
        if ml_literal_ok(s) {
            format!("'''{s}'''")
        } else {
            ml_basic(s)
        }
    } else if old.starts_with('\'') {
        if literal_ok(s) {
            format!("'{s}'")
        } else {
            basic(s)
        }
    } else if old.starts_with("\"\"\"") {
        ml_basic(s)
    } else {
        basic(s)
    };
    // Parsing the spelling (rather than building a repr by hand) proves it is
    // valid TOML for exactly this string; anything else keeps the default.
    text.parse::<TomlValue>()
        .ok()
        .filter(|v| v.as_str() == Some(s))
}

/// A control character a TOML string can't hold raw (tab is allowed).
fn is_control(c: char) -> bool {
    c.is_ascii_control() && c != '\t'
}

fn literal_ok(s: &str) -> bool {
    !s.chars().any(|c| c == '\'' || is_control(c))
}

/// A multi-line literal can't contain `'''`, can't end with `'` (it would run
/// into the closing delimiter), can't start with a newline (TOML trims one
/// there), and holds only the newline among the controls.
fn ml_literal_ok(s: &str) -> bool {
    !s.contains("'''")
        && !s.ends_with('\'')
        && !s.starts_with(['\n', '\r'])
        && !s.chars().any(|c| c != '\n' && is_control(c))
}

/// A one-line basic string, every special character escaped.
fn basic(s: &str) -> String {
    format!("\"{}\"", escape(s, false))
}

/// A multi-line basic string: line breaks stay raw except a leading one (which
/// TOML would trim), and everything else is escaped as in [`basic`].
fn ml_basic(s: &str) -> String {
    let body = escape(s, true);
    match body.strip_prefix('\n') {
        Some(rest) => format!("\"\"\"\\n{rest}\"\"\""),
        None => format!("\"\"\"{body}\"\"\""),
    }
}

fn escape(s: &str, raw_newlines: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' if raw_newlines => out.push('\n'),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if is_control(c) => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Emit a value as TOML: top-level objects become `[table]`s (nested-in-value
/// objects stay inline). Returns text and warnings (none; TOML holds nesting,
/// arrays, and typed scalars).
pub fn emit(value: &Value) -> Result<(String, Vec<String>), EditError> {
    let Value::Object(m) = value else {
        return Err(EditError::new(
            "TOML output requires a table (top-level object)",
        ));
    };
    let mut doc = DocumentMut::new();
    for (k, v) in m {
        doc.insert(k, value_to_item_tables(v)?);
    }
    Ok((doc.to_string(), Vec::new()))
}

fn value_to_item_tables(value: &Value) -> Result<Item, EditError> {
    match value {
        Value::Object(m) => {
            let mut t = Table::new();
            for (k, v) in m {
                t.insert(k, value_to_item_tables(v)?);
            }
            Ok(Item::Table(t))
        }
        _ => Ok(Item::Value(value_to_toml(value)?)),
    }
}

/// An object `Value` as a standalone TOML `Table` (a `[[..]]` array-of-tables
/// element). Nested objects become sub-tables.
fn value_to_table(value: &Value) -> Result<Table, EditError> {
    match value_to_item_tables(value)? {
        Item::Table(t) => Ok(t),
        _ => Err(EditError::new("an array-of-tables element must be a table")),
    }
}

/// Would the array at `container[key]` be (or become) an array-of-tables? True
/// when it is already one, is absent (auto-vivify as one), or is an empty inline
/// array (promote it), so a table value lands in a `[[key]]` block, not inline.
fn array_is_aot_shaped(container: &dyn TableLike, key: &str) -> bool {
    match container.get(key) {
        None => true,
        Some(Item::ArrayOfTables(_)) => true,
        Some(Item::Value(TomlValue::Array(a))) => a.is_empty(),
        _ => false,
    }
}

/// Resolve `idx` (jq-style: negative counts from the end) against `len` for a
/// set, allowing `idx == len` as an append. Out of range is an error naming the
/// append index.
pub(crate) fn resolve_set_index(idx: i64, len: usize) -> Result<usize, EditError> {
    edikt_core::normalize_index(idx, len)
        .filter(|&n| n <= len)
        .ok_or_else(|| {
            EditError::new(format!(
                "array index {idx} out of range (length {len}); append with index {len}"
            ))
        })
}

/// Resolve `idx` against `len` for a delete (no append); out of range yields
/// `None`, a jq-style no-op.
pub(crate) fn resolve_del_index(idx: i64, len: usize) -> Option<usize> {
    edikt_core::resolve_index(idx, len)
}

/// Set (replace or append) the array element at `container[key][idx]`. A table
/// value in an array-of-tables (or absent/empty array) yields a `[[key]]` block;
/// scalars and arrays yield an inline array element. Auto-vivifies the array.
pub(crate) fn set_array_element(
    container: &mut dyn TableLike,
    key: &str,
    idx: i64,
    value: &Value,
) -> Result<(), EditError> {
    if matches!(value, Value::Object(_)) && array_is_aot_shaped(container, key) {
        if !matches!(container.get(key), Some(Item::ArrayOfTables(_))) {
            container.insert(key, Item::ArrayOfTables(ArrayOfTables::new()));
        }
        let aot = container
            .get_mut(key)
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| EditError::new(format!("`{key}` is not an array of tables")))?;
        let at = resolve_set_index(idx, aot.len())?;
        let table = value_to_table(value)?;
        if at == aot.len() {
            aot.push(table);
        } else if let Some(slot) = aot.get_mut(at) {
            *slot = table;
        }
    } else {
        match container.get(key) {
            Some(Item::Value(TomlValue::Array(_))) => {}
            None => {
                container.insert(key, Item::Value(TomlValue::Array(Array::new())));
            }
            Some(_) => return Err(EditError::new(format!("`{key}` is not an array"))),
        }
        let arr = container
            .get_mut(key)
            .and_then(Item::as_value_mut)
            .and_then(TomlValue::as_array_mut)
            .ok_or_else(|| EditError::new(format!("`{key}` is not an array")))?;
        let at = resolve_set_index(idx, arr.len())?;
        let tv = value_to_toml(value)?;
        if at == arr.len() {
            push_in_layout(arr, tv);
        } else if let Some(slot) = arr.get_mut(at) {
            *slot = replacing(slot, tv);
        }
    }
    Ok(())
}

/// Delete the array element at `container[key][idx]` (missing key, wrong type, or
/// out-of-range index is a no-op).
pub(crate) fn delete_array_element(container: &mut dyn TableLike, key: &str, idx: i64) {
    let Some(item) = container.get_mut(key) else {
        return;
    };
    if let Some(aot) = item.as_array_of_tables_mut() {
        if let Some(at) = resolve_del_index(idx, aot.len()) {
            aot.remove(at);
        }
    } else if let Some(arr) = item.as_value_mut().and_then(TomlValue::as_array_mut)
        && let Some(at) = resolve_del_index(idx, arr.len())
    {
        remove_in_layout(arr, at);
    }
}

/// Remove item `at` from `arr`, leaving its neighbours' layout as it was.
///
/// `toml_edit` keeps what follows an item's comma (a comment beside it, the
/// line break, the next item's indent) in the next item's prefix, or in the
/// array's trailing text after the last one, so removing an item's decor
/// alone would hand its neighbour's comment to the item after it. In a
/// one-item-per-line array (the item and what follows it each open a line),
/// the item's line goes whole with the comment beside it, and the comment
/// beside the item before it, and own-line comments above it, stay. Inline,
/// the item goes with one separator: `[1, 2, 3]` loses `1, ` or `, 3`.
fn remove_in_layout(arr: &mut Array, at: usize) {
    fn text(r: Option<&RawString>) -> String {
        r.and_then(RawString::as_str).unwrap_or("").to_owned()
    }
    /// Up to and including the first line break, and the rest.
    fn split(s: &str) -> (&str, &str) {
        s.find('\n').map_or((s, ""), |i| s.split_at(i + 1))
    }
    /// The whole lines of `s`: everything up to its last line break.
    fn lines(s: &str) -> &str {
        s.rfind('\n').map_or("", |i| &s[..=i])
    }
    let n = arr.len();
    let comma = arr.trailing_comma();
    let own = text(arr.get(at).and_then(|v| v.decor().prefix()));
    // What follows the item's comma (or, for a comma-less last item, the
    // item itself): the next item's prefix, the array's trailing text, or
    // the item's own suffix.
    let next = if at + 1 < n {
        text(arr.get(at + 1).and_then(|v| v.decor().prefix()))
    } else if comma {
        text(Some(arr.trailing()))
    } else {
        text(arr.get(at).and_then(|v| v.decor().suffix()))
    };
    let own_line = own.contains('\n') && next.contains('\n');
    // The layout that takes the removed item's place.
    let joined = |inline: String| {
        if own_line {
            let (before, above) = split(&own);
            format!("{before}{}{}", lines(above), split(&next).1)
        } else {
            inline
        }
    };
    if at + 1 < n {
        let prefix = joined(if at == 0 { own.clone() } else { next.clone() });
        if let Some(v) = arr.get_mut(at + 1) {
            v.decor_mut().set_prefix(prefix);
        }
    } else if comma || at == 0 {
        let trailing = joined(if comma { next.clone() } else { String::new() });
        arr.set_trailing(trailing);
    } else if let Some(prev) = arr.get_mut(at - 1) {
        let suffix = format!("{}{}", text(prev.decor().suffix()), joined(next.clone()));
        prev.decor_mut().set_suffix(suffix);
    }
    arr.remove(at);
}
