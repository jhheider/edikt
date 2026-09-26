//! The splices that write a mapping or sequence into the source
//! (jhheider/edikt#83): replacing a value with one of a different shape,
//! adding a key whose value is a collection, and appending collection items.
//!
//! Each builder returns one `(range, text)` splice and leaves every byte
//! outside `range` alone. The layout rules, all following the file:
//!
//! - **Flow stays flow.** A slot inside `[...]`/`{...}`, or a value that
//!   already was a flow collection, takes the single-line flow spelling.
//! - **Block under block.** Anywhere else a non-empty collection is written as
//!   block lines, indented per [`Indent::infer`]. An empty one is `[]`/`{}`,
//!   the only spelling YAML has for it.
//! - **A comment stays on its line.** The comment after a replaced scalar
//!   stays where it was (`key:  # note` above the new block, or after the
//!   first line of a compact `- k: v` item), and a key's comment stays on the
//!   key line when its block becomes a scalar. Comments *inside* a replaced
//!   block go with it, as `del(.a[])` does.

use edikt_core::{EditError, Value};
use std::ops::Range;

use crate::block::{BlockScalar, Chomp};
use crate::compose::{Node, NodeKind};
use crate::edit::{block_end, ends_with_newline, insert_end, newline, trim_newline};
use crate::layout::{
    Indent, block_lines, child_offset, column, content_start, flow_text, inline_text, is_flow,
    line_end, line_start,
};
use crate::scalar::{kept_properties, split_properties};

/// Where the replaced value sits, which decides how its replacement is laid
/// out around it.
#[derive(Clone, Copy)]
pub(crate) enum Slot {
    /// The document's root node.
    Root,
    /// A mapping entry's value; `key` is the key's byte span.
    Value { key: (usize, usize) },
    /// A sequence item, after its `-`.
    Item,
}

/// A splice: replace `range` of the source with the string.
pub(crate) type Splice = (Range<usize>, String);

/// Replace a single-line scalar (or an alias) with the non-empty collection
/// `value`, in block context. `props` are the kept anchor/tag.
pub(crate) fn scalar_to_block(
    source: &str,
    node: &Node,
    slot: Slot,
    props: &str,
    value: &Value,
    indent: Indent,
) -> Result<Splice, EditError> {
    let nl = newline(source);
    let props = props.trim_end();
    let le = line_end(source, node.span.end);
    let tail = &source[node.span.end..le];
    let comment = if tail.trim_start().starts_with('#') {
        tail
    } else {
        ""
    };
    let mut lines = Vec::new();
    match slot {
        Slot::Value {
            key: (key_start, _),
        } => {
            let col = column(source, key_start) + child_offset(value, indent);
            block_lines(value, col, indent, &mut lines)?;
            if line_start(source, node.span.start) == line_start(source, key_start) {
                // `key: old  # c` -> `key:  # c` with the block below it. Only
                // the separating space before the old token goes with it.
                let from = source[..node.span.start]
                    .trim_end_matches([' ', '\t'])
                    .len();
                let head = if props.is_empty() {
                    String::new()
                } else {
                    format!(" {props}")
                };
                Ok((from..le, format!("{head}{comment}{nl}{}", lines.join(nl))))
            } else {
                // The value sits on a line of its own under the key: the block
                // takes over that line.
                if !props.is_empty() {
                    return Err(EditError::new(
                        "cannot replace a value with an anchor or tag on its own line \
                         with a mapping or sequence yet",
                    ));
                }
                Ok((
                    line_start(source, node.span.start)..node.span.end,
                    lines.join(nl),
                ))
            }
        }
        Slot::Item | Slot::Root => {
            // An empty item (`-` alone) has no content column yet: its content
            // goes one space past the dash.
            let empty = node.span.is_empty();
            let col = column(source, node.span.start) + usize::from(empty);
            if matches!(slot, Slot::Root) && (col != 0 || !props.is_empty()) {
                return Err(EditError::new(
                    "cannot replace a document root that shares its line with `---` \
                     or a property with a mapping or sequence yet",
                ));
            }
            block_lines(value, col, indent, &mut lines)?;
            let (first, rest) = lines.split_first().expect("a non-empty collection renders");
            let rest: String = rest.iter().map(|l| format!("{nl}{l}")).collect();
            let text = if props.is_empty() {
                // Compact: the first line takes the old token's place, after
                // the dash, and the comment stays on that line.
                let lead = if empty { " " } else { "" };
                format!("{lead}{}{comment}{rest}", &first[col..])
            } else {
                // `- &a old` -> `- &a` with the block below: on the dash line
                // the property would anchor the first key, not the collection.
                format!("{props}{comment}{nl}{first}{rest}")
            };
            Ok((node.span.start..le, text))
        }
    }
}

