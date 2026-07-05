//! Format-preserving edits and conversion emit, backed by `toml_edit`.

use crate::Toml;
use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value as TomlValue};

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
/// objects stay inline). Returns text and warnings (none - TOML holds nesting,
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
