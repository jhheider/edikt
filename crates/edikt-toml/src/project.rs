//! Project a `toml_edit` document onto the [`Value`] model.

use edikt_core::Value;
use toml_edit::{Item, Value as TomlValue};

pub(crate) fn table_to_value(table: &dyn toml_edit::TableLike) -> Value {
    Value::Object(
        table
            .iter()
            .map(|(k, item)| (k.to_string(), item_to_value(item)))
            .collect(),
    )
}

pub(crate) fn item_to_value(item: &Item) -> Value {
    match item {
        Item::None => Value::Null,
        Item::Value(v) => toml_value_to_value(v),
        Item::Table(t) => table_to_value(t),
        Item::ArrayOfTables(aot) => Value::Array(aot.iter().map(|t| table_to_value(t)).collect()),
    }
}

pub(crate) fn toml_value_to_value(v: &TomlValue) -> Value {
    match v {
        TomlValue::String(s) => Value::Str(s.value().clone()),
        TomlValue::Integer(i) => Value::Int(*i.value()),
        TomlValue::Float(f) => Value::Float(*f.value()),
        TomlValue::Boolean(b) => Value::Bool(*b.value()),
        TomlValue::Datetime(d) => Value::Str(d.value().to_string()),
        TomlValue::Array(a) => Value::Array(a.iter().map(toml_value_to_value).collect()),
        TomlValue::InlineTable(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_string(), toml_value_to_value(v)))
                .collect(),
        ),
    }
}