/// Replace a block collection with `value`: a scalar or empty collection goes
/// inline where the block's value starts, a non-empty collection is laid out
/// over the block's lines. The same kind keeps the old block's own column;
/// a different kind indents per the file's style.
pub(crate) fn block_replace(
    source: &str,
    node: &Node,
    slot: Slot,
    value: &Value,
    indent: Indent,
) -> Result<Splice, EditError> {
    let nl = newline(source);
    let start = content_start(source, node);
    let end = trim_newline(source, block_end(source, node));
    if let Some(text) = inline_text(value) {
        let text = text?;
        return Ok(match slot {
            Slot::Value { key: (_, key_end) } => {
                // The value moves up onto the key line, after the key's anchor
                // or tag, ahead of the comment already there.
                let colon = colon_end(source, key_end)?;
                let tail = &source[colon..line_end(source, colon)];
                let (props, rest) = split_properties(tail.trim_start_matches([' ', '\t']));
                let props = kept_properties(props, value);
                let props = props.trim_end();
                let head = if props.is_empty() {
                    String::new()
                } else {
                    format!("{props} ")
                };
                let comment = match tail.find('#') {
                    Some(i) if rest.starts_with('#') => {
                        let gap = tail[..i].len() - tail[..i].trim_end_matches([' ', '\t']).len();
                        &tail[i - gap..]
                    }
                    _ => "",
                };
                (colon..end, format!(" {head}{text}{comment}"))
            }
            Slot::Item | Slot::Root => (start..end, text),
        });
    }
    let same_kind = matches!(
        (&node.kind, value),
        (NodeKind::Mapping(_), Value::Object(_)) | (NodeKind::Sequence(_), Value::Array(_))
    );
    let mut lines = Vec::new();
    match slot {
        Slot::Value {
            key: (key_start, _),
        } => {
            let col = if same_kind {
                column(source, start)
            } else {
                column(source, key_start) + child_offset(value, indent)
            };
            block_lines(value, col, indent, &mut lines)?;
            Ok((line_start(source, start)..end, lines.join(nl)))
        }
        Slot::Item => {
            let col = column(source, start);
            block_lines(value, col, indent, &mut lines)?;
            lines[0] = lines[0][col..].to_string();
            Ok((start..end, lines.join(nl)))
        }
        Slot::Root => {
            block_lines(value, column(source, start), indent, &mut lines)?;
            Ok((line_start(source, start)..end, lines.join(nl)))
        }
    }
}

/// Add `key: value` to the mapping `parent`: into a flow mapping in flow
/// style, else as block lines after the last entry, at its column.
pub(crate) fn new_key(
    source: &str,
    parent: &Node,
    key: &str,
    value: &Value,
    indent: Indent,
) -> Result<Splice, EditError> {
    let NodeKind::Mapping(entries) = &parent.kind else {
        return Err(EditError::new("can only add a key to a mapping"));
    };
    if is_flow(source, parent) {
        let entry = format!(
            "{}: {}",
            flow_text(&Value::Str(key.into()))?,
            flow_text(value)?
        );
        return flow_insert(source, parent, &entry);
    }
    let Some(last) = entries.last() else {
        return Err(EditError::new("cannot add a key to an empty mapping yet"));
    };
    // The last key's column, not its line's literal prefix: in a compact
    // `- a: 1` item that prefix holds the dash, and copying it would start a
    // new item instead of adding a key.
    let col = column(source, last.key_span.start);
    let mut lines = Vec::new();
    let entry = Value::Object(vec![(key.to_string(), value.clone())]);
    block_lines(&entry, col, indent, &mut lines)?;
    let at = insert_end(source, &last.value);
    Ok(after_block(source, at, &lines, &last.value))
}

/// Append `items` to the sequence `node`: into a flow sequence in flow style,
/// else as block items at its dash column.
pub(crate) fn append_items(
    source: &str,
    node: &Node,
    items: &[Value],
    indent: Indent,
) -> Result<Splice, EditError> {
    if is_flow(source, node) {
        let items: Result<Vec<_>, _> = items.iter().map(flow_text).collect();
        return flow_insert(source, node, &items?.join(", "));
    }
    let col = column(source, content_start(source, node));
    let mut lines = Vec::new();
    block_lines(&Value::Array(items.to_vec()), col, indent, &mut lines)?;
    Ok(after_block(source, insert_end(source, node), &lines, node))
}

