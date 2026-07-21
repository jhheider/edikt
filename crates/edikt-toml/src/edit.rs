//! Format-preserving edits and conversion emit, backed by `toml_edit`.

use crate::Toml;
use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval};
use toml_edit::{
    Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value as TomlValue,
};

pub fn apply(doc: &mut Toml, expr: &Expr) -> Result<(), EditError> {
    match expr {
        Expr::Assign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let value = eval_one(rhs, &doc.to_value())?;
            doc.set(steps, &value)
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let current = doc
                .value_at(steps)
                .ok_or_else(|| EditError::new("path not found"))?;
            let value = eval_one(rhs, &current)?;
            doc.set(steps, &value)
        }
        Expr::AddAssign(lhs, rhs) => {
            let steps = assign_path(lhs)?;
            let current = doc
                .value_at(steps)
                .ok_or_else(|| EditError::new("path not found"))?;
            let addend = eval_one(rhs, &doc.to_value())?;
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
fn resolve_set_index(idx: i64, len: usize) -> Result<usize, EditError> {
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
fn resolve_del_index(idx: i64, len: usize) -> Option<usize> {
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
            arr.push(tv);
        } else if let Some(slot) = arr.get_mut(at) {
            *slot = tv;
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
