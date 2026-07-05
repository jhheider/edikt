//! TOML ↔ the uniform comment model, over `toml_edit`'s decor strings.
//!
//! Extraction reads comments out of decor: a key's (or table's) prefix holds
//! its head lines, a value's (or table header's) suffix holds its inline
//! comment, and the document trailing holds the foot. Inside arrays,
//! `toml_edit` stores a comment written after `elem,` in the *next* element's
//! prefix — the segment before that prefix's first newline is really the
//! previous element's inline comment, and extraction reassigns it.
//!
//! Emission builds the same `DocumentMut` the plain emitter would (identical
//! output when nothing is commented) and then sets decor where comments exist.
//! TOML holds every comment kind except one grammatical hole: comments cannot
//! exist *inside* an inline table (they are single-line by grammar), so
//! comments on values nested in arrays-of-objects hoist above their entry,
//! with a warning.

use crate::{edit, project};
use edikt_core::{Commented, CommentedNode, Comments, EditError, Value};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value as TomlValue};

// --- extraction ---------------------------------------------------------

/// Project the whole document to the commented value model.
pub(crate) fn to_commented(doc: &DocumentMut) -> Commented {
    let mut root = Commented {
        comments: Comments::default(),
        node: table_node(doc.as_table()),
    };
    let trailing = own_line_comments(raw(doc.trailing().as_str()));
    if !trailing.is_empty() {
        root.attach_trailing_foot(trailing);
    }
    root
}

fn table_node(t: &dyn TableLike) -> CommentedNode {
    let mut entries = Vec::new();
    for (k, item) in t.iter() {
        let mut c = item_commented(item);
        if let Some((key, _)) = t.get_key_value(k) {
            let mut head =
                own_line_comments(raw(key.leaf_decor().prefix().and_then(|r| r.as_str())));
            head.append(&mut c.comments.head);
            c.comments.head = head;
        }
        entries.push((k.to_string(), c));
    }
    CommentedNode::Object(entries)
}

fn item_commented(item: &Item) -> Commented {
    match item {
        Item::None => Commented::scalar(Value::Null),
        Item::Value(v) => {
            let mut c = toml_value_commented(v);
            if let Some(text) = same_line_comment(raw(v.decor().suffix().and_then(|r| r.as_str())))
            {
                c.comments.inline = Some(text);
            }
            c
        }
        Item::Table(t) => decorated_table(t),
        Item::ArrayOfTables(aot) => Commented {
            comments: Comments::default(),
            node: CommentedNode::Array(aot.iter().map(decorated_table).collect()),
        },
    }
}

/// A `[table]` (or one `[[table]]` occurrence) with its header decor attached.
fn decorated_table(t: &Table) -> Commented {
    Commented {
        comments: Comments {
            head: own_line_comments(raw(t.decor().prefix().and_then(|r| r.as_str()))),
            inline: same_line_comment(raw(t.decor().suffix().and_then(|r| r.as_str()))),
            foot: Vec::new(),
        },
        node: table_node(t),
    }
}

fn toml_value_commented(v: &TomlValue) -> Commented {
    match v {
        TomlValue::Array(a) => {
            let mut items: Vec<Commented> = Vec::new();
            for e in a.iter() {
                let (same_line, lines) =
                    split_prefix(raw(e.decor().prefix().and_then(|r| r.as_str())));
                if let Some(text) = same_line
                    && let Some(prev) = items.last_mut()
                {
                    prev.comments.inline = Some(text);
                }
                let mut c = toml_value_commented(e);
                let mut head = lines;
                head.append(&mut c.comments.head);
                c.comments.head = head;
                items.push(c);
            }
            // Text between the last element and `]`.
            let (same_line, lines) = split_prefix(raw(a.trailing().as_str()));
            let mut comments = Comments::default();
            match items.last_mut() {
                Some(last) => {
                    if let Some(text) = same_line {
                        last.comments.inline = Some(text);
                    }
                    last.comments.foot.extend(lines);
                }
                None => comments.foot = lines,
            }
            Commented {
                comments,
                node: CommentedNode::Array(items),
            }
        }
        // Comments cannot occur inside an inline table (single-line grammar).
        TomlValue::InlineTable(t) => Commented {
            comments: Comments::default(),
            node: CommentedNode::Object(
                t.iter()
                    .map(|(k, v)| (k.to_string(), toml_value_commented(v)))
                    .collect(),
            ),
        },
        scalar => Commented::scalar(project::toml_value_to_value(scalar)),
    }
}

