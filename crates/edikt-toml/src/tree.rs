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
    /// A collection over a collection is diffed by `edikt_core::assign`
    /// (#117), so a `[table]` given a new object stays a table and an array
    /// that grows appends in its own layout.
    pub fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        edikt_core::assign(self, path, value)
    }

    /// The primitive behind [`Toml::set`], one value written in place.
    pub(crate) fn set_value(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        let Some((last, parent)) = path.split_last() else {
            return self.set_root(value);
        };
        match last {
            Step::Field(key) => {
                let current = walk_tables_vivify(self.doc.as_table_mut(), parent)?;
                // An object over a non-table value takes the form its
                // siblings use (#120): dotted keys beside dotted keys, at
                // the old value's position; an inline table with no dotted
                // precedent. A missing key is not an overwrite, so it keeps
                // the inline table - and so does an array of tables, which
                // is a value, not a scalar to respell dotted.
                let dotted = matches!(value, Value::Object(m) if !m.is_empty())
                    && current
                        .get(key)
                        .is_some_and(|e| e.as_value().is_some_and(|v| !v.is_inline_table()))
                    && follows_dotted_style(current);
                // The key's own decor (the comment lines above it, the
                // spacing before `=`) opens the line it sits on; a dotted
                // parent key's is never printed, so it moves to the new
                // table's first line with it (#120).
                let key_decor = dotted
                    .then(|| current.key(key))
                    .flatten()
                    .map(|k| k.leaf_decor().clone());
                let new_item = if dotted {
                    edit::value_to_dotted_item(value)?
                } else {
                    edit::value_to_item(value)?
                };
                if let Some(existing) = current.get_mut(key) {
                    // An unchanged value keeps its bytes.
                    if project::item_to_value(existing).identical(value) {
                        return Ok(());
                    }
                    // Keep the existing value's decor (spacing + inline
                    // comment) and, for a string, its quote style.
                    let was_value = existing.is_value();
                    let replacement = match (existing.as_value(), new_item) {
                        (Some(old), Item::Value(new)) => Item::Value(edit::replacing(old, new)),
                        // The dotted table's lines take the old line's
                        // decor (spacing after `=`, an inline comment) and
                        // the old key's (the comment above it).
                        (Some(old), Item::Table(mut t)) => {
                            edit::carry_line_decor(old, &mut t);
                            if let Some(d) = &key_decor {
                                edit::carry_key_decor(d, &mut t);
                            }
                            Item::Table(t)
                        }
                        (_, new_item) => new_item,
                    };
                    let now_value = replacement.is_value();
                    *existing = replacement;
                    // The old key's decor now lives on the table's first
                    // line; its own copy would never print (#120).
                    if dotted && let Some(mut k) = current.key_mut(key) {
                        k.leaf_decor_mut().clear();
                    }
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

    /// Set the document itself (`. = v`, `. |= f`, jhheider/edikt#107). A
    /// TOML document is always a table, so anything but an object errors; an
    /// object is diffed key by key (#117), so untouched keys and tables keep
    /// their bytes.
    fn set_root(&mut self, value: &Value) -> Result<(), EditError> {
        if !matches!(value, Value::Object(_)) {
            return Err(EditError::new(format!(
                "a TOML document is a table, so `.` can only be set to an object (got {})",
                value.type_name()
            )));
        }
        // The root always holds an object, so this diffs, never comes back.
        edikt_core::assign(self, &[], value)
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
                let inline = is_inline_table(self.doc.as_item(), parent);
                let Some(current) = walk_tables(self.doc.as_table_mut(), parent) else {
                    return Ok(());
                };
                let last = current.iter().last().is_some_and(|(k, _)| k == key);
                let removed = current.remove(key);
                // An inline table's last value carries the space before `}`
                // in its suffix; the new last value takes it over.
                if inline
                    && last
                    && let Some(Item::Value(gone)) = removed
                    && let Some((_, Item::Value(now))) = current.iter_mut().last()
                {
                    let suffix = gone.decor().suffix().cloned().unwrap_or_default();
                    now.decor_mut().set_suffix(suffix);
                }
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

/// Whether `steps` from `root` lands on an inline table (`{ a = 1 }`), read
/// without creating anything along the way.
fn is_inline_table(root: &Item, steps: &[Step]) -> bool {
    fn walk<'a>(mut item: &'a Item, steps: &[Step]) -> Option<&'a Item> {
        for step in steps {
            item = match step {
                Step::Field(k) => item.get(k.as_str())?,
                Step::Index(i) => {
                    let len = match item {
                        Item::ArrayOfTables(aot) => aot.len(),
                        _ => item.as_array()?.len(),
                    };
                    item.get(edit::resolve_del_index(*i, len)?)?
                }
                _ => return None,
            };
        }
        Some(item)
    }
    walk(root, steps).is_some_and(Item::is_inline_table)
}

/// Walk `steps` from `root` **without** creating anything; returns `None` if
/// the path doesn't resolve to a table (a delete no-op).
pub(crate) fn walk_tables<'a>(
    root: &'a mut dyn TableLike,
    steps: &[Step],
) -> Option<&'a mut dyn TableLike> {
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
