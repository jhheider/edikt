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
