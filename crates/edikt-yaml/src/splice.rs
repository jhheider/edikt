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

use crate::compose::{Node, NodeKind};
use crate::edit::{block_end, ends_with_newline, newline, trim_newline};
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
    Ok(after_block(source, block_end(source, &last.value), &lines))
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
    Ok(after_block(source, block_end(source, node), &lines))
}

/// Insert `lines` as whole lines at `at` (the start of the line after a
/// block), keeping a file without a trailing newline without one.
fn after_block(source: &str, at: usize, lines: &[String]) -> Splice {
    let nl = newline(source);
    let body = lines.join(nl);
    if at == source.len() && !ends_with_newline(source) {
        (at..at, format!("{nl}{body}"))
    } else {
        (at..at, format!("{body}{nl}"))
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
mod tests {
    use crate::{Document, parse, parse_expr};

    /// Apply `expr` to `src` and return the new source.
    fn edit(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        doc.apply(&parse_expr(expr).unwrap()).unwrap();
        doc.to_source()
    }

    fn edit_err(src: &str, expr: &str) -> String {
        let mut doc = parse(src).unwrap();
        doc.apply(&parse_expr(expr).unwrap())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn new_key_takes_block_layout_like_its_siblings() {
        // The case from jhheider/edikt#83, verbatim.
        let src = "places:\n  city:\n    name: Bahía Carmesí\n    aliases:\n      - the bay\n";
        assert_eq!(
            edit(
                src,
                r#".places.city.deprecated_aliases = ["Puerto Jubilar", "Crimson Bay"]"#
            ),
            format!("{src}    deprecated_aliases:\n      - Puerto Jubilar\n      - Crimson Bay\n")
        );
        // A mapping, nested collections, and empties (only `[]`/`{}` spell those).
        assert_eq!(
            edit("a: 1\n", r#".b = {c: [1, {d: "x"}], e: [], f: {}}"#),
            "a: 1\nb:\n  c:\n    - 1\n    - d: x\n  e: []\n  f: {}\n"
        );
        // No trailing newline stays that way.
        assert_eq!(edit("a: 1", ".b = [2]"), "a: 1\nb:\n  - 2");
    }

    #[test]
    fn scalar_becomes_a_block() {
        // The comment beside the old scalar stays on the key line.
        assert_eq!(
            edit("a: old  # note\nb: 2\n", ".a = [1, 2]"),
            "a:  # note\n  - 1\n  - 2\nb: 2\n"
        );
        assert_eq!(edit("a: old\n", ".a = {k: \"v\"}"), "a:\n  k: v\n");
        // An empty value (`a:`) fills in, keeping its comment.
        assert_eq!(
            edit("a:  # todo\nb: 1\n", ".a = {x: 1}"),
            "a:  # todo\n  x: 1\nb: 1\n"
        );
        // A value on its own line under the key is replaced line for line.
        assert_eq!(edit("a:\n  old\nb: 1\n", ".a = [1]"), "a:\n  - 1\nb: 1\n");
        // Deeper keys indent from their own key.
        assert_eq!(edit("m:\n  a: 1\n", ".m.a = [1]"), "m:\n  a:\n    - 1\n");
    }

    #[test]
    fn sequence_item_becomes_a_compact_block() {
        // The first line takes the item's place after its dash, and the
        // item's comment stays on that line.
        assert_eq!(
            edit("xs:\n  - one  # c\n  - two\n", ".xs[0] = {k: 1, j: [2]}"),
            "xs:\n  - k: 1  # c\n    j:\n      - 2\n  - two\n"
        );
        assert_eq!(
            edit("xs:\n  - one\n", ".xs[0] = [7, 8]"),
            "xs:\n  - - 7\n    - 8\n"
        );
        // An empty item gets its space after the dash.
        assert_eq!(
            edit("xs:\n  -\n  - 2\n", ".xs[0] = {x: 1}"),
            "xs:\n  - x: 1\n  - 2\n"
        );
        // An anchored item keeps its anchor on the dash line, the block below.
        assert_eq!(
            edit("xs:\n  - &a one\n", ".xs[0] = {k: 1}"),
            "xs:\n  - &a\n    k: 1\n"
        );
    }

    #[test]
    fn block_becomes_a_scalar() {
        // The key's comment stays, the items and their comments go with the
        // block, and what follows is untouched.
        assert_eq!(
            edit("a:  # note\n  - 1  # one\n  - 2\nb: 2\n", r#".a = "s""#),
            "a: s  # note\nb: 2\n"
        );
        assert_eq!(edit("a:\n  x: 1\nb: 2\n", ".a = []"), "a: []\nb: 2\n");
        // An anchor stays; a core tag that no longer fits goes.
        assert_eq!(edit("a: &x\n  - 1\n", ".a = 5"), "a: &x 5\n");
        assert_eq!(edit("a: !!seq  # c\n  - 1\n", ".a = 5"), "a: 5  # c\n");
        // A compact item collapses onto its dash line.
        assert_eq!(
            edit("xs:\n  - k: 1\n    j: 2\n  - two\n", ".xs[0] = 5"),
            "xs:\n  - 5\n  - two\n"
        );
    }

    #[test]
    fn block_replaced_by_another_shape() {
        // A different kind indents per the file's style from the key.
        assert_eq!(
            edit("a:\n  - 1\nb: 2\n", ".a = {x: 1}"),
            "a:\n  x: 1\nb: 2\n"
        );
        assert_eq!(edit("a:\n  x: 1\nb: 2\n", ".a = [1]"), "a:\n  - 1\nb: 2\n");
        // The same kind, reshaped, keeps the old block's own column, and a
        // comment above its first item stays.
        assert_eq!(
            edit("a:\n  # head\n     - 1\n     - 2\nb: 1\n", r#".a = ["x"]"#),
            "a:\n  # head\n     - x\nb: 1\n"
        );
        // A compact item swaps kind in place.
        assert_eq!(edit("- k: 1\n  j: 2\n", ".[0] = [1, 2]"), "- - 1\n  - 2\n");
    }

    #[test]
    fn nested_blocks_edit_only_what_changed() {
        // Same shape, one deep change: only that value's bytes move, and the
        // comment beside its sibling survives.
        assert_eq!(
            edit(
                "a:\n  b:\n    c: 1  # deep\n    d: 2\n",
                ".a = {b: {c: 1, d: [9]}}"
            ),
            "a:\n  b:\n    c: 1  # deep\n    d:\n      - 9\n"
        );
        // `|=` growing a list appends; item comments stay.
        assert_eq!(
            edit(
                "tags:  # t\n  - a  # first\n  - b\n",
                r#".tags |= . + ["c"]"#
            ),
            "tags:  # t\n  - a  # first\n  - b\n  - c\n"
        );
        // New keys go after the existing ones.
        assert_eq!(
            edit("m:\n  a: 1  # keep\n", ".m = {a: 1, b: {c: 2}}"),
            "m:\n  a: 1  # keep\n  b:\n    c: 2\n"
        );
        // "Unchanged" is exact, not jq's `1 == 1.0`: the new spelling lands.
        assert_eq!(edit("m:\n  a: 1\n", ".m = {a: 1.0}"), "m:\n  a: 1.0\n");
        // Assigning a collection its own value touches nothing.
        let src = "a:\n  - 1   # odd spacing\n  - {x: 1}\n";
        assert_eq!(edit(src, ".a = .a"), src);
        // A removed or reordered key means a wholesale rewrite.
        assert_eq!(
            edit("m:\n  a: 1  # gone\n  b: 2\n", ".m = {b: 2, a: 1}"),
            "m:\n  b: 2\n  a: 1\n"
        );
        // A merged-in key that stays unchanged isn't copied in as explicit.
        assert_eq!(
            edit(
                "base: &b\n  t: 30\nprod:\n  <<: *b\n  r: 5\n",
                ".prod = {r: 5, t: 30, x: 1}"
            ),
            "base: &b\n  t: 30\nprod:\n  <<: *b\n  r: 5\n  x: 1\n"
        );
    }

    #[test]
    fn flow_context_stays_flow() {
        // Inside a flow collection, a new value is spelled flow.
        assert_eq!(
            edit("f: {a: 1}\n", r#".f.a = {z: "a,b"}"#),
            "f: {a: {z: \"a,b\"}}\n"
        );
        // A flow collection replaced keeps flow style and its comment.
        assert_eq!(
            edit("f: [1, 2]  # c\n", ".f = {x: [1]}"),
            "f: {x: [1]}  # c\n"
        );
        assert_eq!(edit("f: [1,\n  2]\n", ".f = [3]"), "f: [3]\n");
        // A flow value's elements change in place.
        assert_eq!(edit("f: [1,2]\n", ".f = [1, 3]"), "f: [1,3]\n");
        // New keys and items join a flow collection in flow style.
        assert_eq!(
            edit("f: {a: 1}\n", ".f.b = [1, 2]"),
            "f: {a: 1, b: [1, 2]}\n"
        );
        assert_eq!(edit("f: {}\n", ".f.b = 1"), "f: {b: 1}\n");
        assert_eq!(edit("f: [1, 2]\n", ".f += [{a: 1}]"), "f: [1, 2, {a: 1}]\n");
        assert_eq!(edit("f: [1,]\n", ".f += [2]"), "f: [1, 2,]\n");
        assert_eq!(edit("f: []\n", ".f += [3]"), "f: [3]\n");
        assert_eq!(edit("f: [1, 2]\n", ".f |= . + [3]"), "f: [1, 2, 3]\n");
        // Growing a multi-line flow collection would reflow it: refused.
        assert!(edit_err("f: [1,\n  2]\n", ".f += [3]").contains("multi-line flow"));
        assert!(edit_err("f: {a: 1,\n  b: 2}\n", ".f.c = 3").contains("multi-line flow"));
    }

    #[test]
    fn follows_the_files_indent_width() {
        // Four-space file: four-space levels.
        assert_eq!(
            edit("a:\n    x: 1\nb: 3\n", ".b = {k: [1, 2]}"),
            "a:\n    x: 1\nb:\n    k:\n        - 1\n        - 2\n"
        );
        // Indentless sequences stay indentless.
        assert_eq!(
            edit("a:\n    x:\n    - 1\nb: 3\n", ".b = {k: [1, 2]}"),
            "a:\n    x:\n    - 1\nb:\n    k:\n    - 1\n    - 2\n"
        );
        assert_eq!(
            edit("xs:\n- 1\n", ".xs += [{a: 1, b: [2]}]"),
            "xs:\n- 1\n- a: 1\n  b:\n  - 2\n"
        );
        // Nothing nested to learn from: two spaces.
        assert_eq!(edit("a: 1\n", ".a = {b: 1}"), "a:\n  b: 1\n");
    }

    #[test]
    fn append_collections_to_a_block_sequence() {
        assert_eq!(
            edit("xs:\n  - 1\n", ".xs += [{a: 1, b: 2}, [3]]"),
            "xs:\n  - 1\n  - a: 1\n    b: 2\n  - - 3\n"
        );
        // An anchored sequence: the dash is past the anchor's line.
        assert_eq!(
            edit("xs: &s\n  - 1\n", ".xs += [2]"),
            "xs: &s\n  - 1\n  - 2\n"
        );
        assert_eq!(
            edit("xs:\n  - 1", ".xs += [{a: 1}]"),
            "xs:\n  - 1\n  - a: 1"
        );
    }

    #[test]
    fn assignment_creates_missing_parents() {
        // jhheider/edikt#85: `=` creates every missing level, laid out like
        // any new collection. Under a block mapping, at the file's width:
        assert_eq!(
            edit("a:\n    x: 1\n", ".a.b.c = [1]"),
            "a:\n    x: 1\n    b:\n        c:\n            - 1\n"
        );
        // At the root, and after a file without a trailing newline.
        assert_eq!(edit("a: 1\n", ".b.c = 1"), "a: 1\nb:\n  c: 1\n");
        assert_eq!(edit("a: 1", ".b.c = 1"), "a: 1\nb:\n  c: 1");
        // Under a compact list item, at that mapping's column.
        assert_eq!(
            edit("xs:\n  - a: 1\n", r#".xs[0].b.c = "v""#),
            "xs:\n  - a: 1\n    b:\n      c: v\n"
        );
        // Indentless sequences stay indentless in the created levels.
        assert_eq!(
            edit("m:\n    x:\n    - 1\n", ".m.k.l = [1, 2]"),
            "m:\n    x:\n    - 1\n    k:\n        l:\n        - 1\n        - 2\n"
        );
        // In an empty or flow parent, flow.
        assert_eq!(edit("f: {}\n", ".f.a.b = 1"), "f: {a: {b: 1}}\n");
        assert_eq!(
            edit("f: {a: 1}\n", ".f.b.c = [1]"),
            "f: {a: 1, b: {c: [1]}}\n"
        );
        // CRLF files get CRLF lines.
        assert_eq!(edit("a: 1\r\n", ".b.c = 1"), "a: 1\r\nb:\r\n  c: 1\r\n");
        // Every document of a stream gets the missing levels.
        assert_eq!(
            edit("---\nk: A\n---\nk: B\nm:\n  x: 1\n", ".m.z = 1"),
            "---\nk: A\nm:\n  z: 1\n---\nk: B\nm:\n  x: 1\n  z: 1\n"
        );
        // No array elements out of thin air, and no key inside a scalar.
        assert!(edit_err("a: 1\n", ".b.c[0] = 1").contains("cannot create array elements"));
        assert!(edit_err("a: 1\n", ".a.b = 1").contains("path not found"));
        // `|=` and `+=` still need the path to exist.
        assert!(edit_err("a: 1\n", ".b.c |= 1").contains("path not found"));
    }

    #[test]
    fn roots_aliases_streams_and_crlf() {
        assert_eq!(edit("- 1\n- 2\n", ". = {a: [1]}"), "a:\n  - 1\n");
        assert_eq!(edit("a: 1\n", ". = [1]"), "- 1\n");
        assert_eq!(edit("hello  # c\n", ". = {a: 1}"), "a: 1  # c\n");
        // An alias replaced by a block writes the block; the anchor's target
        // replaced keeps its anchor for the aliases.
        assert_eq!(
            edit("base: &b\n  - 1\nother: *b\n", ".other = [1, 2]"),
            "base: &b\n  - 1\nother:\n  - 1\n  - 2\n"
        );
        assert_eq!(
            edit("base: &b\n  - 1\nother: *b\n", ".base = {x: 1}"),
            "base: &b\n  x: 1\nother: *b\n"
        );
        // Every document of a stream gets the block.
        assert_eq!(
            edit("---\na: 1\n---\na: 2\n", ".a = [3]"),
            "---\na:\n  - 3\n---\na:\n  - 3\n"
        );
        // CRLF files get CRLF lines.
        assert_eq!(
            edit("a: 1\r\nb: 2\r\n", ".a = [1, 2]"),
            "a:\r\n  - 1\r\n  - 2\r\nb: 2\r\n"
        );
        assert_eq!(
            edit("a:\r\n  - 1\r\nb: 2\r\n", ".a = 5"),
            "a: 5\r\nb: 2\r\n"
        );
    }
}

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
                            // merge has no bytes of its own; a block scalar
                            // is refused. Anything else is a failure.
                            Err(e) if e.to_string().contains("path not found") => continue,
                            Err(e) if e.to_string().contains("multi-line") => continue,
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
}
