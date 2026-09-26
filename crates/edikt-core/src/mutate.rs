//! The mutation driver every format shares.
//!
//! A format provides the primitive, format-preserving edits ([`Mutable`]:
//! read the value at a path, set it, delete it); [`apply_mutation`] turns a
//! mutation expression (`=`, `|=`, `+=`, `del`, `|`-chains of them, and
//! path-expression targets through [`crate::lower_mutation`]) into calls on
//! those primitives. The format decides what a path means; the driver decides
//! what the expression means, once, for all of them. That includes what an
//! assignment of a collection over a collection means: element edits, not
//! one replacement ([`assign`]).

use crate::ast::{Expr, Step};
use crate::error::EditError;
use crate::eval::{eval, expand_iter_paths};
use crate::value::Value;

mod diff;

pub use diff::{assign, diff_grows};

/// Which mutation a target path belongs to, for [`Mutable::check_path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    /// `path = value`
    Assign,
    /// `path |= f`
    Update,
    /// `path += value`
    Add,
    /// `del(path)`
    Delete,
}

/// The primitive edits a format provides to [`apply_mutation`].
///
/// Paths handed to `value_at`, `set`, `add` and `delete` are plain paths.
/// With [`Mutable::FANS_OUT`] they are also iterate-free for everything but
/// `=`'s create check and `delete` (a format's own `del(.a[])` semantics).
pub trait Mutable {
    /// Whether `[]` in an assignment target fans out into one edit per
    /// element (`.a[] |= f`). A format without it sees the iterate path in
    /// `set`/`value_at` and answers for it itself.
    const FANS_OUT: bool = true;

    /// The document value an assignment's right side, and a path-expression
    /// target, is evaluated against.
    fn whole(&self) -> Value;

    /// The value at `path`, or `None` when it doesn't resolve.
    fn value_at(&self, path: &[Step]) -> Option<Value>;

    /// Set the value at `path`, format-preserving (creating it where the
    /// format can).
    fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError>;

    /// Delete the value at `path`.
    fn delete(&mut self, path: &[Step]) -> Result<(), EditError>;

    /// Append `items` to the array at `path` in place, format-preserving.
    /// `None` (the default) means the format has no in-place append, and
    /// `+=` writes the concatenated array through `set` instead.
    fn append(&mut self, path: &[Step], items: &[Value]) -> Option<Result<(), EditError>> {
        let _ = (path, items);
        None
    }

    /// `path += addend`, where `current` is the value there: array onto
    /// array goes through [`Mutable::append`] when the format has one;
    /// object onto object is jq's shallow merge, written as one keyed
    /// [`assign`] per right-hand entry, so the object's other entries keep
    /// their bytes;
    /// anything else writes `current + addend`.
    fn add(&mut self, path: &[Step], current: &Value, addend: &Value) -> Result<(), EditError> {
        if let (Value::Array(_), Value::Array(items)) = (current, addend)
            && let Some(done) = self.append(path, items)
        {
            return done;
        }
        if let (Value::Object(_), Value::Object(entries)) = (current, addend) {
            let mut at = path.to_vec();
            for (k, v) in entries {
                at.push(Step::Field(k.clone()));
                assign(self, &at, v)?;
                at.pop();
            }
            return Ok(());
        }
        let sum = add_values(current, addend)?;
        self.set(path, &sum)
    }

    /// What `|=`/`+=` does when `path` doesn't resolve. The default is an
    /// error; a document mapped over leniently can make it a no-op.
    fn miss(&self, path: &[Step]) -> Result<(), EditError> {
        let _ = path;
        Err(EditError::new("path not found"))
    }

    /// Whether an assignment of `value` over the collection `current` at
    /// `path` may be written as element edits ([`assign`]). A format
    /// declines where it can't make them in place, and the collection is
    /// then replaced through [`Mutable::set`]. The default accepts.
    fn diffs(&self, path: &[Step], current: &Value, value: &Value) -> bool {
        let _ = (path, current, value);
        true
    }

    /// Reject a target path this format can't edit before anything happens.
    /// The default accepts every path.
    fn check_path(&self, path: &[Step], kind: MutationKind) -> Result<(), EditError> {
        let _ = (path, kind);
        Ok(())
    }
}