fn raw(s: Option<&str>) -> &str {
    s.unwrap_or("")
}

/// Every `# …` line in a decor string, delimiter-stripped and trimmed.
fn own_line_comments(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let t = l.trim();
            t.starts_with('#')
                .then(|| t.trim_start_matches('#').trim().to_string())
        })
        .collect()
}

/// A `# …` comment with no newline before it (i.e. trailing on the same line).
fn same_line_comment(text: &str) -> Option<String> {
    let first = text.split('\n').next().unwrap_or("");
    let t = first.trim();
    t.starts_with('#')
        .then(|| t.trim_start_matches('#').trim().to_string())
}

/// Split a decor prefix into (comment on the previous element's line, own-line
/// comment lines that follow).
fn split_prefix(text: &str) -> (Option<String>, Vec<String>) {
    match text.split_once('\n') {
        Some((first, rest)) => (same_line_comment(first), own_line_comments(rest)),
        None => (same_line_comment(text), Vec::new()),
    }
}

// --- emission -----------------------------------------------------------

/// Emit a commented value as TOML: top-level (and nested) objects become
/// `[table]`s, exactly as the plain emitter shapes them, with comments set via
/// decor. Returns the text and any warnings.
pub fn emit_commented(c: &Commented) -> Result<(String, Vec<String>), EditError> {
    let CommentedNode::Object(entries) = &c.node else {
        return Err(EditError::new(
            "TOML output requires a table (top-level object)",
        ));
    };
    let mut displaced = false;
    let mut doc = DocumentMut::new();
    let mut pending = c.comments.head.clone();
    build_table_body(doc.as_table_mut(), entries, &mut pending, &mut displaced)?;

    // Whatever is still pending (the last entry's foot), plus the root's own
    // trailers, ends the document.
    let mut trail = pending;
    trail.extend(c.comments.inline.clone());
    trail.extend(c.comments.foot.iter().cloned());
    if !trail.is_empty() {
        doc.set_trailing(comment_block(&trail, ""));
    }

    let mut warnings = Vec::new();
    if displaced {
        warnings.push(
            "comments inside inline values were moved above their entry (TOML allows none there)"
                .to_string(),
        );
    }
    Ok((doc.to_string(), warnings))
}

/// Fill `table` from commented entries. Values are attached first and tables
/// after, matching TOML's render order (root key-values precede `[table]`
/// headers), so pending foot comments land on the item that actually renders
/// next. `pending` carries foot lines forward across entries.
fn build_table_body(
    table: &mut Table,
    entries: &[(String, Commented)],
    pending: &mut Vec<String>,
    displaced: &mut bool,
) -> Result<(), EditError> {
    // Pass 1: plain values (rendered before any table header).
    for (k, v) in entries {
        if matches!(v.node, CommentedNode::Object(_)) {
            continue;
        }
        let (tv, mut hoisted) = build_value(v, displaced)?;
        table.insert(k, Item::Value(tv));
        let mut head = std::mem::take(pending);
        head.append(&mut hoisted);
        head.extend(v.comments.head.iter().cloned());
        if !head.is_empty()
            && let Some(mut key) = table.key_mut(k)
        {
            key.leaf_decor_mut().set_prefix(comment_block(&head, ""));
        }
        if let Some(inline) = &v.comments.inline
            && let Some(val) = table.get_mut(k).and_then(|i| i.as_value_mut())
        {
            val.decor_mut()
                .set_suffix(format!(" # {}", sanitize(inline)));
        }
        pending.extend(v.comments.foot.iter().cloned());
    }
    // Pass 2: nested objects, as [tables].
    for (k, v) in entries {
        let CommentedNode::Object(inner) = &v.node else {
            continue;
        };
        let mut sub = Table::new();
        build_table_body(&mut sub, inner, pending, displaced)?;
        table.insert(k, Item::Table(sub));
        let sub = table.get_mut(k).and_then(|i| i.as_table_mut()).unwrap();
        let mut head = std::mem::take(pending);
        head.extend(v.comments.head.iter().cloned());
        if !head.is_empty() {
            // Keep the default leading newline a fresh table renders with.
            let default = sub
                .decor()
                .prefix()
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            sub.decor_mut()
                .set_prefix(format!("{default}{}", comment_block(&head, "")));
        }
        if let Some(inline) = &v.comments.inline {
            sub.decor_mut()
                .set_suffix(format!(" # {}", sanitize(inline)));
        }
        *pending = v.comments.foot.clone();
    }
    Ok(())
}