/// Insert `lines` as whole lines at `at` (the start of the line after
/// `last`, the node they follow), keeping a file without a trailing newline
/// without one.
///
/// A block scalar ending such a file has no final line break in its value;
/// the line break the insertion puts after it would add one, so its header
/// takes strip chomping (`|-`) to keep the value as it was (#111).
fn after_block(source: &str, at: usize, lines: &[String], last: &Node) -> Splice {
    let nl = newline(source);
    let body = lines.join(nl);
    if at < source.len() || ends_with_newline(source) {
        return (at..at, format!("{body}{nl}"));
    }
    let leaf = last_leaf(last);
    if let Some(block) = BlockScalar::of(source, leaf)
        && block.chomp != Chomp::Strip
        && matches!(&leaf.kind, NodeKind::Scalar(Value::Str(s)) if !s.is_empty())
    {
        let header = block.stripped_header(source);
        let range = block.header.start..at;
        let kept = &source[block.header.end..at];
        return (range, format!("{header}{kept}{nl}{body}"));
    }
    (at..at, format!("{nl}{body}"))
}

/// The node that ends `node` in the source: its last item's or last
/// value's, all the way down.
fn last_leaf(node: &Node) -> &Node {
    match &node.kind {
        NodeKind::Sequence(items) => items.last().map_or(node, last_leaf),
        NodeKind::Mapping(entries) => entries.last().map_or(node, |e| last_leaf(&e.value)),
        NodeKind::Scalar(_) => node,
    }
}

/// Insert `elems` (already spelled, comma-joined) before the closing bracket
/// of a single-line flow collection, matching its trailing-comma habit.
fn flow_insert(source: &str, node: &Node, elems: &str) -> Result<Splice, EditError> {
    let (props, _) = split_properties(&source[node.span.clone()]);
    let open = node.span.start + props.len();
    let close = node.span.end - 1;
    if source[open..node.span.end].contains('\n') {
        return Err(EditError::new(
            "cannot add to a multi-line flow collection yet",
        ));
    }
    let inner = &source[open + 1..close];
    let body = inner.trim_end();
    let at = open + 1 + body.len();
    Ok(if body.trim_start().is_empty() {
        (open + 1..close, elems.to_string())
    } else if body.ends_with(',') {
        (at..at, format!(" {elems},"))
    } else {
        (at..at, format!(", {elems}"))
    })
}