/// `current + addend` with the evaluator's `+` (numbers add, strings and
/// arrays concatenate, objects merge, `null` is the identity).
pub fn add_values(current: &Value, addend: &Value) -> Result<Value, EditError> {
    Ok(crate::eval::add(current, addend)?)
}

/// Apply a mutation expression to `doc` through its [`Mutable`] primitives.
///
/// `=` and `+=` evaluate their right side against the whole document; `|=`
/// against the value it replaces. A path-expression target is lowered to one
/// plain-path edit per match first, and `a | b` applies `a` then `b`.
pub fn apply_mutation<M: Mutable + ?Sized>(doc: &mut M, expr: &Expr) -> Result<(), EditError> {
    // A path-expression target (`(.xs[] | select(...) | .n) = v`, #88)
    // resolves to concrete paths first; each is then an ordinary edit.
    if let Some(each) = crate::lower_mutation(expr, || doc.whole())? {
        for e in &each {
            apply_mutation(doc, e)?;
        }
        return Ok(());
    }
    match expr {
        Expr::Assign(lhs, rhs) => {
            let steps = target(doc, lhs, MutationKind::Assign)?;
            let whole = doc.whole();
            let value = eval_one(rhs, &whole)?;
            if fans_out::<M>(steps) {
                // `.a[] = x`: every element gets the same value.
                return set_each(doc, steps, &whole, true, |_| Ok(value.clone()));
            }
            assign(doc, steps, &value)
        }
        Expr::UpdateAssign(lhs, rhs) => {
            let steps = target(doc, lhs, MutationKind::Update)?;
            if fans_out::<M>(steps) {
                // `.a[] |= f`: map `f` over each element.
                let whole = doc.whole();
                return set_each(doc, steps, &whole, false, |current| eval_one(rhs, current));
            }
            let Some(current) = doc.value_at(steps) else {
                return doc.miss(steps);
            };
            let value = eval_one(rhs, &current)?;
            assign(doc, steps, &value)
        }
        Expr::AddAssign(lhs, rhs) => {
            let steps = target(doc, lhs, MutationKind::Add)?;
            let whole = doc.whole();
            let addend = eval_one(rhs, &whole)?;
            if fans_out::<M>(steps) {
                // `.a[] += x`: jq's `.a[] |= . + x`, per element, each
                // through the format's own `add` (in-place append, keyed
                // merge) like a hand-typed `.a[i] += x`.
                for path in expand_iter_paths(steps, &whole)? {
                    let Some(current) = doc.value_at(&path) else {
                        return doc.miss(&path);
                    };
                    doc.add(&path, &current, &addend)?;
                }
                return Ok(());
            }
            let Some(current) = doc.value_at(steps) else {
                return doc.miss(steps);
            };
            doc.add(steps, &current, &addend)
        }
        Expr::Pipe(a, b) => {
            apply_mutation(doc, a)?;
            apply_mutation(doc, b)
        }
        Expr::Call(name, args) if name == "del" => {
            if args.len() != 1 {
                return Err(EditError::new("del(...) takes one path argument"));
            }
            let steps = args[0]
                .as_path()
                .ok_or_else(|| EditError::new("del(...) takes a path"))?;
            doc.check_path(steps, MutationKind::Delete)?;
            doc.delete(steps)
        }
        _ => Err(EditError::new(
            "expected an assignment (`path = value`) or `del(path)`",
        )),
    }
}

/// An assignment's left side as a plain path the format accepts.
fn target<'e, M: Mutable + ?Sized>(
    doc: &M,
    lhs: &'e Expr,
    kind: MutationKind,
) -> Result<&'e [Step], EditError> {
    let steps = lhs
        .as_path()
        .ok_or_else(|| EditError::new("left side of an assignment must be a path"))?;
    doc.check_path(steps, kind)?;
    Ok(steps)
}

fn fans_out<M: Mutable + ?Sized>(steps: &[Step]) -> bool {
    M::FANS_OUT && steps.contains(&Step::Iterate)
}

