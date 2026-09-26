//! Assigning a collection over a collection as element edits (#117).
//!
//! `.o = {a: 1}` over `{a: 1, b: 2}` means "delete `b`", and `.k = [1, 3]`
//! over `[1, 2, 3]` means "delete `.k[1]`": written as those edits, through
//! the format's own primitives, every element the new value keeps keeps its
//! bytes, comments and layout, where one replacement would respell them all.
//! The rules are the contract's (Mutation, "Assigning a collection over a
//! collection diffs").

use super::Mutable;
use crate::ast::Step;
use crate::error::EditError;
use crate::value::Value;

/// Set `path` to `value` in `doc`. A collection written over a collection of
/// the same kind becomes element edits where the diff covers it and the
/// format doesn't decline it ([`Mutable::diffs`]); anything else is one
/// [`Mutable::set`].
pub fn assign<M: Mutable + ?Sized>(
    doc: &mut M,
    path: &[Step],
    value: &Value,
) -> Result<(), EditError> {
    // An iterate path names many values; the format answers for it in `set`.
    if matches!(value, Value::Array(_) | Value::Object(_))
        && !path.contains(&Step::Iterate)
        && let Some(current) = doc.value_at(path)
        && let Some(plan) = plan(&current, value)
    {
        if plan.is_empty() {
            return Ok(());
        }
        if doc.diffs(path, &current, value) {
            return apply(doc, path, value, plan);
        }
    }
    doc.set(path, value)
}

/// Whether [`assign`] writes `value` over `current` with an append or an
/// added key (so a format that can't grow a collection in place can decline
/// the diff, [`Mutable::diffs`]).
pub fn diff_grows(current: &Value, value: &Value) -> bool {
    match plan(current, value) {
        Some(Plan::Object { added, .. }) => !added.is_empty(),
        Some(Plan::InPlace { extras, .. } | Plan::Remove { extras, .. }) => !extras.is_empty(),
        None => false,
    }
}

/// The element edits that bring one collection to another.
#[derive(Debug, PartialEq)]
enum Plan<'v> {
    /// Keys deleted, keys assigned (both present, value changed), and keys
    /// added, each in the new value's order.
    Object {
        removed: Vec<&'v str>,
        changed: Vec<(&'v str, &'v Value)>,
        added: Vec<(&'v str, &'v Value)>,
    },
    /// Elements assigned at their index, then `extras` appended.
    InPlace {
        changed: Vec<(usize, &'v Value)>,
        extras: &'v [Value],
    },
    /// Elements deleted (ascending indices), then `extras` appended.
    Remove {
        removed: Vec<usize>,
        extras: &'v [Value],
    },
}

impl Plan<'_> {
    fn is_empty(&self) -> bool {
        match self {
            Plan::Object {
                removed,
                changed,
                added,
            } => removed.is_empty() && changed.is_empty() && added.is_empty(),
            Plan::InPlace { changed, extras } => changed.is_empty() && extras.is_empty(),
            Plan::Remove { removed, extras } => removed.is_empty() && extras.is_empty(),
        }
    }
}

/// Plan `new` over `old`, or `None` when it is one replacement: different
/// kinds, a scalar, or an array the diff doesn't cover.
fn plan<'v>(old: &'v Value, new: &'v Value) -> Option<Plan<'v>> {
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            let get = |k: &str| old.iter().find(|(o, _)| o == k).map(|(_, v)| v);
            let removed = old
                .iter()
                .filter(|(k, _)| new.iter().all(|(n, _)| n != k))
                .map(|(k, _)| k.as_str())
                .collect();
            let mut changed = Vec::new();
            let mut added = Vec::new();
            for (k, v) in new {
                match get(k) {
                    Some(was) if was.identical(v) => {}
                    Some(_) => changed.push((k.as_str(), v)),
                    None => added.push((k.as_str(), v)),
                }
            }
            Some(Plan::Object {
                removed,
                changed,
                added,
            })
        }
        (Value::Array(old), Value::Array(new)) => in_place(old, new).or_else(|| remove(old, new)),
        _ => None,
    }
}

/// `new` as `old` with elements changed at their index and extras appended,
/// when no element moved: no old element replaced at its index turns up
/// among the new array's changed elements or extras (that is a reorder).
fn in_place<'v>(old: &'v [Value], new: &'v [Value]) -> Option<Plan<'v>> {
    if new.len() < old.len() {
        return None;
    }
    let changed: Vec<(usize, &Value)> = old
        .iter()
        .zip(new)
        .enumerate()
        .filter(|(_, (o, n))| !o.identical(n))
        .map(|(i, (_, n))| (i, n))
        .collect();
    let extras = &new[old.len()..];
    let fresh = || changed.iter().map(|&(_, v)| v).chain(extras);
    let moved = changed
        .iter()
        .any(|&(i, _)| fresh().any(|v| v.identical(&old[i])));
    (!moved).then_some(Plan::InPlace { changed, extras })
}

/// `new` as `old` with elements removed (the rest in order) and extras
/// appended, keeping at least one element, when no removed element comes
/// back among the extras (that is a move). The match is greedy: each element
/// of `new` takes the earliest remaining old element identical to it, so
/// with duplicates the first ones are kept.
fn remove<'v>(old: &'v [Value], new: &'v [Value]) -> Option<Plan<'v>> {
    let mut kept = Vec::new();
    let mut from = 0;
    for v in new {
        match old[from..].iter().position(|o| o.identical(v)) {
            Some(i) => {
                kept.push(from + i);
                from += i + 1;
            }
            None => break,
        }
    }
    if kept.is_empty() {
        return None;
    }
    let extras = &new[kept.len()..];
    let removed: Vec<usize> = (0..old.len()).filter(|i| !kept.contains(i)).collect();
    let moved = removed
        .iter()
        .any(|&i| extras.iter().any(|v| v.identical(&old[i])));
    (!moved).then_some(Plan::Remove { removed, extras })
}

