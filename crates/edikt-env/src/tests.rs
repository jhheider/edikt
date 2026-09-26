use super::*;
use edikt_core::eval;
use edikt_core::parse as parse_expr;

const SAMPLE: &str =
    "# service env\nDATABASE_URL=postgres://localhost/app\nDEBUG = true\nEMPTY=\nWITH_HASH=a#b\n";

fn q(src: &str, expr: &str) -> Vec<Value> {
    let v = parse(src).unwrap().to_value();
    eval(&parse_expr(expr).unwrap(), &v).unwrap()
}

fn edit_src(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    apply(&mut doc, &parse_expr(expr).unwrap()).unwrap();
    doc.to_source()
}

fn cedit(src: &str, expr: &str) -> String {
    let mut doc = parse(src).unwrap();
    edikt_core::apply_comment_mutation(&mut doc, &parse_expr(expr).unwrap()).unwrap();
    doc.to_source()
}

/// Set `key` to `value` on `src` in `dialect`; on success, assert the
/// written file reads back with exactly that key and value.
fn set_round_trip(src: &str, dialect: Dialect, key: &str, value: &str) -> Result<(), String> {
    let mut doc = parse_with(src, dialect).unwrap();
    doc.set(key, &Value::Str(value.into()))
        .map_err(|e| e.to_string())?;
    let back = parse_with(&doc.to_source(), dialect).unwrap();
    assert_eq!(
        back.value_at(key),
        Some(Value::Str(value.into())),
        "{key:?} = {value:?} wrote {:?}",
        doc.to_source()
    );
    Ok(())
}

#[test]
fn a_value_or_key_that_cannot_read_back_errors() {
    use Dialect::{Punctuated as P, Spaced as S};
    // Values: a line break would inject an entry (`B=evil`); surrounding
    // whitespace reads back trimmed. Both for existing and new keys.
    for (d, src) in [(P, "A=1\n"), (S, "A 1\n")] {
        for key in ["A", "NEW"] {
            for bad in ["x\nB=evil", "x\rB=evil", " x", "x\t"] {
                let err = set_round_trip(src, d, key, bad).unwrap_err();
                assert!(err.contains("no quoting"), "{err}");
            }
        }
        // No injected entry survives the refused edit.
        let mut doc = parse_with(src, d).unwrap();
        assert!(doc.set("A", &Value::Str("x\nB=evil".into())).is_err());
        assert_eq!(doc.to_source(), src);
    }
    // Keys: a comment marker, a separator, or surrounding whitespace.
    for bad in ["#A", "!A", "A=B", "A:B", " A", "A ", "A\nB"] {
        let err = set_round_trip("A=1\n", P, bad, "v").unwrap_err();
        assert!(err.contains("can't hold the key"), "{bad:?}: {err}");
    }
    for bad in ["#A", "A B", "A\tB", ""] {
        let err = set_round_trip("A 1\n", S, bad, "v").unwrap_err();
        assert!(err.contains("can't hold the key"), "{bad:?}: {err}");
    }
    // What the formats can hold still writes, and reads back as itself.
    for (key, value) in [
        ("B", "a#b"),
        ("B", "x = y"),
        ("B", "\"quoted\""),
        ("B", ""),
        ("A B", "v"),
        ("B!", "v"),
    ] {
        set_round_trip("A=1\n", P, key, value).unwrap();
    }
    set_round_trip("A 1\n", S, "Subsystem", "sftp /usr/lib/sftp-server").unwrap();
    set_round_trip("A 1\n", S, "K=V", "x").unwrap();
}