/// Apply `f` to each element selected by `steps` (which contains at least one
/// `Step::Iterate`), assigning each through the format's ordinary
/// index-keyed edits, so every replacement is as surgical as a hand-typed
/// `.a[i] = ..`.
/// `create` is true for plain assignment (`=`), where an expansion resolving
/// to nothing means "you asked to create elements through `[]`" and errors;
/// the update forms (`|=`, `+=`) treat an empty expansion as a miss (a no-op),
/// jq-shaped.
fn set_each<M: Mutable + ?Sized>(
    doc: &mut M,
    steps: &[Step],
    whole: &Value,
    create: bool,
    f: impl Fn(&Value) -> Result<Value, EditError>,
) -> Result<(), EditError> {
    let paths = expand_iter_paths(steps, whole)?;
    if paths.is_empty() && create {
        return Err(EditError::new("cannot create through `[]`"));
    }
    for path in &paths {
        let Some(current) = doc.value_at(path) else {
            return doc.miss(path);
        };
        let value = f(&current)?;
        assign(doc, path, &value)?;
    }
    Ok(())
}

/// The first result of `expr` over `input`: an assignment's right side.
fn eval_one(expr: &Expr, input: &Value) -> Result<Value, EditError> {
    eval(expr, input)?
        .into_iter()
        .next()
        .ok_or_else(|| EditError::new("right side of the assignment produced no value"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    /// An in-memory document: `set`/`delete` go through the evaluator, so the
    /// driver's dispatch is tested apart from any CST.
    struct Mem {
        v: Value,
        adds: usize,
    }

    impl Mutable for Mem {
        fn whole(&self) -> Value {
            self.v.clone()
        }
        fn value_at(&self, path: &[Step]) -> Option<Value> {
            let got = eval(&Expr::Path(path.to_vec()), &self.v).ok()?;
            got.into_iter().next().filter(|v| *v != Value::Null)
        }
        fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
            let e = Expr::Assign(
                Box::new(Expr::Path(path.to_vec())),
                Box::new(Expr::Literal(value.clone())),
            );
            self.v = eval(&e, &self.v)?.remove(0);
            Ok(())
        }
        fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
            let e = Expr::Call("del".into(), vec![Expr::Path(path.to_vec())]);
            self.v = eval(&e, &self.v)?.remove(0);
            Ok(())
        }
        fn add(&mut self, path: &[Step], current: &Value, addend: &Value) -> Result<(), EditError> {
            self.adds += 1;
            let sum = add_values(current, addend)?;
            self.set(path, &sum)
        }
    }

    fn run(doc: Value, expr: &str) -> Result<(Value, usize), EditError> {
        let mut m = Mem { v: doc, adds: 0 };
        apply_mutation(&mut m, &parse(expr).unwrap())?;
        Ok((m.v, m.adds))
    }

    #[test]
    fn dispatches_every_mutation_form() {
        let doc = || json!({"a": 1, "xs": [1, 2], "s": "x"});
        let with = |a: Value, xs: Value, s: Value| json!({"a": a, "xs": xs, "s": s});
        let ok = |e: &str| run(doc(), e).unwrap().0;
        assert_eq!(ok(".a = .xs[1]"), with(json!(2), json!([1, 2]), json!("x")));
        assert_eq!(ok(".a |= . + 5"), with(json!(6), json!([1, 2]), json!("x")));
        let (v, adds) = run(doc(), ".s += \"y\"").unwrap();
        assert_eq!((v, adds), (with(json!(1), json!([1, 2]), json!("xy")), 1));
        assert_eq!(
            ok(".xs[] |= . * 10"),
            with(json!(1), json!([10, 20]), json!("x"))
        );
        assert_eq!(ok(".xs[] += 1"), with(json!(1), json!([2, 3]), json!("x")));
        assert_eq!(ok(".xs[] = 0"), with(json!(1), json!([0, 0]), json!("x")));
        assert_eq!(
            ok(".a = 5 | .s = \"z\""),
            with(json!(5), json!([1, 2]), json!("z"))
        );
        assert_eq!(ok("del(.a)"), json!({"xs": [1, 2], "s": "x"}));
        assert_eq!(
            ok("(.xs[] | select(. == 2)) = 9"),
            with(json!(1), json!([1, 9]), json!("x"))
        );
    }

    /// A document on the trait's default `add`, recording each `set` path.
    struct Keyed {
        mem: Mem,
        sets: Vec<String>,
    }

    impl Mutable for Keyed {
        fn whole(&self) -> Value {
            self.mem.whole()
        }
        fn value_at(&self, path: &[Step]) -> Option<Value> {
            self.mem.value_at(path)
        }
        fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
            self.sets.push(crate::render_path(path));
            self.mem.set(path, value)
        }
        fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
            self.mem.delete(path)
        }
    }

    #[test]
    fn object_add_assign_is_one_keyed_set_per_right_hand_entry() {
        // #106: `+=` of an object is jq's shallow merge, written key by key
        // so a format leaves the object's other entries alone.
        let run = |expr: &str| {
            let mut k = Keyed {
                mem: Mem {
                    v: json!({"o": {"a": 1, "b": 2}, "xs": [{"a": 1}, {"b": 2}]}),
                    adds: 0,
                },
                sets: Vec::new(),
            };
            apply_mutation(&mut k, &parse(expr).unwrap()).unwrap();
            (k.mem.v, k.sets)
        };
        let (v, sets) = run(".o += {b: 3, c: 4}");
        assert_eq!(sets, [".o.b", ".o.c"]);
        assert_eq!(
            v.to_json(),
            r#"{"o":{"a":1,"b":3,"c":4},"xs":[{"a":1},{"b":2}]}"#
        );
        // Fan-out and path-expression targets merge per element the same way.
        let (_v, sets) = run(".xs[] += {c: 0}");
        assert_eq!(sets, [".xs[0].c", ".xs[1].c"]);
        let (_v, sets) = run("(.xs[] | select(.b)) += {c: 0}");
        assert_eq!(sets, [".xs[1].c"]);
        // Anything but object onto object still writes the sum.
        let (_v, sets) = run(".o.a += 1");
        assert_eq!(sets, [".o.a"]);
    }

    #[test]
    fn misses_and_non_mutations_error() {
        let doc = json!({"xs": []});
        let err = |e: &str| run(doc.clone(), e).unwrap_err().to_string();
        assert_eq!(err(".nope |= 1"), "path not found");
        assert_eq!(err(".nope += 1"), "path not found");
        assert_eq!(err(".xs[] = 1"), "cannot create through `[]`");
        assert_eq!(
            err(".a"),
            "expected an assignment (`path = value`) or `del(path)`"
        );
        assert_eq!(err("del(.a; .b)"), "del(...) takes one path argument");
        assert_eq!(
            err(".a = .xs[]"),
            "right side of the assignment produced no value"
        );
        // An update over an empty expansion is a no-op, jq-shaped.
        assert_eq!(run(doc.clone(), ".xs[] |= 1").unwrap().0, doc);
    }

    /// A document logging each primitive edit, with or without an in-place
    /// append, optionally declining every diff.
    struct Logged {
        mem: Mem,
        ops: Vec<String>,
        appends: bool,
        declines: bool,
    }

    impl Mutable for Logged {
        fn whole(&self) -> Value {
            self.mem.whole()
        }
        fn value_at(&self, path: &[Step]) -> Option<Value> {
            let got = eval(&Expr::Path(path.to_vec()), &self.mem.v).ok()?;
            got.into_iter().next()
        }
        fn set(&mut self, path: &[Step], value: &Value) -> Result<(), EditError> {
            let at = crate::render_path(path);
            self.ops.push(format!("set {at} {}", value.to_json()));
            self.mem.set(path, value)
        }
        fn delete(&mut self, path: &[Step]) -> Result<(), EditError> {
            self.ops.push(format!("del {}", crate::render_path(path)));
            self.mem.delete(path)
        }
        fn append(&mut self, path: &[Step], items: &[Value]) -> Option<Result<(), EditError>> {
            if !self.appends {
                return None;
            }
            let at = crate::render_path(path);
            self.ops.push(format!(
                "append {at} {}",
                Value::Array(items.to_vec()).to_json()
            ));
            let current = self.value_at(path).unwrap();
            let sum = add_values(&current, &Value::Array(items.to_vec())).unwrap();
            Some(self.mem.set(path, &sum))
        }
        fn diffs(&self, _: &[Step], _: &Value, _: &Value) -> bool {
            !self.declines
        }
    }

    fn logged(doc: Value, expr: &str, appends: bool, declines: bool) -> (Value, Vec<String>) {
        let mut d = Logged {
            mem: Mem { v: doc, adds: 0 },
            ops: Vec::new(),
            appends,
            declines,
        };
        apply_mutation(&mut d, &parse(expr).unwrap()).unwrap();
        (d.mem.v, d.ops)
    }

    #[test]
    fn a_collection_over_a_collection_is_diffed() {
        // #117: every form that writes a value diffs it against the old one.
        let doc = || json!({"o": {"a": 1, "b": {"x": 1, "y": 2}, "c": 3}, "k": [1, 2, 3]});
        let ops = |expr: &str| {
            let (v, ops) = logged(doc(), expr, true, false);
            // The edits reach the value the expression means.
            let want = eval(&parse(expr).unwrap(), &doc()).unwrap().remove(0);
            assert_eq!(v, want, "{expr}");
            ops
        };
        // Objects: removed keys deleted, changed ones recursed into, new ones
        // added after the rest, unchanged ones untouched.
        assert_eq!(
            ops(".o = {a: 1, b: {x: 1, y: 5}, d: 4}"),
            ["del .o.c", "set .o.b.y 5", "set .o.d 4"]
        );
        assert_eq!(ops(".o |= del(.a)"), ["del .o.a"]);
        assert_eq!(ops(". = .").len(), 0);
        assert_eq!(ops(".o = .o").len(), 0);
        // Arrays: removals back to front, then extras appended; in-place
        // changes; identical is a no-op.
        assert_eq!(ops(".k = [1, 3]"), ["del .k[1]"]);
        assert_eq!(ops(".k = [2]"), ["del .k[2]", "del .k[0]"]);
        assert_eq!(ops(".k |= [.[0], .[2]]"), ["del .k[1]"]);
        assert_eq!(ops(".k = [1, 3, 4]"), ["del .k[1]", "append .k [4]"]);
        assert_eq!(ops(".k = [1, 2, 3, 4]"), ["append .k [4]"]);
        assert_eq!(ops(".k = [1, 5, 3]"), ["set .k[1] 5"]);
        assert_eq!(ops(".k = [1, 2, 3]").len(), 0);
        // A reorder, or a new array keeping nothing, is one replacement.
        assert_eq!(ops(".k = [3, 2, 1]"), ["set .k [3,2,1]"]);
        assert_eq!(ops(".k = []"), ["set .k []"]);
        assert_eq!(ops(".k = [9]"), ["set .k [9]"]);
        // A different kind, or a scalar, is a plain set.
        assert_eq!(ops(".k = {a: 1}"), [r#"set .k {"a":1}"#]);
        assert_eq!(ops(".o.a = 2"), ["set .o.a 2"]);
        // A merge's shared key, and each element of a fan-out, diff too.
        assert_eq!(ops(".o += {b: {x: 1}}"), ["del .o.b.y"]);
        assert_eq!(
            ops(".o = {a: [1], b: {x: 1, y: 2}, c: 3} | .o.a[] = 7"),
            ["set .o.a [1]", "set .o.a[0] 7"]
        );
    }

    #[test]
    fn a_diff_falls_back_to_set_when_the_format_cant_append_or_declines() {
        let doc = || json!({"k": [1, 2, 3], "o": {"a": 1}});
        // No in-place append: the removals, then the whole array, which now
        // holds the kept elements as a prefix.
        let (v, ops) = logged(doc(), ".k = [1, 3, 4]", false, false);
        assert_eq!(ops, ["del .k[1]", "set .k [1,3,4]"]);
        assert_eq!(v, json!({"k": [1, 3, 4], "o": {"a": 1}}));
        // Declined: one replacement, but identical is still a no-op.
        let (_, ops) = logged(doc(), ".o = {a: 1, b: 2}", true, true);
        assert_eq!(ops, [r#"set .o {"a":1,"b":2}"#]);
        let (_, ops) = logged(doc(), ".o = {a: 1}", true, true);
        assert!(ops.is_empty());
    }
}