/// Carry out `plan`, which brings the collection at `path` to `value`.
fn apply<M: Mutable + ?Sized>(
    doc: &mut M,
    path: &[Step],
    value: &Value,
    plan: Plan<'_>,
) -> Result<(), EditError> {
    let mut at = path.to_vec();
    let mut child = |step: Step| {
        at.truncate(path.len());
        at.push(step);
        at.clone()
    };
    match plan {
        Plan::Object {
            removed,
            changed,
            added,
        } => {
            for k in removed {
                doc.delete(&child(Step::Field(k.to_owned())))?;
            }
            for (k, v) in changed {
                assign(doc, &child(Step::Field(k.to_owned())), v)?;
            }
            for (k, v) in added {
                doc.set(&child(Step::Field(k.to_owned())), v)?;
            }
            Ok(())
        }
        Plan::InPlace { changed, extras } => {
            for (i, v) in changed {
                assign(doc, &child(Step::Index(i as i64)), v)?;
            }
            append(doc, path, value, extras)
        }
        Plan::Remove { removed, extras } => {
            for &i in removed.iter().rev() {
                doc.delete(&child(Step::Index(i as i64)))?;
            }
            append(doc, path, value, extras)
        }
    }
}

/// Append `extras` to the array at `path` the way `+=` does: in place when
/// the format can, else by setting the whole `value`, which the array now
/// holds as a prefix.
fn append<M: Mutable + ?Sized>(
    doc: &mut M,
    path: &[Step],
    value: &Value,
    extras: &[Value],
) -> Result<(), EditError> {
    if extras.is_empty() {
        return Ok(());
    }
    match doc.append(path, extras) {
        Some(done) => done,
        None => doc.set(path, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_of(old: Value, new: Value) -> Option<String> {
        plan(&old, &new).map(|p| format!("{p:?}"))
    }

    #[test]
    fn objects_always_diff() {
        let p = plan_of(
            json!({"a": 1, "b": 2, "c": 3}),
            json!({"d": 4, "c": 0, "a": 1}),
        );
        assert_eq!(
            p.as_deref(),
            Some(r#"Object { removed: ["b"], changed: [("c", Int(0))], added: [("d", Int(4))] }"#)
        );
        // Unchanged is `identical`: `1.0` over `1` is a change.
        let p = plan_of(json!({"a": 1}), json!({"a": 1.0}));
        assert!(p.unwrap().contains(r#"changed: [("a", Float(1.0))]"#));
    }

    #[test]
    fn arrays_change_in_place_or_remove_or_replace() {
        let arr = |v: &[i64]| Value::Array(v.iter().map(|&i| Value::Int(i)).collect());
        let p = |old: &[i64], new: &[i64]| plan_of(arr(old), arr(new));
        // Identical, and pure extension, are in place.
        assert_eq!(
            p(&[1, 2], &[1, 2]).as_deref(),
            Some("InPlace { changed: [], extras: [] }")
        );
        assert_eq!(
            p(&[1, 2], &[1, 2, 3]).as_deref(),
            Some("InPlace { changed: [], extras: [Int(3)] }")
        );
        assert_eq!(
            p(&[], &[1]).as_deref(),
            Some("InPlace { changed: [], extras: [Int(1)] }")
        );
        // A changed element that moved nowhere is set at its index.
        assert_eq!(
            p(&[1, 2, 3], &[1, 5, 3]).as_deref(),
            Some("InPlace { changed: [(1, Int(5))], extras: [] }")
        );
        // Removals, with or without extras.
        assert_eq!(
            p(&[1, 2, 3], &[1, 3]).as_deref(),
            Some("Remove { removed: [1], extras: [] }")
        );
        assert_eq!(
            p(&[1, 2, 3], &[1, 3, 4]).as_deref(),
            Some("Remove { removed: [1], extras: [Int(4)] }")
        );
        assert_eq!(
            p(&[1, 2, 3], &[3]).as_deref(),
            Some("Remove { removed: [0, 1], extras: [] }")
        );
        // Duplicates: the earliest match is kept.
        assert_eq!(
            p(&[1, 1, 2], &[1, 2]).as_deref(),
            Some("Remove { removed: [1], extras: [] }")
        );
        assert_eq!(
            p(&[1, 2, 1], &[1, 1]).as_deref(),
            Some("Remove { removed: [1], extras: [] }")
        );
        // Reorders, moves, and nothing kept are one replacement.
        assert_eq!(p(&[1, 2, 3], &[3, 2, 1]), None);
        assert_eq!(p(&[1, 2], &[2, 1]), None);
        assert_eq!(p(&[1, 2, 3], &[1, 3, 2]), None);
        assert_eq!(p(&[1, 2], &[]), None);
        assert_eq!(p(&[1, 2], &[3]), None);
        // Kinds must match.
        assert_eq!(plan_of(json!([1]), json!({"a": 1})), None);
        assert_eq!(plan_of(json!(1), json!(2)), None);
    }
}
