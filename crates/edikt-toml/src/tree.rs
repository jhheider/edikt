//! Setting and deleting by path over `toml_edit`'s table tree: the walks
//! that find (or create) the table a path lands in, and the `Toml` methods
//! built on them.

use crate::{Toml, edit, project};
use edikt_core::{Document, EditError, Expr, Step, Value, eval};
use toml_edit::{Item, Table, TableLike, Value as TomlValue};

impl Toml {
    /// Set the value at `path`, format-preserving. Missing keys are created,
    /// including intermediate tables (jq's `.a.b = 1` auto-creates `.a`). A path
    /// ending in an array index sets, appends (`idx == len`), or auto-vivifies
    /// an array or array-of-tables element (`.foo[0] = { ... }` -> `[[foo]]`).
    pub fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        let Some((last, parent)) = path.split_last() else {
            return self.set_root(value);
        };
        match last {
            Step::Field(key) => {
                let current = walk_tables_vivify(self.doc.as_table_mut(), parent)?;
                if let Some(existing) = current.get(key) {
                    // An unchanged value keeps its bytes (an identity update
                    // must not restyle a `[table]` as an inline one).
                    if project::item_to_value(existing).identical(value) {
                        return Ok(());
                    }
                    // A `[table]` given a new object is updated key by key,
                    // so it stays a table and its untouched keys keep their
                    // bytes.
                    if let (Some(table), Value::Object(entries)) = (existing.as_table(), value) {
                        let stale: Vec<String> = table
                            .iter()
                            .map(|(k, _)| k.to_string())
                            .filter(|k| entries.iter().all(|(n, _)| n != k))
                            .collect();
                        return self.update_table(path, entries, &stale);
                    }
                }
                let current = walk_tables_vivify(self.doc.as_table_mut(), parent)?;
                let new_item = edit::value_to_item(value)?;
                if let Some(existing) = current.get_mut(key) {
                    // A new array that extends the old one appends in the
                    // array's own layout (#91) instead of rewriting it.
                    if let Value::Array(new) = value {
                        let extended = match existing {
                            Item::Value(TomlValue::Array(old)) => edit::extend_in_layout(old, new)?,
                            Item::ArrayOfTables(old) => edit::extend_aot(old, new)?,
                            _ => false,
                        };
                        if extended {
                            return Ok(());
                        }
                    }
                    // Keep the existing value's decor (spacing + inline
                    // comment) and, for a string, its quote style.
                    let was_value = existing.is_value();
                    let replacement = match (existing.as_value(), new_item) {
                        (Some(old), Item::Value(new)) => Item::Value(edit::replacing(old, new)),
                        (_, new_item) => new_item,
                    };
                    let now_value = replacement.is_value();
                    *existing = replacement;
                    // A `[table]` / `[[array]]` header's key carries no
                    // spacing; as a `key = value` line it takes the default.
                    if !was_value
                        && now_value
                        && let Some(mut k) = current.key_mut(key)
                    {
                        k.leaf_decor_mut().clear();
                    }
                } else {
                    current.insert(key, new_item);
                }
                Ok(())
            }
            Step::Index(n) => {
                // The array's key is the step just before the index; walk the
                // rest as tables, then set/append the element.
                let Some((Step::Field(arr_key), table_path)) = parent.split_last() else {
                    return Err(EditError::new(
                        "an array index needs an array key before it, e.g. `.foo[0]`",
                    ));
                };
                let current = walk_tables_vivify(self.doc.as_table_mut(), table_path)?;
                edit::set_array_element(current, arr_key, *n, value)
            }
            _ => Err(EditError::new(
                "TOML set targets object keys or array indices",
            )),
        }
    }

    /// Set the document itself (`. = v`, `. |= f`, jhheider/edikt#107). The
    /// root is a standard table, so it is updated the same way: an unchanged
    /// value is a no-op, and any other table is brought to `value` key by key,
    /// so untouched keys and tables keep their bytes. A TOML document is
    /// always a table, so anything else errors.
    fn set_root(&mut self, value: &Value) -> Result<(), EditError> {
        let Value::Object(entries) = value else {
            return Err(EditError::new(format!(
                "a TOML document is a table, so `.` can only be set to an object (got {})",
                value.type_name()
            )));
        };
        if self.to_value().identical(value) {
            return Ok(());
        }
        let stale: Vec<String> = self
            .doc
            .iter()
            .map(|(k, _)| k.to_string())
            .filter(|k| entries.iter().all(|(n, _)| n != k))
            .collect();
        self.update_table(&[], entries, &stale)
    }

    /// Bring the standard table at `path` to `entries`: each key set in place
    /// (recursively, so a sub-table stays a table too), and each `stale` key
    /// deleted.
    fn update_table(
        &mut self,
        path: &[Step],
        entries: &[(String, Value)],
        stale: &[String],
    ) -> Result<(), EditError> {
        let at = |k: &str| {
            let mut p = path.to_vec();
            p.push(Step::Field(k.to_string()));
            p
        };
        for k in stale {
            self.delete(&at(k))?;
        }
        for (k, v) in entries {
            self.set(&at(k), v)?;
        }
        Ok(())
    }

    /// The value at `path`, or `None`.
    pub fn value_at(&self, path: &[Step]) -> Option<Value> {
        eval(&Expr::Path(path.to_vec()), &self.to_value())
            .ok()?
            .into_iter()
            .next()
    }

    /// Delete the key or array element at `path` (a missing target is a no-op).
    /// A `[]` in `path` deletes **every** iterated element/table-entry (jq's
    /// `del(.a[])` empties the collection).
    pub fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
        if path.contains(&Step::Iterate) {
            let whole = self.to_value();
            let paths = edikt_core::expand_delete_paths(path, &whole)?;
            for p in &paths {
                self.delete(p)?;
            }
            return Ok(());
        }
        let Some((last, parent)) = path.split_last() else {
            return Ok(());
        };
        match last {
            Step::Field(key) => {
                let Some(current) = walk_tables(self.doc.as_table_mut(), parent) else {
                    return Ok(());
                };
                current.remove(key);
                Ok(())
            }
            Step::Index(n) => {
                let Some((Step::Field(arr_key), table_path)) = parent.split_last() else {
                    return Ok(());
                };
                let Some(current) = walk_tables(self.doc.as_table_mut(), table_path) else {
                    return Ok(());
                };
                edit::delete_array_element(current, arr_key, *n);
                Ok(())
            }
            _ => Err(EditError::new(
                "TOML del targets object keys or array indices",
            )),
        }
    }
}

