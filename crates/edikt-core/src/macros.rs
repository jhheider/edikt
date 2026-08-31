//! The `json!` value-construction macro.
//!
//! Parity with `serde_json` / `jsonc_parser`, whose `json!` is the shape every
//! Rust user already has in their fingers (jhheider/edikt#67). It builds a
//! [`Value`](crate::Value), not a document: the CST is what round-trips bytes,
//! and this is the data model an edit *writes into* it.
//!
//! Deliberately not a re-parse of a JSON string literal - the macro expands to
//! direct constructor calls, so a malformed literal is a compile error rather
//! than a runtime one, and interpolated expressions never go through a
//! serialize/parse round trip.

/// Construct a [`Value`](crate::Value) from JSON-ish literal syntax.
///
/// ```
/// use edikt_core::{Value, json};
///
/// let v = json!({
///     "name": "edikt",
///     "version": 1,
///     "lossless": true,
///     "formats": ["jsonc", "toml", "yaml"],
///     "parent": null,
/// });
/// assert_eq!(v.to_json(), r#"{"name":"edikt","version":1,"lossless":true,"formats":["jsonc","toml","yaml"],"parent":null}"#);
/// ```
///
/// Keys may be literals or parenthesised expressions, and any value position
/// accepts an expression that converts into a `Value`:
///
/// ```
/// use edikt_core::{Value, json};
///
/// let key = "dynamic";
/// let count = 3_i64;
/// let v = json!({ (key): count, "doubled": count * 2 });
/// assert_eq!(v.to_json(), r#"{"dynamic":3,"doubled":6}"#);
/// ```
///
/// Nesting, arrays, and trailing commas all work:
///
/// ```
/// use edikt_core::{Value, json};
///
/// let v = json!([1, [2, 3], { "k": [true, null] },]);
/// assert_eq!(v.to_json(), r#"[1,[2,3],{"k":[true,null]}]"#);
/// ```
#[macro_export]
macro_rules! json {
    (null) => { $crate::Value::Null };

    ([]) => { $crate::Value::Array(::std::vec::Vec::new()) };
    ([ $($elem:tt)+ ]) => {
        $crate::Value::Array($crate::json_array![[] $($elem)+])
    };

    ({}) => { $crate::Value::Object(::std::vec::Vec::new()) };
    ({ $($member:tt)+ }) => {
        $crate::Value::Object($crate::json_object![[] $($member)+])
    };

    // Any other single token tree is an ordinary expression. `Value` itself
    // converts through the identity `From` impl, so `json!(existing)` nests.
    ($other:expr) => { $crate::Value::from($other) };
}

/// Accumulate array elements. Internal to [`json!`]; not a stable API.
#[macro_export]
#[doc(hidden)]
macro_rules! json_array {
    // Done: no more input.
    ([$($done:expr),*]) => { ::std::vec![$($done),*] };
    ([$($done:expr),*] ,) => { ::std::vec![$($done),*] };

    // A nested array or object element must be matched before the general
    // `expr` arm, because `[..]` / `{..}` are not expressions here.
    ([$($done:expr),*] [$($arr:tt)*] , $($rest:tt)*) => {
        $crate::json_array![[$($done,)* $crate::json!([$($arr)*])] $($rest)*]
    };
    ([$($done:expr),*] [$($arr:tt)*]) => {
        $crate::json_array![[$($done,)* $crate::json!([$($arr)*])]]
    };
    ([$($done:expr),*] {$($obj:tt)*} , $($rest:tt)*) => {
        $crate::json_array![[$($done,)* $crate::json!({$($obj)*})] $($rest)*]
    };
    ([$($done:expr),*] {$($obj:tt)*}) => {
        $crate::json_array![[$($done,)* $crate::json!({$($obj)*})]]
    };

    // `null` ahead of the `expr` arms: once captured as an `expr` fragment it
    // can no longer re-match the keyword arm in `json!`, and would resolve as a
    // path to a value named `null`. (serde_json treats it as a keyword too.)
    ([$($done:expr),*] null , $($rest:tt)*) => {
        $crate::json_array![[$($done,)* $crate::Value::Null] $($rest)*]
    };
    ([$($done:expr),*] null) => {
        $crate::json_array![[$($done,)* $crate::Value::Null]]
    };

    ([$($done:expr),*] $next:expr , $($rest:tt)*) => {
        $crate::json_array![[$($done,)* $crate::json!($next)] $($rest)*]
    };
    ([$($done:expr),*] $last:expr) => {
        $crate::json_array![[$($done,)* $crate::json!($last)]]
    };
}