/// The byte just past the `:` that follows a mapping key ending at `key_end`.
fn colon_end(source: &str, key_end: usize) -> Result<usize, EditError> {
    let rest = &source[key_end..];
    let skipped = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    if rest[skipped..].starts_with(':') {
        Ok(key_end + skipped + 1)
    } else {
        Err(EditError::new(
            "cannot restructure the value of a complex (`?`) key",
        ))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod corpus {
    use crate::edit::Strictness;
    use crate::{Document, Step, Value, json, parse};

    /// Every path to every node of `v`, the root included.
    fn paths(v: &Value, at: &mut Vec<Step>, out: &mut Vec<Vec<Step>>) {
        out.push(at.clone());
        match v {
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    at.push(Step::Index(i as i64));
                    paths(item, at, out);
                    at.pop();
                }
            }
            Value::Object(entries) => {
                for (k, item) in entries {
                    at.push(Step::Field(k.clone()));
                    paths(item, at, out);
                    at.pop();
                }
            }
            _ => {}
        }
    }

    /// `v` with the node at `path` replaced by `new`.
    fn set_in(v: &mut Value, path: &[Step], new: &Value) {
        let Some((step, rest)) = path.split_first() else {
            *v = new.clone();
            return;
        };
        match (step, v) {
            (Step::Index(i), Value::Array(items)) => set_in(&mut items[*i as usize], rest, new),
            (Step::Field(k), Value::Object(entries)) => {
                let e = entries.iter_mut().find(|(ek, _)| ek == k).unwrap();
                set_in(&mut e.1, rest, new);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn every_fixture_path_takes_every_shape() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/yaml");
        let shapes = [
            json!([1, {"a": "x y", "b": [true, null]}]),
            json!({"k": {"l": [1.5]}, "m": "#no comment"}),
            json!("plain"),
            json!([]),
        ];
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let src = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            let docs = parse(&src).unwrap();
            for idx in 0..docs.docs.len() {
                let mut all = Vec::new();
                paths(&docs.doc_value(idx), &mut Vec::new(), &mut all);
                for path in &all {
                    for shape in &shapes {
                        let mut doc = parse(&src).unwrap();
                        let before = doc.doc_value(idx);
                        match doc.set(idx, path, shape, Strictness::Strict) {
                            Ok(()) => {}
                            // An element reached only through an alias or a
                            // merge has no bytes of its own. Anything else
                            // is a failure, block scalars included (#89).
                            Err(e) if e.to_string().contains("path not found") => continue,
                            Err(e) => panic!("{path:?} = {shape:?}: {e}\n{src}"),
                        }
                        let got = parse(&doc.to_source()).unwrap();
                        assert_eq!(got.value_at(idx, path).as_ref(), Some(shape), "{path:?}");
                        if !src.contains(['&', '*']) {
                            let mut want = before;
                            set_in(&mut want, path, shape);
                            assert_eq!(got.doc_value(idx), want, "{path:?} = {shape:?}");
                        }
                        checked += 1;
                    }
                    // Under every mapping, `=` creates two missing levels
                    // (jhheider/edikt#85) and they read back as assigned.
                    let base = parse(&src).unwrap();
                    if matches!(base.value_at(idx, path), Some(Value::Object(_))) {
                        let deep = [
                            path.clone(),
                            vec![Step::Field("zz_new".into()), Step::Field("deep".into())],
                        ]
                        .concat();
                        for shape in &shapes {
                            let mut doc = parse(&src).unwrap();
                            match doc.set(idx, &deep, shape, Strictness::Strict) {
                                Ok(()) => {}
                                // A mapping reached only through an alias or a merge.
                                Err(e) if e.to_string().contains("path not found") => continue,
                                // Refused by the contract, not reflowed.
                                Err(e)
                                    if ["multi-line flow collection", "single-pair mapping"]
                                        .iter()
                                        .any(|r| e.to_string().contains(r)) =>
                                {
                                    continue;
                                }
                                Err(e) => panic!("{deep:?} = {shape:?}: {e}\n{src}"),
                            }
                            let got = parse(&doc.to_source()).unwrap();
                            assert_eq!(got.value_at(idx, &deep).as_ref(), Some(shape), "{deep:?}");
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 100, "only {checked} edits checked");
    }

    /// Deleting any one element, or appending to any sequence, of every
    /// fixture changes exactly that element: the document reads back as the
    /// evaluator's `del(path)` / `path += [x]` (#111). The refusals allowed
    /// are the documented ones; everything in `flow.yaml` must go through.
    #[test]
    fn every_fixture_element_deletes_and_appends_alone() {
        use edikt_core::{Expr, eval};
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/yaml");
        let refusals = [
            "multi-line flow collection",
            "compact `- ` item",
            "would remove anchor",
        ];
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let file = entry.unwrap().path();
            let src = std::fs::read_to_string(&file).unwrap();
            let flow_fixture = file.file_name().is_some_and(|n| n == "flow.yaml");
            let docs = parse(&src).unwrap();
            for idx in 0..docs.docs.len() {
                let before = docs.doc_value(idx);
                let mut all = Vec::new();
                paths(&before, &mut Vec::new(), &mut all);
                for path in all.iter().filter(|p| !p.is_empty()) {
                    let del = Expr::Call("del".into(), vec![Expr::Path(path.clone())]);
                    let mut edits = vec![(del, true)];
                    if matches!(docs.value_at(idx, path), Some(Value::Array(_))) {
                        let add = Expr::AddAssign(
                            Box::new(Expr::Path(path.clone())),
                            Box::new(Expr::Literal(json!([9]))),
                        );
                        edits.push((add, false));
                    }
                    for (expr, is_del) in edits {
                        let mut doc = parse(&src).unwrap();
                        let done = if is_del {
                            doc.delete(idx, path)
                        } else {
                            doc.append(idx, path, &[json!(9)], Strictness::Strict)
                        };
                        match done {
                            Ok(()) => {}
                            // Growing a multi-line flow collection is refused
                            // by the contract; the rest only elsewhere.
                            Err(e)
                                if refusals[..if flow_fixture { 1 } else { refusals.len() }]
                                    .iter()
                                    .any(|r| e.to_string().contains(r)) =>
                            {
                                continue;
                            }
                            Err(e) => panic!("{file:?} {path:?}: {e}"),
                        }
                        let out = doc.to_source();
                        let got = parse(&out).unwrap();
                        if !src.contains('*') {
                            let want = eval(&expr, &before).unwrap().remove(0);
                            assert_eq!(got.doc_value(idx), want, "{file:?} {path:?}\n{out}");
                        }
                        for other in (0..docs.docs.len()).filter(|&o| o != idx) {
                            assert_eq!(got.doc_value(other), docs.doc_value(other));
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 100, "only {checked} edits checked");
    }
}