#[test]
fn inserted_lines_take_the_files_crlf() {
    let src = "A=1\r\nB=2\r\n";
    assert_eq!(edit_src(src, r#".C = "3""#), "A=1\r\nB=2\r\nC=3\r\n");
    assert_eq!(
        edit_src("A=1\r\nB=2", r#".C = "3""#),
        "A=1\r\nB=2\r\nC=3\r\n"
    );
    assert_eq!(cedit(src, ".B.# = \"n\""), "A=1\r\n# n\r\nB=2\r\n");
    let mut sp = parse_spaced("Port 22\r\n").unwrap();
    sp.set("X", &Value::Str("y".into())).unwrap();
    assert_eq!(sp.to_source(), "Port 22\r\nX y\r\n");
}

#[test]
fn a_foot_comment_below_an_unterminated_last_line_gets_its_own_line() {
    // Used to glue on: `A=1# x`, which reads back as the value `1# x`.
    let out = cedit("A=1", ".A.#.foot = \"x\"");
    assert_eq!(out, "A=1\n# x");
    assert_eq!(q(&out, ".A"), vec![Value::Str("1".into())]);
}

#[test]
fn a_bom_is_not_part_of_the_first_key() {
    // `.A` used to miss, and `.A = 2` appended a duplicate `A=2`.
    let src = "\u{FEFF}A=1\nB=2\n";
    assert_eq!(q(src, ".A"), vec![Value::Str("1".into())]);
    assert_eq!(parse(src).unwrap().to_source(), src);
    assert_eq!(edit_src(src, r#".A = "2""#), "\u{FEFF}A=2\nB=2\n");
    assert_eq!(edit_src(src, r#".C = "3""#), "\u{FEFF}A=1\nB=2\nC=3\n");
}

#[test]
fn comment_mutation_head_foot_and_inline_refused() {
    // Head above an entry.
    assert_eq!(
        cedit("DATABASE_URL=x\nDEBUG=true\n", ".DEBUG.# = \"verbose\""),
        "DATABASE_URL=x\n# verbose\nDEBUG=true\n"
    );
    // Foot after an entry.
    assert_eq!(
        cedit("A=1\nB=2\n", ".B.#.foot = \"end\""),
        "A=1\nB=2\n# end\n"
    );
    // Replace an existing head; delete it.
    assert_eq!(
        cedit("# old\nK=v\n", ".K.# |= ascii_upcase"),
        "# OLD\nK=v\n"
    );
    assert_eq!(cedit("# drop\nK=v\n", "del(.K.#)"), "K=v\n");
    // Inline is refused; `.env` has no inline comments.
    let mut doc = parse("K=v\n").unwrap();
    let err =
        edikt_core::apply_comment_mutation(&mut doc, &parse_expr(".K.#.inline = \"x\"").unwrap())
            .unwrap_err()
            .to_string();
    assert!(err.contains("no inline comments"), "got: {err}");
}

#[test]
fn roundtrips_byte_identically() {
    for src in [
        SAMPLE,
        "",
        "KEY=value",
        "a:1\nb : 2\n",
        "  spaced = yes  \n# comment\n",
        "! properties comment\nkey.with.dots=1\n",
        "A=1\r\nB=2\r\n", // CRLF terminators preserved
        "A=1\n\nB=2\n",   // a blank line between entries
        "\n\n",           // blank-only document
    ] {
        assert_eq!(parse(src).unwrap().to_source(), src, "round-trip: {src:?}");
    }
}

#[test]
fn projects_flat() {
    assert_eq!(
        q(SAMPLE, ".DATABASE_URL"),
        vec![Value::Str("postgres://localhost/app".into())]
    );
    assert_eq!(q(SAMPLE, ".DEBUG"), vec![Value::Str("true".into())]);
    assert_eq!(q(SAMPLE, ".EMPTY"), vec![Value::Str("".into())]);
    // No inline-comment parsing: the `#` stays in the value.
    assert_eq!(q(SAMPLE, ".WITH_HASH"), vec![Value::Str("a#b".into())]);
}

#[test]
fn set_preserves_separator_style() {
    // `DATABASE_URL=...` has no spaces; `DEBUG = true` does. Keep each.
    assert!(
        edit_src(SAMPLE, r#".DATABASE_URL = "sqlite://x""#).contains("DATABASE_URL=sqlite://x")
    );
    assert!(edit_src(SAMPLE, ".DEBUG = false").contains("DEBUG = false"));
}

#[test]
fn del_removes_line_and_keeps_comment() {
    let out = edit_src(SAMPLE, "del(.DEBUG)");
    assert!(!out.contains("DEBUG"));
    assert!(out.contains("# service env"));
    assert!(out.contains("DATABASE_URL="));
}

#[test]
fn del_entries_in_pipeline() {
    assert_eq!(edit_src("A=1\nB=2\nC=3\n", "del(.A) | del(.B)"), "C=3\n");
}

#[test]
fn update_and_add_assign() {
    assert!(edit_src(SAMPLE, ".DEBUG |= ascii_upcase").contains("DEBUG = TRUE"));
    assert!(edit_src(SAMPLE, r#".DEBUG += "!""#).contains("DEBUG = true!"));
}

#[test]
fn nesting_and_arrays_rejected() {
    let mut doc = parse(SAMPLE).unwrap();
    assert!(apply(&mut doc, &parse_expr(".DEBUG = [1]").unwrap()).is_err());
    assert!(apply(&mut doc, &parse_expr(".a.b = 1").unwrap()).is_err()); // no nesting
}

#[test]
fn malformed_line_errors() {
    assert!(parse("not an entry line\n").is_err());
}

#[test]
fn creates_new_key_by_appending() {
    assert_eq!(edit_src("A=1\n", r#".B = "2""#), "A=1\nB=2\n");
    // appends even when the file lacks a trailing newline
    assert_eq!(edit_src("A=1", r#".B = "2""#), "A=1\nB=2\n");
    // preserves the existing content and comments
    let out = edit_src(SAMPLE, r#".NEW_FLAG = "on""#);
    assert!(out.contains("# service env"));
    assert!(out.ends_with("NEW_FLAG=on\n"));
}

#[test]
fn dotted_properties_keys_are_single_keys() {
    // In `.properties`, `app.name` is one key, addressed with a quoted field.
    let src = "app.name = edikt\nserver.port: 8080\n";
    assert_eq!(q(src, r#"."app.name""#), vec![Value::Str("edikt".into())]);
    assert_eq!(q(src, r#"."server.port""#), vec![Value::Str("8080".into())]);
    assert!(edit_src(src, r#"."server.port" = "9090""#).contains("server.port: 9090"));
}

// --- comment model (extraction + commented emit) -----------------------

#[test]
fn extracts_head_comments_and_trailing_foot() {
    let src = "# service env\nDATABASE_URL=x\n# stop here\n";
    let doc = parse(src).unwrap();
    let c = doc.to_commented().unwrap();
    assert_eq!(c.to_value(), doc.to_value(), "shapes must match");
    let edikt_core::CommentedNode::Object(entries) = &c.node else {
        panic!("expected object");
    };
    assert_eq!(entries[0].1.comments.head, vec!["service env"]);
    assert_eq!(entries[0].1.comments.foot, vec!["stop here"]);
}

#[test]
fn commented_emit_round_trips_and_remaps_inline() {
    let c = parse(SAMPLE).unwrap().to_commented().unwrap();
    let (out, warnings) = emit_commented(&c).unwrap();
    assert!(warnings.is_empty());
    assert!(out.starts_with("# service env\nDATABASE_URL="));
    assert_eq!(parse(&out).unwrap().to_commented().unwrap(), c);

    // An inline comment (from a richer format) moves to its own line, and
    // that remap warns.
    let mut inline = edikt_core::Commented::from_value(&Value::Object(vec![(
        "PORT".into(),
        Value::Str("80".into()),
    )]));
    let edikt_core::CommentedNode::Object(entries) = &mut inline.node else {
        unreachable!();
    };
    entries[0].1.comments.inline = Some("the listen port".into());
    let (out2, warnings2) = emit_commented(&inline).unwrap();
    assert_eq!(out2, "# the listen port\nPORT=80\n");
    assert_eq!(warnings2.len(), 1);
    assert!(
        warnings2[0].contains("inline comments moved"),
        "got: {warnings2:?}"
    );
}

#[test]
fn roundtrips_every_fixture() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/env");
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).expect("fixtures/env directory") {
        let path = entry.unwrap().path();
        // The dialect is chosen by extension here only because these are
        // fixtures; real envspaced files (sshd_config) have no extension,
        // which is exactly why the CLI refuses to auto-detect them.
        let dialect = match path.extension().and_then(|e| e.to_str()) {
            Some("env") | Some("properties") => Dialect::Punctuated,
            Some("envspaced") => Dialect::Spaced,
            _ => continue,
        };
        let src = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            parse_with(&src, dialect).unwrap().to_source(),
            src,
            "round-trip must be byte-identical: {}",
            path.display()
        );
        count += 1;
    }
    assert!(count >= 2, "expected env fixtures, found {count}");
}

// --- edit dispatch: pipe, del arity, non-assignment ---------------------

#[test]
fn piped_mutations_apply_in_order() {
    assert_eq!(
        edit_src("A=1\nB=2\n", r#".A = "x" | .B = "y""#),
        "A=x\nB=y\n"
    );
}

#[test]
fn del_with_wrong_arity_errors() {
    // Function args are `;`-separated, so `del(.A; .B)` is two arguments.
    let mut doc = parse("A=1\nB=2\n").unwrap();
    let err = apply(&mut doc, &parse_expr("del(.A; .B)").unwrap())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("del(...) takes one path argument"),
        "got: {err}"
    );
}

#[test]
fn a_bare_query_is_not_a_mutation() {
    let mut doc = parse("A=1\n").unwrap();
    let err = apply(&mut doc, &parse_expr(".A").unwrap())
        .unwrap_err()
        .to_string();
    assert!(err.contains("expected an assignment"), "got: {err}");
}

// --- comment paths: document-level and nested are refused ---------------

#[test]
fn comment_document_banner_is_a_followup() {
    let mut doc = parse("A=1\n").unwrap();
    let err =
        edikt_core::apply_comment_mutation(&mut doc, &parse_expr(r#".# = "banner""#).unwrap())
            .unwrap_err()
            .to_string();
    assert!(err.contains("document-level"), "got: {err}");
}

#[test]
fn nested_comment_path_is_refused() {
    let mut doc = parse("A=1\n").unwrap();
    let err = edikt_core::apply_comment_mutation(&mut doc, &parse_expr(r#".a.b.# = "x""#).unwrap())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("flat: comment paths are a single"),
        "got: {err}"
    );
}

// --- comment deletion: inline and missing-key no-ops --------------------

#[test]
fn deleting_inline_comment_is_a_noop() {
    // `.env` has no inline comments, so `del(.K.#.inline)` changes nothing.
    assert_eq!(cedit("# h\nK=v\n", "del(.K.#.inline)"), "# h\nK=v\n");
}

#[test]
fn deleting_comment_on_missing_key_is_a_noop() {
    assert_eq!(cedit("# h\nK=v\n", "del(.NOPE.#)"), "# h\nK=v\n");
}

// --- extraction: trailing comments with no entries ----------------------

#[test]
fn all_comments_no_entries_become_document_foot() {
    let doc = parse("# just a note\n# and another\n").unwrap();
    let c = doc.to_commented().unwrap();
    let edikt_core::CommentedNode::Object(entries) = &c.node else {
        panic!("expected object");
    };
    assert!(entries.is_empty(), "no entries in a comment-only file");
    assert_eq!(
        c.comments.foot,
        vec!["just a note".to_string(), "and another".to_string()]
    );
}

// --- emission edge cases ------------------------------------------------

#[test]
fn emit_rejects_a_top_level_scalar() {
    let err = emit(&Value::Str("x".into())).unwrap_err().to_string();
    assert!(err.contains("requires a top-level object"), "got: {err}");
}

#[test]
fn emit_carries_an_entry_foot_comment() {
    let mut c = edikt_core::Commented::from_value(&Value::Object(vec![(
        "A".into(),
        Value::Str("1".into()),
    )]));
    let edikt_core::CommentedNode::Object(entries) = &mut c.node else {
        unreachable!();
    };
    entries[0].1.comments.foot.push("tail".into());
    let (out, warnings) = emit_commented(&c).unwrap();
    assert_eq!(out, "A=1\n# tail\n");
    assert!(warnings.is_empty());
}

#[test]
fn emit_flattens_nesting_and_warns() {
    let (out, warnings) = emit(&Value::Object(vec![(
        "a".into(),
        Value::Object(vec![("b".into(), Value::Str("1".into()))]),
    )]))
    .unwrap();
    assert_eq!(out, "a.b=1\n");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("flattened"), "got: {warnings:?}");
}

#[test]
fn emit_refuses_what_the_file_could_not_read_back() {
    let one = |k: &str, v: &str| Value::Object(vec![(k.to_string(), Value::Str(v.to_string()))]);
    // Each of these used to emit a line that parses as something else.
    for (value, dialect, why) in [
        (one("A", "x\ny"), Dialect::Punctuated, "line break"),
        (one("A", " padded "), Dialect::Punctuated, "whitespace"),
        (one("a=b", "1"), Dialect::Punctuated, "`=` or `:`"),
        (one("#a", "1"), Dialect::Punctuated, "comment"),
        (one("two words", "1"), Dialect::Spaced, "whitespace"),
    ] {
        let c = edikt_core::Commented::from_value(&value);
        let err = emit_commented_with(&c, dialect).unwrap_err().to_string();
        assert!(err.contains(why), "{value:?}: got {err}");
        assert!(err.contains("no quoting"), "{value:?}: got {err}");
    }
    // What reads back as itself still emits.
    assert_eq!(emit(&one("A", "x y")).unwrap().0, "A=x y\n");
}

// --- Document trait surface + syntax accessor ---------------------------

#[test]
fn syntax_accessor_exposes_the_tree() {
    let doc = parse("A=1\n# c\n").unwrap();
    assert_eq!(doc.syntax().text().to_string(), "A=1\n# c\n");
}

#[test]
fn document_trait_features_comments_and_apply() {
    let mut doc = parse("# note\nA=1\n").unwrap();
    assert_eq!(doc.features(), FEATURES);
    assert!(doc.has_comments());
    assert!(!parse("A=1\n").unwrap().has_comments());
    // The trait's `apply` dispatches into the format-preserving edit path.
    Document::apply(&mut doc, &parse_expr(r#".A = "2""#).unwrap()).unwrap();
    assert_eq!(doc.to_source(), "# note\nA=2\n");
}

// --- Language mapping invariant -----------------------------------------

#[test]
fn language_kind_mapping_roundtrips() {
    use rowan::Language;
    for k in [
        Sk::Ws,
        Sk::Newline,
        Sk::Comment,
        Sk::Key,
        Sk::Sep,
        Sk::ValStr,
        Sk::Error,
        Sk::Value,
        Sk::Entry,
        Sk::Root,
    ] {
        let raw = crate::syntax::EnvLang::kind_to_raw(k);
        assert_eq!(crate::syntax::EnvLang::kind_from_raw(raw), k);
    }
}

// ---- the envspaced dialect (edikt-087 BUG-2) ----

const SSHD: &str = "# managed\nPort 22\nPermitRootLogin\tyes\n\nHostKey    /etc/ssh/k\n";

#[test]
fn envspaced_parses_and_round_trips_byte_identically() {
    let doc = parse_spaced(SSHD).unwrap();
    assert_eq!(doc.to_source(), SSHD);
}

#[test]
fn envspaced_reads_values_across_separator_spellings() {
    let doc = parse_spaced(SSHD).unwrap();
    // A single space, a tab, and a run of spaces all end the key.
    assert_eq!(doc.value_at("Port"), Some(Value::Str("22".into())));
    assert_eq!(
        doc.value_at("PermitRootLogin"),
        Some(Value::Str("yes".into()))
    );
    assert_eq!(
        doc.value_at("HostKey"),
        Some(Value::Str("/etc/ssh/k".into()))
    );
}

#[test]
fn envspaced_value_keeps_its_internal_spaces() {
    // Only the FIRST whitespace run is the separator; the rest is value.
    let doc = parse_spaced("Subsystem sftp /usr/lib/sftp-server\n").unwrap();
    assert_eq!(
        doc.value_at("Subsystem"),
        Some(Value::Str("sftp /usr/lib/sftp-server".into()))
    );
}

#[test]
fn envspaced_edit_preserves_the_separator_it_found() {
    // A tab-separated line must stay tab-separated: the separator is not
    // the edit's business, only the value is.
    let mut doc = parse_spaced(SSHD).unwrap();
    doc.set("PermitRootLogin", &Value::Str("no".into()))
        .unwrap();
    assert!(doc.to_source().contains("PermitRootLogin\tno"));
    // and the untouched lines are byte-identical
    assert!(doc.to_source().contains("HostKey    /etc/ssh/k"));
    assert!(doc.to_source().contains("# managed"));
}

#[test]
fn envspaced_append_uses_a_space_not_an_equals() {
    // The dialect is remembered on the document: appending `Key=value` into
    // a spaced file would produce something that no longer parses as one.
    let mut doc = parse_spaced(SSHD).unwrap();
    doc.set("MaxAuthTries", &Value::Str("3".into())).unwrap();
    assert!(doc.to_source().contains("MaxAuthTries 3"));
    assert!(!doc.to_source().contains("MaxAuthTries="));
    // and it parses back as the same document
    assert_eq!(
        parse_spaced(&doc.to_source())
            .unwrap()
            .value_at("MaxAuthTries"),
        Some(Value::Str("3".into()))
    );
}

#[test]
fn the_two_dialects_do_not_read_each_others_files() {
    // `PORT=22` under the spaced dialect is one key with no separator, and
    // `Port 22` under the punctuated one likewise: neither silently
    // half-parses the other, which is why detection is never guessed.
    assert!(parse_spaced("PORT=22\n").is_err());
    assert!(parse("Port 22\n").is_err());
}

#[test]
fn envspaced_deletes_a_whole_line() {
    let mut doc = parse_spaced(SSHD).unwrap();
    doc.delete("Port").unwrap();
    assert!(!doc.to_source().contains("Port"));
    assert!(doc.to_source().contains("HostKey"));
}