/// A commented value (scalar, array, or object-in-value position) to a
/// `TomlValue`. Objects here become inline tables (mirroring the plain
/// emitter); comments inside them cannot be placed and hoist upward.
fn build_value(c: &Commented, displaced: &mut bool) -> Result<(TomlValue, Vec<String>), EditError> {
    match &c.node {
        CommentedNode::Scalar(v) => Ok((edit::value_to_toml(v)?, Vec::new())),
        CommentedNode::Object(entries) => {
            // The node's own comments stay with the caller; only the interior
            // ones (inside the inline table) have no legal home and hoist.
            let mut hoisted = Vec::new();
            for (_, v) in entries {
                collect_comments(v, &mut hoisted);
            }
            if !hoisted.is_empty() {
                *displaced = true;
            }
            Ok((edit::value_to_toml(&c.to_value())?, hoisted))
        }
        CommentedNode::Array(items) => {
            let mut built = Vec::new();
            let mut any_comments = false;
            for item in items {
                let (tv, mut hoisted) = build_value(item, displaced)?;
                let mut head = std::mem::take(&mut hoisted);
                head.extend(item.comments.head.iter().cloned());
                any_comments |= !head.is_empty()
                    || item.comments.inline.is_some()
                    || !item.comments.foot.is_empty();
                built.push((tv, head, item.comments.clone()));
            }
            let mut arr = Array::new();
            for (tv, _, _) in &built {
                arr.push(tv.clone());
            }
            if any_comments {
                // Multi-line form: each element on its own line so head lines
                // fit above it and an inline comment fits after the comma
                // (which lives in the *next* element's prefix, or in the
                // array's trailing for the last element).
                let mut carry = String::new(); // inline+foot of the previous element
                for (i, (_, head, comments)) in built.iter().enumerate() {
                    let mut prefix = std::mem::take(&mut carry);
                    for l in head {
                        prefix.push_str("\n  # ");
                        prefix.push_str(&sanitize(l));
                    }
                    prefix.push_str("\n  ");
                    if let Some(v) = arr.get_mut(i) {
                        v.decor_mut().set_prefix(prefix);
                        v.decor_mut().set_suffix("");
                    }
                    if let Some(inline) = &comments.inline {
                        carry.push_str(&format!(" # {}", sanitize(inline)));
                    }
                    for l in &comments.foot {
                        carry.push_str("\n  # ");
                        carry.push_str(&sanitize(l));
                    }
                }
                arr.set_trailing(format!("{carry}\n"));
                arr.set_trailing_comma(true);
            }
            Ok((TomlValue::Array(arr), Vec::new()))
        }
    }
}

/// Gather every comment in a subtree, in document order (for hoisting out of
/// positions TOML's grammar cannot comment).
fn collect_comments(c: &Commented, out: &mut Vec<String>) {
    out.extend(c.comments.head.iter().cloned());
    out.extend(c.comments.inline.clone());
    match &c.node {
        CommentedNode::Scalar(_) => {}
        CommentedNode::Array(items) => items.iter().for_each(|i| collect_comments(i, out)),
        CommentedNode::Object(entries) => {
            entries.iter().for_each(|(_, v)| collect_comments(v, out));
        }
    }
    out.extend(c.comments.foot.iter().cloned());
}

/// Render comment lines as a `# …\n` block, each line prefixed by `indent`.
fn comment_block(lines: &[String], indent: &str) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(indent);
        out.push_str("# ");
        out.push_str(&sanitize(l));
        out.push('\n');
    }
    out
}

fn sanitize(line: &str) -> String {
    line.replace(['\n', '\r'], " ")
}
