//! edikt TOML format module.
//!
//! Backed by `toml_edit`, whose whole purpose is format-preserving TOML edits -
//! so edikt gets lossless TOML (comments, spacing, table layout) essentially for
//! free, and the moat holds without a hand-rolled CST.

mod comments;
mod edit;
mod project;

pub use comments::emit_commented;
pub use edit::{apply, emit};

// The edikt-core types that appear in this crate's own public API, re-exported
// so a dependent can call these methods without also taking a direct
// edikt-core dependency (jhheider/edikt#66). `parse` is aliased because this
// crate's own `parse` is the document parser.
use edikt_core::eval;
pub use edikt_core::{
    CommentKind, Commented, Document, EditError, Expr, Feature, Step, Value, json,
    parse as parse_expr,
};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value as TomlValue};

/// Comment kinds this format supports (empty => none); the comment
/// capability, subsuming the boolean `Feature::Comments`.
pub const COMMENT_KINDS: &[CommentKind] =
    &[CommentKind::Head, CommentKind::Inline, CommentKind::Foot];

/// Capabilities of TOML.
pub const FEATURES: &[Feature] = &[
    Feature::Comments,
    Feature::Nesting,
    Feature::Arrays,
    Feature::TypedScalars,
];

/// A parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg}")]
pub struct ParseError {
    pub msg: String,
}

/// A parsed TOML document, backed by a lossless `toml_edit` tree.
pub struct Toml {
    doc: DocumentMut,
    had_comments: bool,
    /// The parsed source (after any BOM). `toml_edit` writes every line
    /// ending as `\n` and always ends the file with one, so serializing
    /// restores the original's endings and missing final newline from it.
    original: String,
    /// The source opened with a UTF-8 byte-order mark (restored on output).
    bom: bool,
}

impl Toml {
    /// Set the value at `path`, format-preserving. Missing keys are created,
    /// including intermediate tables (jq's `.a.b = 1` auto-creates `.a`). A path
    /// ending in an array index sets, appends (`idx == len`), or auto-vivifies
    /// an array or array-of-tables element (`.foo[0] = { ... }` -> `[[foo]]`).
    pub fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
        let Some((last, parent)) = path.split_last() else {
            return Err(EditError::new("cannot set the whole document"));
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

/// Parse TOML source into a [`Toml`] document.
pub fn parse(src: &str) -> Result<Toml, ParseError> {
    let (bom, src) = edikt_core::text::split_bom(src);
    let doc = src
        .parse::<DocumentMut>()
        .map_err(|e| ParseError { msg: e.to_string() })?;
    let had_comments = has_comment_decor(&doc);
    Ok(Toml {
        doc,
        had_comments,
        original: src.to_string(),
        bom,
    })
}

/// Whether any decor in the document holds a comment. `toml_edit` keeps
/// comments only in decor strings (key, value, and table prefix/suffix, an
/// array's or inline table's inner whitespace, the document trailer), which
/// hold nothing but whitespace and comments, so a `#` there is a comment,
/// while a `#` inside a string value (`tag = "v #1"`) is never looked at.
fn has_comment_decor(doc: &DocumentMut) -> bool {
    fn raw(s: Option<&str>) -> bool {
        s.is_some_and(|s| s.contains('#'))
    }
    fn decor(d: &toml_edit::Decor) -> bool {
        raw(d.prefix().and_then(|r| r.as_str())) || raw(d.suffix().and_then(|r| r.as_str()))
    }
    fn key(k: &toml_edit::Key) -> bool {
        decor(k.leaf_decor()) || decor(k.dotted_decor())
    }
    fn value(v: &TomlValue) -> bool {
        if decor(v.decor()) {
            return true;
        }
        match v {
            TomlValue::Array(a) => raw(a.trailing().as_str()) || a.iter().any(value),
            TomlValue::InlineTable(t) => {
                raw(t.preamble().as_str())
                    || t.iter()
                        .any(|(k, _)| t.get_key_value(k).is_some_and(|(k, v)| key(k) || item(v)))
            }
            _ => false,
        }
    }
    fn item(i: &Item) -> bool {
        match i {
            Item::None => false,
            Item::Value(v) => value(v),
            Item::Table(t) => table(t),
            Item::ArrayOfTables(a) => a.iter().any(table),
        }
    }
    fn table(t: &Table) -> bool {
        decor(t.decor())
            || t.iter()
                .any(|(k, _)| t.get_key_value(k).is_some_and(|(k, i)| key(k) || item(i)))
    }
    raw(doc.trailing().as_str()) || table(doc.as_table())
}

impl Document for Toml {
    fn to_source(&self) -> String {
        let mut out = self.doc.to_string();
        // `toml_edit` terminates the last line unconditionally; a file that
        // didn't end with a newline keeps not ending with one.
        if !self.original.is_empty() && !self.original.ends_with('\n') && out.ends_with('\n') {
            out.pop();
        }
        // `toml_edit` also drops every `\r` it writes (decor and string reprs
        // alike), so put the original's line endings back, line by line.
        let out = edikt_core::text::restore_endings(&self.original, &out);
        edikt_core::text::with_bom(self.bom, out)
    }
    fn to_value(&self) -> Value {
        project::table_to_value(self.doc.as_table())
    }
    fn features(&self) -> &'static [Feature] {
        FEATURES
    }
    fn apply(&mut self, expr: &Expr) -> Result<Vec<String>, EditError> {
        edit::apply(self, expr).map(|()| Vec::new())
    }
    fn has_comments(&self) -> bool {
        self.had_comments
    }
    fn to_commented(&self) -> Option<edikt_core::Commented> {
        Some(comments::to_commented(&self.doc))
    }
    fn set_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
        text: &str,
    ) -> Result<Vec<String>, EditError> {
        let warnings = comments::set_node_comment(&mut self.doc, path, kind, text)?;
        self.had_comments = true;
        Ok(warnings)
    }
    fn delete_comment(
        &mut self,
        path: &[Step],
        kind: edikt_core::CommentKind,
    ) -> Result<(), EditError> {
        comments::delete_node_comment(&mut self.doc, path, kind)
    }
}

#[cfg(test)]
mod tests;
