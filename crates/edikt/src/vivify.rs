//! What an edit will do that it should say out loud: the paths a plain `=`
//! would create (the `created` note, `--no-vivify`), and the path-expression
//! targets that matched nothing.

use edikt_core::Value;

/// Is the mutation exempt from auto-vivify inspection? A `select(` filter or a
/// `^dN` document selector scopes the edit at apply time (per document / per
/// match), so whether a path "exists" can't be decided against the whole
/// document; those keep the default behavior.
pub(crate) fn scoped_edit(expr: &edikt_core::Expr) -> bool {
    match expr {
        edikt_core::Expr::DocSelect(..) => true,
        edikt_core::Expr::Pipe(a, b) => scoped_edit(a) || scoped_edit(b),
        edikt_core::Expr::Call(name, _) => name == "select",
        _ => false,
    }
}

/// The concrete paths a plain `=` assignment would create (its LHS doesn't
/// resolve against the value model, and it's not an array-append). One entry
/// per document per created path. A path that errors mid-walk isn't ours to
/// judge - the apply will raise the real error.
pub(crate) fn would_create(expr: &edikt_core::Expr, values: &[Value]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (di, v) in values.iter().enumerate() {
        for steps in assign_paths(expr, v) {
            if !path_resolves(&steps, v) {
                out.push((di, edikt_core::render_path(&steps)));
            }
        }
    }
    out
}

/// The LHS paths of every plain `=` in the expression (not `|=`/`+=`/`del`,
/// which already handle a miss on their own), against document value `v`.
/// Iterate fan-outs (`[]`) are skipped: creating *through* `[]` errors
/// anyway. A path-expression LHS (`(.xs[] | select(...) | .new) = 1`)
/// contributes each concrete path it resolves to, so a key it creates is
/// announced like any other.
pub(crate) fn assign_paths(expr: &edikt_core::Expr, v: &Value) -> Vec<Vec<edikt_core::Step>> {
    match expr {
        edikt_core::Expr::Assign(lhs, _) => match lhs.as_path() {
            Some(p) if !p.is_empty() && !p.contains(&edikt_core::Step::Iterate) => vec![p.to_vec()],
            Some(_) => Vec::new(),
            // An error is the apply's to raise.
            None => edikt_core::eval_paths(lhs, v).unwrap_or_default(),
        },
        edikt_core::Expr::Pipe(a, b) => {
            let mut out = assign_paths(a, v);
            out.extend(assign_paths(b, v));
            out
        }
        _ => Vec::new(),
    }
}

/// The path-expression targets (of `=`, `|=`, `+=`, `del`) that resolve to
/// no path in any document, rendered for the "matched nothing" note.
pub(crate) fn unmatched_targets(expr: &edikt_core::Expr, values: &[Value]) -> Vec<String> {
    match expr {
        edikt_core::Expr::Pipe(a, b) => {
            let mut out = unmatched_targets(a, values);
            out.extend(unmatched_targets(b, values));
            out
        }
        _ => match edikt_core::path_expr_target(expr) {
            Some(lhs)
                if values.iter().all(|v| {
                    edikt_core::eval_paths(lhs, v).is_ok_and(|paths| paths.is_empty())
                }) =>
            {
                vec![render_target(lhs)]
            }
            _ => Vec::new(),
        },
    }
}

/// A path expression, rendered for a note: paths as written, `select`'s
/// predicate elided (`.items[] | select(...) | .n`).
pub(crate) fn render_target(expr: &edikt_core::Expr) -> String {
    match expr {
        edikt_core::Expr::Path(steps) => edikt_core::render_path(steps),
        edikt_core::Expr::Pipe(a, b) => format!("{} | {}", render_target(a), render_target(b)),
        edikt_core::Expr::Comma(items) => items
            .iter()
            .map(render_target)
            .collect::<Vec<_>>()
            .join(", "),
        edikt_core::Expr::Call(name, _) => format!("{name}(...)"),
        _ => "...".to_string(),
    }
}

/// Does `steps` already address a value (or a TOML-style `arr[len]` append
/// point)? A key missing entirely, or ending mid-path, means `=` would create.
pub(crate) fn path_resolves(steps: &[edikt_core::Step], v: &Value) -> bool {
    match edikt_core::eval(&edikt_core::Expr::Path(steps.to_vec()), v) {
        Ok(stream) => {
            if !stream.is_empty() {
                true
            } else {
                // `arr[i] = v` appends when i == len (TOML; JSONC refuses there).
                let Some((last, parent)) = steps.split_last() else {
                    return false;
                };
                let edikt_core::Step::Index(i) = last else {
                    return false;
                };
                let some = edikt_core::eval(&edikt_core::Expr::Path(parent.to_vec()), v)
                    .ok()
                    .and_then(|s| s.into_iter().next());
                match some {
                    Some(Value::Array(a)) => {
                        edikt_core::normalize_index(*i, a.len()) == Some(a.len())
                    }
                    _ => false,
                }
            }
        }
        // A type error mid-path is the apply's to raise, not ours.
        Err(_) => true,
    }
}

/// "created `<path>` (was missing)", with a document qualifier on streams.
pub(crate) fn create_note(doc: usize, ndocs: usize, path: &str) -> String {
    if ndocs > 1 {
        format!("created `{path}` in document {doc} (was missing)")
    } else {
        format!("created `{path}` (was missing)")
    }
}
