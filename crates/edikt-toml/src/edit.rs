//! Format-preserving edits and conversion emit, backed by `toml_edit`.

use crate::Toml;
use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval, expand_iter_paths};
use toml_edit::{
    Array, ArrayOfTables, DocumentMut, InlineTable, Item, RawString, Table, TableLike,
    Value as TomlValue,
};

pub fn apply(doc: &mut Toml, expr: &Expr) -> Result<(), EditError> {
    match expr {
        Expr::Assign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let whole = doc.to_value();
            let value = eval_one(rhs, &whole)?;
            if steps.contains(&Step::Iterate) {
                return set_each(doc, steps, &whole, true, |_| Ok(value.clone()));
            }
            doc.set(steps, &value)
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let whole = doc.to_value();
            if steps.contains(&Step::Iterate) {
                return set_each(doc, steps, &whole, false, |current| eval_one(rhs, current));
            }
            let current = doc
                .value_at(steps)
                .ok_or_else(|| EditError::new("path not found"))?;
            let value = eval_one(rhs, &current)?;
            doc.set(steps, &value)
        }
        Expr::AddAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let whole = doc.to_value();
            let addend = eval_one(rhs, &whole)?;
            if steps.contains(&Step::Iterate) {
                return set_each(doc, steps, &whole, false, |current| {
                    add_values(current, &addend)
                });
            }
            let current = doc
                .value_at(steps)
                .ok_or_else(|| EditError::new("path not found"))?;
            doc.set(steps, &add_values(&current, &addend)?)
        }
        Expr::Pipe(a, b) => {
            apply(doc, a)?;
            apply(doc, b)
        }
        Expr::Call(name, args) if name == "del" => {
            if args.len() != 1 {
                return Err(EditError::new("del(...) takes one path argument"));
            }
            let steps = args[0]
                .as_path()
                .ok_or_else(|| EditError::new("del(...) takes a path"))?;
            doc.delete(steps)
        }
        _ => Err(EditError::new(
            "expected an assignment (`path = value`) or `del(path)`",
        )),
    }
}

fn assign_path(lhs: &Expr) -> Result<&[Step], EditError> {
    lhs.as_path()
        .ok_or_else(|| EditError::new("left side of an assignment must be a path"))
}

fn eval_one(expr: &Expr, input: &Value) -> Result<Value, EditError> {
    eval(expr, input)
        .map_err(|e| EditError::new(e.to_string()))?
        .into_iter()
        .next()
        .ok_or_else(|| EditError::new("right side of the assignment produced no value"))
}

fn add_values(current: &Value, addend: &Value) -> Result<Value, EditError> {
    let expr = Expr::Binary(
        BinOp::Add,
        Box::new(Expr::Path(Vec::new())),
        Box::new(Expr::Literal(addend.clone())),
    );
    eval_one(&expr, current)
}

/// Apply `f` to each element selected by `steps` (which contains at least one
/// `Step::Iterate`), setting each element's value in place via the ordinary
/// index-keyed `set` path. `steps` must *resolve* against the value model so the
/// expansion knows how many elements exist; the per-element edit is then a
/// plain `.a[i]` set, format-preserving.
fn set_each(
    doc: &mut Toml,
    steps: &[Step],
    whole: &Value,
    create: bool,
    f: impl Fn(&Value) -> Result<Value, EditError>,
) -> Result<(), EditError> {
    let paths = expand_iter_paths(steps, whole).map_err(|e| EditError::new(e.to_string()))?;
    if paths.is_empty() && create {
        return Err(EditError::new("cannot create through `[]`"));
    }
    for path in &paths {
        let current = doc
            .value_at(path)
            .ok_or_else(|| EditError::new("path not found"))?;
        let value = f(&current)?;
        doc.set(path, &value)?;
    }
    Ok(())
}

/// A `Value` as a TOML `Item` for setting a key (nested objects become inline
/// tables, matching "set this value here").
pub(crate) fn value_to_item(value: &Value) -> Result<Item, EditError> {
    Ok(Item::Value(value_to_toml(value)?))
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

/// Replace `arr` with `new` by appending, when `new` extends it (its first
/// elements equal the current ones): the kept elements stay byte-for-byte and
/// each extra one goes in through [`push_in_layout`], so `.a += [x]` on a
/// one-item-per-line array adds one line. Returns `false`, touching nothing,
/// when `new` is not an extension; the caller then rewrites the array.
pub(crate) fn extend_in_layout(arr: &mut Array, new: &[Value]) -> Result<bool, EditError> {
    if new.len() < arr.len()
        || arr
            .iter()
            .zip(new)
            .any(|(old, new)| crate::project::toml_value_to_value(old) != *new)
    {
        return Ok(false);
    }
    let extra = new[arr.len()..]
        .iter()
        .map(value_to_toml)
        .collect::<Result<Vec<_>, _>>()?;
    for v in extra {
        push_in_layout(arr, v);
    }
    Ok(true)
}

/// [`extend_in_layout`] for an array of tables: when `new` extends `aot` with
/// more tables, each goes in as its own `[[key]]` block, rather than the whole
/// array collapsing to an inline array of inline tables.
pub(crate) fn extend_aot(aot: &mut ArrayOfTables, new: &[Value]) -> Result<bool, EditError> {
    if new.len() < aot.len()
        || aot
            .iter()
            .zip(new)
            .any(|(old, new)| crate::project::table_to_value(old) != *new)
        || !new[aot.len()..]
            .iter()
            .all(|v| matches!(v, Value::Object(_)))
    {
        return Ok(false);
    }
    let extra = new[aot.len()..]
        .iter()
        .map(value_to_table)
        .collect::<Result<Vec<_>, _>>()?;
    for t in extra {
        aot.push(t);
    }
    Ok(true)
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
    let resolved = if idx < 0 { len as i64 + idx } else { idx };
    if resolved < 0 || resolved as usize > len {
        return Err(EditError::new(format!(
            "array index {idx} out of range (length {len}); append with index {len}"
        )));
    }
    Ok(resolved as usize)
}

/// Resolve `idx` against `len` for a delete (no append); out of range yields
/// `None`, a jq-style no-op.
pub(crate) fn resolve_del_index(idx: i64, len: usize) -> Option<usize> {
    let resolved = if idx < 0 { len as i64 + idx } else { idx };
    (resolved >= 0 && (resolved as usize) < len).then_some(resolved as usize)
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
        arr.remove(at);
    }
}