/// Accumulate object members. Internal to [`json!`]; not a stable API.
#[macro_export]
#[doc(hidden)]
macro_rules! json_object {
    ([$($done:expr),*]) => { ::std::vec![$($done),*] };
    ([$($done:expr),*] ,) => { ::std::vec![$($done),*] };

    // Nested composite values, ahead of the general `expr` arm (see above).
    ([$($done:expr),*] $key:tt : [$($arr:tt)*] , $($rest:tt)*) => {
        $crate::json_object![[$($done,)* ($crate::json_key!($key), $crate::json!([$($arr)*]))] $($rest)*]
    };
    ([$($done:expr),*] $key:tt : [$($arr:tt)*]) => {
        $crate::json_object![[$($done,)* ($crate::json_key!($key), $crate::json!([$($arr)*]))]]
    };
    ([$($done:expr),*] $key:tt : {$($obj:tt)*} , $($rest:tt)*) => {
        $crate::json_object![[$($done,)* ($crate::json_key!($key), $crate::json!({$($obj)*}))] $($rest)*]
    };
    ([$($done:expr),*] $key:tt : {$($obj:tt)*}) => {
        $crate::json_object![[$($done,)* ($crate::json_key!($key), $crate::json!({$($obj)*}))]]
    };

    // See the `null` note in `json_array!`.
    ([$($done:expr),*] $key:tt : null , $($rest:tt)*) => {
        $crate::json_object![[$($done,)* ($crate::json_key!($key), $crate::Value::Null)] $($rest)*]
    };
    ([$($done:expr),*] $key:tt : null) => {
        $crate::json_object![[$($done,)* ($crate::json_key!($key), $crate::Value::Null)]]
    };

    ([$($done:expr),*] $key:tt : $val:expr , $($rest:tt)*) => {
        $crate::json_object![[$($done,)* ($crate::json_key!($key), $crate::json!($val))] $($rest)*]
    };
    ([$($done:expr),*] $key:tt : $val:expr) => {
        $crate::json_object![[$($done,)* ($crate::json_key!($key), $crate::json!($val))]]
    };
}

/// Render an object key. Internal to [`json!`]; not a stable API.
///
/// A parenthesised key is an expression (`(key): v`); anything else is taken
/// for its literal text, so bare and quoted keys both work.
#[macro_export]
#[doc(hidden)]
macro_rules! json_key {
    (($key:expr)) => {
        ::std::string::ToString::to_string(&$key)
    };
    ($key:literal) => {
        ::std::string::ToString::to_string(&$key)
    };
    ($key:tt) => {
        ::std::stringify!($key).to_string()
    };
}

#[cfg(test)]
mod tests {
    use crate::Value;

    #[test]
    fn builds_scalars_and_empties() {
        assert_eq!(json!(null), Value::Null);
        assert_eq!(json!(true), Value::Bool(true));
        assert_eq!(json!(42), Value::Int(42));
        assert_eq!(json!(2.5), Value::Float(2.5));
        assert_eq!(json!("s"), Value::Str("s".into()));
        assert_eq!(json!([]).to_json(), "[]");
        assert_eq!(json!({}).to_json(), "{}");
    }

    #[test]
    fn nests_arrays_and_objects_and_tolerates_trailing_commas() {
        let v = json!({
            "a": [1, 2, [3, {"deep": true}]],
            "b": { "c": { "d": null } },
        });
        assert_eq!(
            v.to_json(),
            r#"{"a":[1,2,[3,{"deep":true}]],"b":{"c":{"d":null}}}"#
        );
        assert_eq!(json!([1, 2, 3,]).to_json(), "[1,2,3]");
    }

    #[test]
    fn object_keys_preserve_insertion_order() {
        // Order is user-visible in every format edikt emits, so the macro must
        // not reorder; a HashMap-backed builder would.
        let v = json!({ "z": 1, "a": 2, "m": 3 });
        assert_eq!(v.to_json(), r#"{"z":1,"a":2,"m":3}"#);
    }

    #[test]
    fn interpolates_expressions_in_key_and_value_position() {
        let key = "dynamic";
        let n = 3_i64;
        let owned = String::from("owned");
        let v = json!({ (key): n * 2, "s": owned, "computed": n > 1 });
        assert_eq!(v.to_json(), r#"{"dynamic":6,"s":"owned","computed":true}"#);
    }

    #[test]
    fn nests_an_existing_value_through_the_identity_conversion() {
        let inner = json!({ "already": "built" });
        let outer = json!({ "wrapped": inner });
        assert_eq!(outer.to_json(), r#"{"wrapped":{"already":"built"}}"#);
    }

    #[test]
    fn converts_options_vecs_and_slices() {
        let some: Option<i64> = Some(7);
        let none: Option<i64> = None;
        let xs: Vec<i64> = vec![1, 2];
        let slice: &[&str] = &["a", "b"];
        let v = json!({ "some": some, "none": none, "xs": xs, "slice": slice });
        assert_eq!(
            v.to_json(),
            r#"{"some":7,"none":null,"xs":[1,2],"slice":["a","b"]}"#
        );
    }

    #[test]
    fn collects_pairs_into_an_object() {
        let v: Value = [("a", 1_i64), ("b", 2)].into_iter().collect();
        assert_eq!(v.to_json(), r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn integer_widths_all_convert() {
        assert_eq!(json!(1_i8), Value::Int(1));
        assert_eq!(json!(1_u32), Value::Int(1));
        assert_eq!(json!(-1_isize), Value::Int(-1));
        assert_eq!(json!(1.5_f32), Value::Float(1.5));
    }
}
