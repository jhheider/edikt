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