/// Descend into element `idx` of the array at `item`, which is either an
/// array-of-tables (`[[key]]` blocks) or an array of inline tables.
///
/// `resolve` is what decides how a negative or past-the-end index is treated,
/// so a walk on the way to a `set` can append where a walk on the way to a
/// `del` must not.
fn descend_index<'a>(
    item: &'a mut Item,
    key: &str,
    idx: i64,
    resolve: impl Fn(i64, usize) -> Result<Option<usize>, EditError>,
) -> Result<&'a mut dyn TableLike, EditError> {
    // Shape is checked immutably first: taking the mutable borrow inside an
    // `if let` would hold it for the function's whole lifetime, so the second
    // branch could not borrow `item` again.
    if item.as_array_of_tables().is_some() {
        let aot = item
            .as_array_of_tables_mut()
            .expect("just checked it is an array of tables");
        let len = aot.len();
        let Some(at) = resolve(idx, len)? else {
            return Err(EditError::new(format!(
                "`{key}[{idx}]` is out of range (length {len})"
            )));
        };
        if at == len {
            aot.push(Table::new());
        }
        return Ok(aot.get_mut(at).expect("index resolved within the array"));
    }
    if item.as_array().is_some() {
        let arr = item.as_array_mut().expect("just checked it is an array");
        let len = arr.len();
        let Some(at) = resolve(idx, len)? else {
            return Err(EditError::new(format!(
                "`{key}[{idx}]` is out of range (length {len})"
            )));
        };
        return arr
            .get_mut(at)
            .and_then(TomlValue::as_inline_table_mut)
            .map(|t| t as &mut dyn TableLike)
            .ok_or_else(|| EditError::new(format!("`{key}[{idx}]` is not a table")));
    }
    Err(EditError::new(format!("`{key}` is not an array")))
}

/// Walk `steps` from `root`, **creating** missing intermediate tables
/// (implicit, so a table that only holds sub-tables emits no bare header).
///
/// A field may be followed by an index, which descends into that element of an
/// array-of-tables: `.bin[0].name = "x"` edits one `[[bin]]` block in place
/// rather than making the caller rewrite the whole element.
fn walk_tables_vivify<'a>(
    root: &'a mut dyn TableLike,
    steps: &[Step],
) -> Result<&'a mut dyn TableLike, EditError> {
    let mut current = root;
    let mut i = 0;
    while i < steps.len() {
        let Step::Field(k) = &steps[i] else {
            return Err(EditError::new(
                "TOML paths for set are object keys, each with an optional array index",
            ));
        };
        if let Some(Step::Index(n)) = steps.get(i + 1) {
            let item = current
                .get_mut(k)
                .ok_or_else(|| EditError::new(format!("`{k}` does not exist")))?;
            // Appending at `len` matches what `arr[len] = v` already does, so
            // walking through an index cannot create more than one element.
            current = descend_index(item, k, *n, |idx, len| {
                edit::resolve_set_index(idx, len).map(Some)
            })?;
            i += 2;
            continue;
        }
        if current.get(k).is_none() {
            let mut t = Table::new();
            t.set_implicit(true);
            // Match the surrounding style: under a dotted table, or beside a
            // dotted sibling (`edition.workspace = true`), the new table is a
            // dotted key too, not a fresh `[a.b]` header.
            t.set_dotted(follows_dotted_style(current));
            current.insert(k, Item::Table(t));
        }
        let item = current
            .get_mut(k)
            .expect("key exists: it was just inserted or already present");
        current = item
            .as_table_like_mut()
            .ok_or_else(|| EditError::new(format!("`{k}` is not a table")))?;
        i += 1;
    }
    Ok(current)
}

/// Does a table created in `parent` belong as a dotted key? Yes when `parent`
/// is itself a dotted table, or when any of its sub-tables is spelled dotted.
/// With no dotted precedent, a new table keeps its own `[header]`.
fn follows_dotted_style(parent: &dyn TableLike) -> bool {
    parent.is_dotted()
        || parent
            .iter()
            .any(|(_, item)| matches!(item, Item::Table(t) if t.is_dotted()))
}

/// Walk `steps` from `root` **without** creating anything; returns `None` if
/// the path doesn't resolve to a table (a delete no-op).
fn walk_tables<'a>(root: &'a mut dyn TableLike, steps: &[Step]) -> Option<&'a mut dyn TableLike> {
    let mut current = root;
    let mut i = 0;
    while i < steps.len() {
        let Step::Field(k) = &steps[i] else {
            return None;
        };
        if let Some(Step::Index(n)) = steps.get(i + 1) {
            let item = current.get_mut(k)?;
            current = descend_index(item, k, *n, |idx, len| {
                Ok(edit::resolve_del_index(idx, len))
            })
            .ok()?;
            i += 2;
            continue;
        }
        current = current.get_mut(k)?.as_table_like_mut()?;
        i += 1;
    }
    Some(current)
}
