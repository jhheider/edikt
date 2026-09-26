//! Format-preserving edits and conversion emit over `kdl-rs`.
//!
//! Edits are surgical: set an argument or property in place (only its value
//! text changes), create a leaf node, delete a node/prop/argument, extend a
//! run of repeated nodes. Replacing a whole node body wholesale is refused
//! rather than reflowed; same policy as YAML.

use crate::Kdl;
use crate::project::{self, ARGS_KEY};
use crate::spell::spell_like;
use edikt_core::{BinOp, Document, EditError, Expr, Step, Value, eval};
use kdl::{
    FormatConfig, KdlDocument, KdlDocumentFormat, KdlEntry, KdlEntryFormat, KdlNode, KdlValue,
};

pub fn apply(doc: &mut Kdl, expr: &Expr) -> Result<(), EditError> {
    // A path-expression target (`(.xs[] | select(...) | .n) = v`, #88)
    // resolves to concrete paths first; each is then an ordinary edit.
    if let Some(each) = edikt_core::lower_mutation(expr, || doc.to_value())
        .map_err(|e| EditError::new(e.to_string()))?
    {
        for e in &each {
            apply(doc, e)?;
        }
        return Ok(());
    }
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
            let sum = eval_one(
                &Expr::Binary(
                    BinOp::Add,
                    Box::new(Expr::Path(Vec::new())),
                    Box::new(Expr::Literal(addend)),
                ),
                &current,
            )?;
            doc.set(steps, &sum)
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

// --- set ------------------------------------------------------------------

pub(crate) fn set_in_doc(
    doc: &mut KdlDocument,
    path: &[Step],
    value: &Value,
    depth: usize,
    unit: &str,
) -> Result<(), EditError> {
    let Some(Step::Field(name)) = path.first() else {
        return Err(EditError::new(
            "KDL assignment paths start with a node name",
        ));
    };
    let occurrences: Vec<usize> = (0..doc.nodes().len())
        .filter(|&i| doc.nodes()[i].name().value() == name)
        .collect();

    match occurrences.len() {
        0 => {
            if path.len() > 1 {
                return Err(EditError::new(format!("no node `{name}`")));
            }
            let at = doc.nodes().len();
            insert_nodes(doc, value_to_nodes(name, value)?, at, depth, unit);
            Ok(())
        }
        1 => {
            let node = &mut doc.nodes_mut()[occurrences[0]];
            if path.len() == 1 {
                set_node(node, value)
            } else {
                set_in_node(node, &path[1..], value, depth + 1, unit)
            }
        }
        n => match path.get(1) {
            Some(Step::Index(i)) => {
                let idx = resolve_index(*i, n)
                    .ok_or_else(|| EditError::new(format!("`{name}` index out of range")))?;
                let node = &mut doc.nodes_mut()[occurrences[idx]];
                if path.len() == 2 {
                    set_node(node, value)
                } else {
                    set_in_node(node, &path[2..], value, depth + 1, unit)
                }
            }
            None => extend_repeated(doc, name, &occurrences, value, depth, unit),
            _ => Err(EditError::new(format!(
                "`{name}` is repeated: address one occurrence (.{name}[i])"
            ))),
        },
    }
}

fn set_in_node(
    node: &mut KdlNode,
    path: &[Step],
    value: &Value,
    depth: usize,
    unit: &str,
) -> Result<(), EditError> {
    match &path[0] {
        Step::Field(k) if k == ARGS_KEY => match path.get(1) {
            None => replace_args(node, value),
            Some(Step::Index(i)) if path.len() == 2 => set_arg(node, *i, value),
            _ => Err(EditError::new("arguments are scalars; cannot go deeper")),
        },
        Step::Field(k) => {
            if node.entry(k.as_str()).is_some() {
                if path.len() > 1 {
                    return Err(EditError::new(format!("property `{k}` is a scalar")));
                }
                let kv = scalar_only(value)?;
                let entry = node.entry_mut(k.as_str()).unwrap();
                set_entry_value(entry, kv);
                return Ok(());
            }
            if node.children().is_some() {
                set_in_doc(
                    node.children_mut().as_mut().unwrap(),
                    path,
                    value,
                    depth,
                    unit,
                )
            } else if node.entries().is_empty() {
                // A bare node grows a children block, set off by a space
                // (`name {`) as a parsed block is.
                if let Some(f) = node.format_mut()
                    && f.before_children.is_empty()
                {
                    f.before_children = " ".into();
                }
                let children = node.ensure_children();
                // The closing `}` sits at the node's own level (`depth` is
                // the children's).
                children.set_format(block_format(unit, depth.saturating_sub(1)));
                set_in_doc(children, path, value, depth, unit)
            } else {
                Err(EditError::new(format!(
                    "`{}` holds arguments, not children; cannot create `{k}` inside it",
                    node.name().value()
                )))
            }
        }
        Step::Index(i) => set_arg(node, *i, value),
        Step::Iterate => Err(EditError::new(
            "`[]` in assignment paths is not supported for KDL",
        )),
        Step::Comment(_) => Err(EditError::new(
            "editing comments (`#`): the comment step must end the path and be \
             edited on its own, e.g. `.foo.# = \"text\"`",
        )),
    }
}

/// Set a whole node from a value. Scalar/argument-row nodes update in place;
/// replacing a container body wholesale is refused rather than reflowed.
fn set_node(node: &mut KdlNode, value: &Value) -> Result<(), EditError> {
    let is_leaf = node.children().is_none() && node.entries().iter().all(|e| e.name().is_none());
    if !is_leaf {
        return Err(EditError::new(format!(
            "replacing the whole body of `{}` is not supported; set its keys instead",
            node.name().value()
        )));
    }
    replace_args(node, value)
}

/// Replace a node's positional arguments with a scalar or array of scalars.
/// If the new array starts with the current arguments, the extras append and
/// the existing entries keep their bytes (`+=` reduces to this).
fn replace_args(node: &mut KdlNode, value: &Value) -> Result<(), EditError> {
    let new: Vec<KdlValue> = match value {
        Value::Array(items) => items
            .iter()
            .map(scalar_only)
            .collect::<Result<Vec<_>, _>>()?,
        scalar => vec![scalar_only(scalar)?],
    };
    let current: Vec<usize> = (0..node.entries().len())
        .filter(|&i| node.entries()[i].name().is_none())
        .collect();
    let is_prefix = current.len() <= new.len()
        && current
            .iter()
            .zip(&new)
            .all(|(&i, nv)| node.entries()[i].value() == nv);
    if is_prefix {
        for nv in new.into_iter().skip(current.len()) {
            node.entries_mut().push(arg_entry(nv));
        }
        return Ok(());
    }
    if current.len() == new.len() {
        // Same shape: update each argument's value in place.
        for (&i, nv) in current.iter().zip(new) {
            set_entry_value(&mut node.entries_mut()[i], nv);
        }
        return Ok(());
    }
    // Different shape: rebuild the argument row (properties stay untouched).
    node.entries_mut().retain(|e| e.name().is_some());
    for nv in new {
        node.entries_mut().push(arg_entry(nv));
    }
    Ok(())
}

fn set_arg(node: &mut KdlNode, i: i64, value: &Value) -> Result<(), EditError> {
    let args: Vec<usize> = (0..node.entries().len())
        .filter(|&j| node.entries()[j].name().is_none())
        .collect();
    let idx = resolve_index(i, args.len())
        .ok_or_else(|| EditError::new("argument index out of range"))?;
    let kv = scalar_only(value)?;
    set_entry_value(&mut node.entries_mut()[args[idx]], kv);
    Ok(())
}

/// `.name = [bigger array]` on repeated nodes: if the existing occurrences
/// match the array's prefix, the extras append as new nodes (this is how `+=`
/// lands); anything else would rewrite untargeted nodes, so it is refused.
fn extend_repeated(
    doc: &mut KdlDocument,
    name: &str,
    occurrences: &[usize],
    value: &Value,
    depth: usize,
    unit: &str,
) -> Result<(), EditError> {
    let Value::Array(items) = value else {
        return Err(EditError::new(format!(
            "`{name}` is repeated: address one occurrence (.{name}[i])"
        )));
    };
    let matches_prefix = occurrences.len() <= items.len()
        && occurrences
            .iter()
            .zip(items)
            .all(|(&i, v)| project::node_to_value(&doc.nodes()[i]) == *v);
    if !matches_prefix {
        return Err(EditError::new(format!(
            "replacing the repeated `{name}` nodes wholesale is not supported; \
             append with `+=` or address one occurrence (.{name}[i])"
        )));
    }
    let mut new_nodes = Vec::new();
    for v in &items[occurrences.len()..] {
        new_nodes.extend(value_to_nodes(name, v)?);
    }
    let at = occurrences.last().unwrap() + 1;
    insert_nodes(doc, new_nodes, at, depth, unit);
    Ok(())
}

// --- delete ----------------------------------------------------------------

/// `in_children` marks a node's children block, whose first child's leading
/// decor also holds the rest of the `{` line (see [`remove_nodes`]).
pub(crate) fn delete_in_doc(
    doc: &mut KdlDocument,
    path: &[Step],
    in_children: bool,
) -> Result<(), EditError> {
    // Fan-out delete: `.foo[]` iterates the projection - repeated occurrences
    // when `/foo` is an array of objects, the node's contents otherwise -
    // expanded to concrete paths and deleted back-to-front.
    if path.contains(&Step::Iterate) {
        let whole = crate::project::doc_to_value(doc);
        // `del(.name[])` on an array **of objects** is a run of repeated nodes:
        // one whole-document retain (which also sidesteps the single-occurrence
        // `.name[i]`-means-argument quirk the per-path deletes would hit on the
        // last remaining node). A single node's value is an object (its args/
        // props/children) or an args array of scalars - jq's `{a:1,b:2} |
        // del(.[]) -> {}` / `[1,2] | del(.[]) -> []` - and falls through to the
        // per-path deletes below.
        if path.len() == 2 && matches!(path[0], Step::Field(_)) && path[1] == Step::Iterate {
            // `del(.name[])` on a **repeated** name removes every occurrence:
            // one retain (which also sidesteps the single-occurrence
            // `.name[i]`-means-argument quirk the per-path deletes would hit on
            // the last remaining node). A repeated single-arg node projects
            // identically to a single multi-arg node (`["a","b"]` either way),
            // so the discriminating truth is the actual node count in the DOM,
            // not the value model. A single node falls through to per-path
            // deletes over its contents: `{a:1,b:2} | del(.[]) -> {}`,
            // `[1,2] | del(.[]) -> []`.
            let Step::Field(name) = &path[0] else {
                unreachable!("guarded by the matches! above");
            };
            let occurrences = doc
                .nodes()
                .iter()
                .filter(|n| n.name().value() == name)
                .count();
            if occurrences > 1 {
                return delete_in_doc(doc, &path[..1], in_children);
            }
        }
        let paths = edikt_core::expand_delete_paths(path, &whole)
            .map_err(|e| EditError::new(e.to_string()))?;
        for p in &paths {
            delete_in_doc(doc, p, in_children)?;
        }
        return Ok(());
    }
    let Some(Step::Field(name)) = path.first() else {
        return Err(EditError::new("KDL paths start with a node name"));
    };
    let occurrences: Vec<usize> = (0..doc.nodes().len())
        .filter(|&i| doc.nodes()[i].name().value() == name)
        .collect();
    if occurrences.is_empty() {
        return Ok(()); // jq semantics: deleting a miss is a no-op
    }
    if path.len() == 1 {
        remove_nodes(doc, &occurrences, in_children);
        return Ok(());
    }
    if let Step::Index(i) = path[1] {
        // Multiple occurrences: the index picks the i-th occurrence. A *single*
        // occurrence's index addresses that node's **arguments** (the
        // projection: `.node` is the args array), so it must not be gated on
        // the occurrence count - resolving `.node[1]` against 1 occurrence
        // silently no-ops. `remove_arg` resolves against the real arg count.
        if occurrences.len() > 1 {
            let Some(idx) = resolve_index(i, occurrences.len()) else {
                return Ok(());
            };
            if path.len() == 2 {
                remove_nodes(doc, &occurrences[idx..=idx], in_children);
                return Ok(());
            }
            return delete_in_node(&mut doc.nodes_mut()[occurrences[idx]], &path[2..]);
        }
        // A single occurrence: `.node[i]` addresses the node's arguments (the
        // projection makes `.node` the args array), so the index goes to
        // `remove_arg` - but only as a terminal step; a deeper path is a
        // node-level delete (`.node[0].x`), resolved from the index onward.
        let tail = if path.len() == 2 {
            &path[1..]
        } else {
            &path[2..]
        };
        return delete_in_node(&mut doc.nodes_mut()[occurrences[0]], tail);
    } else if occurrences.len() > 1 {
        return Err(EditError::new(format!(
            "`{name}` is repeated: address one occurrence (.{name}[i])"
        )));
    }
    delete_in_node(&mut doc.nodes_mut()[occurrences[0]], &path[1..])
}

fn delete_in_node(node: &mut KdlNode, path: &[Step]) -> Result<(), EditError> {
    match &path[0] {
        Step::Field(k) if k == ARGS_KEY => match path.get(1) {
            None => {
                node.entries_mut().retain(|e| e.name().is_some());
                Ok(())
            }
            Some(Step::Index(i)) if path.len() == 2 => {
                remove_arg(node, *i);
                Ok(())
            }
            _ => Err(EditError::new("arguments are scalars; cannot go deeper")),
        },
        Step::Field(k) => {
            if node.entry(k.as_str()).is_some() {
                if path.len() > 1 {
                    return Err(EditError::new(format!("property `{k}` is a scalar")));
                }
                node.entries_mut()
                    .retain(|e| e.name().map(|n| n.value()) != Some(k.as_str()));
                return Ok(());
            }
            match node.children_mut() {
                Some(children) => delete_in_doc(children, path, true),
                None => Ok(()), // nothing there: no-op
            }
        }
        Step::Index(i) => {
            remove_arg(node, *i);
            Ok(())
        }
        Step::Iterate => Err(EditError::new("del(.[]) is not supported for KDL")),
        Step::Comment(_) => Err(EditError::new(
            "deleting comments (`#`): the comment step must end the path and be \
             deleted on its own, e.g. `del(.foo.#)`",
        )),
    }
}

/// Remove the nodes at `indices` (ascending). In a children block, kdl-rs
/// keeps the rest of the `{` line (its line break, and any comment trailing
/// the brace) in the first child's leading decor; when that child goes, the
/// first survivor inherits that prefix so the block keeps its line break.
/// The rest of the removed decor (the node's own head comment and indent)
/// goes with it.
fn remove_nodes(doc: &mut KdlDocument, indices: &[usize], in_children: bool) {
    let brace_line = (in_children && indices.first() == Some(&0))
        .then(|| {
            let leading = doc.nodes()[0].format().map_or("", |f| f.leading.as_str());
            leading.find('\n').map(|nl| leading[..=nl].to_owned())
        })
        .flatten();
    for &i in indices.iter().rev() {
        doc.nodes_mut().remove(i);
    }
    if let Some(prefix) = brace_line
        && let Some(f) = doc.nodes_mut().first_mut().and_then(|n| n.format_mut())
    {
        f.leading.insert_str(0, &prefix);
    }
}

fn remove_arg(node: &mut KdlNode, i: i64) {
    let args: Vec<usize> = (0..node.entries().len())
        .filter(|&j| node.entries()[j].name().is_none())
        .collect();
    if let Some(idx) = resolve_index(i, args.len()) {
        node.entries_mut().remove(args[idx]);
    }
}

// --- building --------------------------------------------------------------

/// A value as fresh node(s) named `name`, inverting the projection:
///
/// - an array whose elements are **all scalars** -> one node carrying them as
///   positional arguments (`["a","b"]` -> `name "a" "b"`);
/// - an array with any **object or array** element -> one node per element (a
///   run of repeated nodes), each object->node, each inner array->that node's
///   arguments, each scalar->a one-argument node;
/// - an object -> one node (args under `"-"`, then props, then children);
/// - a scalar -> a one-argument node; `null` -> a bare node.
///
/// A triply-nested array (an array element that is itself an array of arrays)
/// has no KDL spelling and errors cleanly.
pub(crate) fn value_to_nodes(name: &str, value: &Value) -> Result<Vec<KdlNode>, EditError> {
    if let Value::Array(items) = value
        && items
            .iter()
            .any(|v| matches!(v, Value::Object(_) | Value::Array(_)))
    {
        let mut out = Vec::new();
        for item in items {
            out.push(one_node(name, item)?);
        }
        return Ok(out);
    }
    Ok(vec![one_node(name, value)?])
}

/// Build exactly one node named `name` from `value`.
fn one_node(name: &str, value: &Value) -> Result<KdlNode, EditError> {
    let mut node = KdlNode::new(name);
    match value {
        Value::Object(entries) => {
            for (k, v) in entries {
                if k == ARGS_KEY {
                    match v {
                        Value::Array(args) => {
                            for a in args {
                                node.entries_mut().push(arg_entry(scalar_only(a)?));
                            }
                        }
                        scalar => node.entries_mut().push(arg_entry(scalar_only(scalar)?)),
                    }
                } else {
                    for child in value_to_nodes(k, v)? {
                        node.ensure_children().nodes_mut().push(child);
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                node.entries_mut().push(arg_entry(scalar_only(item)?));
            }
        }
        Value::Null => {} // a bare node
        scalar => node.entries_mut().push(arg_entry(scalar_only(scalar)?)),
    }
    Ok(node)
}

/// Insert freshly built nodes at position `at`, formatted to sit at `depth`.
///
/// The new node copies the layout of the sibling it follows: its indentation
/// on a line of its own, or a `;`-separated slot on a single-line block
/// (`{ a 1; b 2 }`). Internals autoformat below it, indented by the file's
/// own `unit`. With no sibling to copy, the level is `unit` x `depth`.
fn insert_nodes(doc: &mut KdlDocument, nodes: Vec<KdlNode>, at: usize, depth: usize, unit: &str) {
    let config = FormatConfig::builder()
        .indent_level(depth)
        .indent(unit)
        .build();
    let level_indent = own_line_indents(doc).next().map(str::to_owned);
    for (i, mut node) in nodes.into_iter().enumerate() {
        node.autoformat_config(&config);
        close_blocks(&mut node, unit, depth);
        let pos = at + i;
        let prev_fmt = pos
            .checked_sub(1)
            .and_then(|p| doc.nodes().get(p))
            .and_then(|p| p.format().cloned());
        if let (Some(prev), Some(f)) = (prev_fmt, node.format_mut()) {
            if prev.terminator.contains('\n') || prev.trailing.contains('\n') {
                // Line layout: the previous line break is already there.
                if let Some(indent) = &level_indent {
                    f.leading = indent.clone();
                }
            } else {
                // The previous node ends its line without a break: a `;` or
                // the block's closing `}`. Join the same line, and make sure
                // a terminator separates the two nodes (without one, the new
                // node would read as more arguments of the previous one).
                f.leading = if prev.leading.contains('\n') {
                    format!("\n{}", level_indent.as_deref().unwrap_or(""))
                } else {
                    " ".into()
                };
                f.before_terminator = prev.before_terminator.clone();
                f.terminator = prev.terminator.clone();
                if prev.terminator.is_empty()
                    && let Some(pf) = doc.nodes_mut()[pos - 1].format_mut()
                {
                    pf.before_terminator.clear();
                    pf.terminator = ";".into();
                }
            }
        }
        doc.nodes_mut().insert(pos, node);
    }
}

/// Decor for a fresh children block of a node at `level`: the `{` ends its
/// line and the closing `}` is indented to the node's own level.
fn block_format(unit: &str, level: usize) -> KdlDocumentFormat {
    KdlDocumentFormat {
        leading: "\n".into(),
        trailing: unit.repeat(level),
    }
}

/// kdl-rs's autoformat leaves a built node's children blocks without decor,
/// and then indents their closing `}` by four spaces a level whatever the
/// configured indent; give each block the file's own indentation instead.
fn close_blocks(node: &mut KdlNode, unit: &str, level: usize) {
    if let Some(children) = node.children_mut() {
        if children.format().is_none() {
            children.set_format(block_format(unit, level));
        }
        for child in children.nodes_mut() {
            close_blocks(child, unit, level + 1);
        }
    }
}

fn is_indent(s: &str) -> bool {
    s.chars().all(|c| c == ' ' || c == '\t')
}

/// The indentation of each node in `doc` that starts a line of its own: the
/// whitespace after the line break, which sits either at the end of the
/// node's own `leading` decor (a block's first child, or after a comment) or
/// in the previous node's terminator.
fn own_line_indents(doc: &KdlDocument) -> impl Iterator<Item = &str> {
    let mut prev_broke = false;
    doc.nodes().iter().filter_map(move |node| {
        let f = node.format()?;
        let indent = match f.leading.rsplit_once('\n') {
            Some((_, tail)) => Some(tail),
            None if prev_broke => Some(f.leading.as_str()),
            None => None,
        }
        .filter(|s| is_indent(s));
        prev_broke = f.terminator.contains('\n') || f.trailing.contains('\n');
        indent
    })
}

/// The file's indentation unit, learned from its first indented node on a
/// line of its own (its indent divided by its depth); kdl-rs's four spaces
/// when nothing nested sits on its own line.
pub(crate) fn indent_unit(doc: &KdlDocument) -> String {
    fn walk(doc: &KdlDocument, depth: usize) -> Option<String> {
        if depth > 0
            && let Some(indent) =
                own_line_indents(doc).find(|s| !s.is_empty() && s.len() % depth == 0)
        {
            return Some(indent[..indent.len() / depth].to_owned());
        }
        doc.nodes()
            .iter()
            .filter_map(|n| n.children())
            .find_map(|c| walk(c, depth + 1))
    }
    walk(doc, 0).unwrap_or_else(|| "    ".into())
}

// --- emit -------------------------------------------------------------------

/// Emit a value as a fresh KDL document (autoformatted; there is no layout to
/// preserve on this path). Returns text and warnings (none: KDL holds every
/// feature).
pub fn emit(value: &Value) -> Result<(String, Vec<String>), EditError> {
    Ok((build_document(value)?.to_string(), Vec::new()))
}

pub(crate) fn build_document(value: &Value) -> Result<KdlDocument, EditError> {
    let Value::Object(entries) = value else {
        return Err(EditError::new(
            "KDL output requires a top-level object (a document is a list of nodes)",
        ));
    };
    let mut doc = KdlDocument::new();
    for (k, v) in entries {
        for node in value_to_nodes(k, v)? {
            doc.nodes_mut().push(node);
        }
    }
    doc.autoformat();
    Ok(doc)
}

// --- shared helpers ----------------------------------------------------------

fn scalar_only(v: &Value) -> Result<KdlValue, EditError> {
    project::to_kdl_scalar(v).ok_or_else(|| {
        EditError::new(format!(
            "a KDL argument/property holds a scalar, not {}",
            v.type_name()
        ))
    })
}

/// Update an entry's value *and* its stored text repr (kdl-rs renders the
/// repr, so `set_value` alone would print the stale bytes).
fn set_entry_value(entry: &mut KdlEntry, value: KdlValue) {
    let old = entry.format().map_or("", |f| f.value_repr.as_str());
    let repr = spell_like(old, &value);
    entry.set_value(value);
    if let Some(f) = entry.format_mut() {
        f.value_repr = repr;
    }
}

/// A fresh positional-argument entry with its own leading space.
fn arg_entry(value: KdlValue) -> KdlEntry {
    let mut e = KdlEntry::new(value.clone());
    e.set_format(KdlEntryFormat {
        value_repr: spell_like("", &value),
        leading: " ".into(),
        ..Default::default()
    });
    e
}

fn resolve_index(i: i64, len: usize) -> Option<usize> {
    let idx = if i < 0 { len as i64 + i } else { i };
    (0..len as i64).contains(&idx).then_some(idx as usize)
}
