//! End-to-end CLI tests: drive the built `edikt` binary over stdin/files and
//! assert stdout + exit codes (the grep-shaped contract).

use std::io::Write;
use std::process::{Command, Stdio};

/// Run edikt with `args`, feeding `stdin`; return (stdout, stderr, exit code).
fn run(args: &[&str], stdin: &str) -> (String, String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_edikt"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn edikt");
    {
        // The child may exit before reading stdin (e.g. a bad expression or -i
        // error, which are detected first). A broken pipe on the write is then
        // expected; ignore it. Dropping the handle closes stdin (EOF).
        let mut sin = child.stdin.take().unwrap();
        let _ = sin.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

const TSCONFIG: &str = "{ \"compilerOptions\": { \"strict\": true, \"target\": \"ES2020\", \"lib\": [\"ES2020\", \"DOM\"] } }";

#[test]
fn query_scalar_from_stdin() {
    let (out, _e, code) = run(&["-t", "jsonc", ".compilerOptions.strict"], TSCONFIG);
    assert_eq!(out, "true\n");
    assert_eq!(code, 0);
}

#[test]
fn strings_are_raw_by_default() {
    let (out, _e, code) = run(&["-t", "jsonc", ".compilerOptions.target"], TSCONFIG);
    assert_eq!(out, "ES2020\n");
    assert_eq!(code, 0);
}

#[test]
fn json_output_flag() {
    // `--json` is shorthand for `-T json`: structural results emit as pretty
    // JSON (jq-shaped), scalars JSON-encode.
    let (out, _e, code) = run(&["-t", "jsonc", "--json", ".compilerOptions.lib"], TSCONFIG);
    assert_eq!(out, "[\n  \"ES2020\",\n  \"DOM\"\n]\n");
    assert_eq!(code, 0);

    let (s, _e, c2) = run(
        &["-t", "jsonc", "--json", ".compilerOptions.target"],
        TSCONFIG,
    );
    assert_eq!(s, "\"ES2020\"\n");
    assert_eq!(c2, 0);
}

#[test]
fn iterate_yields_one_line_each() {
    let (out, _e, code) = run(&["-t", "jsonc", ".compilerOptions.lib[]"], TSCONFIG);
    assert_eq!(out, "ES2020\nDOM\n");
    assert_eq!(code, 0);
}

#[test]
fn computed_value() {
    let (out, _e, code) = run(
        &["-t", "jsonc", ".compilerOptions.target | ascii_downcase"],
        TSCONFIG,
    );
    assert_eq!(out, "es2020\n");
    assert_eq!(code, 0);
}

#[test]
fn miss_is_a_silent_noop() {
    // sed-shaped: no match, no output, no error; exit 0.
    let (out, err, code) = run(&["-t", "jsonc", ".nope"], TSCONFIG);
    assert_eq!(out, "");
    assert_eq!(err, "");
    assert_eq!(code, 0);
    // jq-shaped presence testing is an explicit opt-in.
    let (_o, _e, c2) = run(&["-t", "jsonc", "--exit-status", ".nope"], TSCONFIG);
    assert_eq!(c2, 1);
    let (_o, _e, c3) = run(
        &["-t", "jsonc", "--exit-status", ".compilerOptions.strict"],
        TSCONFIG,
    );
    assert_eq!(c3, 0);
}

#[test]
fn alternative_operator_defaults_a_miss() {
    let (out, _e, code) = run(&["-t", "jsonc", r#".nope // "fallback""#], TSCONFIG);
    assert_eq!(out, "fallback\n");
    assert_eq!(code, 0);
    // The RHS of an assignment gets the default: set-with-fallback in one pass.
    let (out2, _e, c2) = run(
        &["-t", "jsonc", r#".target = .missing // "ES2022""#],
        "{ \"target\": \"ES5\" }",
    );
    assert_eq!(out2, "{ \"target\": \"ES2022\" }");
    assert_eq!(c2, 0);
}

#[test]
fn multiple_files_edit_like_sed() {
    // Shell globs expand to multiple operands; -i edits each in place.
    let dir = env!("CARGO_TARGET_TMPDIR");
    let a = format!("{dir}/multi-a.json");
    let b = format!("{dir}/multi-b.json");
    std::fs::write(&a, "{\"v\": 1}").unwrap();
    std::fs::write(&b, "{\"v\": 2}").unwrap();
    let (_o, _e, code) = run(&["-i", ".v = 9", &a, &b], "");
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "{\"v\": 9}");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "{\"v\": 9}");
    // Queries over several files concatenate their results, in order.
    let (out, _e, c2) = run(&[".v", &a, &b], "");
    assert_eq!(out, "9\n9\n");
    assert_eq!(c2, 0);
}

#[test]
fn bad_expression_is_exit_2() {
    let (_o, err, code) = run(&["-t", "jsonc", "@bad"], TSCONFIG);
    assert_eq!(code, 2);
    assert!(err.contains("edikt"));
}

#[test]
fn stdin_without_type_errors() {
    let (_o, err, code) = run(&[".a"], "{}");
    assert_eq!(code, 2);
    assert!(err.contains("-t"));
}

#[test]
fn expr_via_dash_e() {
    let (out, _e, code) = run(&["-t", "jsonc", "-e", ".compilerOptions.strict"], TSCONFIG);
    assert_eq!(out, "true\n");
    assert_eq!(code, 0);
}

#[test]
fn in_place_requires_a_mutation() {
    let (_o, err, code) = run(&["-t", "jsonc", "-i", ".a"], "{\"a\":1}");
    assert_eq!(code, 2);
    assert!(err.contains("in-place"));
}

#[test]
fn mutation_writes_whole_doc_to_stdout() {
    let (out, _e, code) = run(&["-t", "jsonc", ".a = 5"], "{ \"a\": 1, \"b\": 2 }");
    assert_eq!(out, "{ \"a\": 5, \"b\": 2 }");
    assert_eq!(code, 0);
}

#[test]
fn update_assign_via_cli() {
    let (out, _e, code) = run(&["-t", "jsonc", ".count |= . + 1"], "{ \"count\": 9 }");
    assert_eq!(out, "{ \"count\": 10 }");
    assert_eq!(code, 0);
}

#[test]
fn in_place_edits_file_and_keeps_comments() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/edit.jsonc");
    std::fs::write(&path, "{\n  // keep me\n  \"strict\": true,\n}\n").unwrap();
    let (out, _e, code) = run(&["-i", ".strict = false", &path], "");
    assert_eq!(out, ""); // -i writes to the file, not stdout
    assert_eq!(code, 0);
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, "{\n  // keep me\n  \"strict\": false,\n}\n");
}

#[test]
fn in_place_with_backup_suffix_keeps_a_backup() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/bak.json");
    std::fs::write(&path, "{\"a\": 1}\n").unwrap();
    // `-i.bak` writes the pre-edit bytes to `path.bak` (sed/perl style).
    let (_o, _e, code) = run(&["-i.bak", ".a = 2", &path], "");
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\": 2}\n");
    assert_eq!(
        std::fs::read_to_string(format!("{path}.bak")).unwrap(),
        "{\"a\": 1}\n"
    );

    // Bare `-i` still backs nothing up.
    let plain = format!("{dir}/bak-plain.json");
    std::fs::write(&plain, "{\"a\": 1}\n").unwrap();
    let (_o2, _e2, code2) = run(&["-i", ".a = 3", &plain], "");
    assert_eq!(code2, 0);
    assert_eq!(std::fs::read_to_string(&plain).unwrap(), "{\"a\": 3}\n");
    assert!(
        !std::fs::exists(format!("{plain}.bak")).unwrap(),
        "bare -i must not leave a backup"
    );

    // The long form (`--in-place=.bak`) works too.
    let long = format!("{dir}/bak-long.json");
    std::fs::write(&long, "{\"a\": 4}\n").unwrap();
    let (_o3, _e3, code3) = run(&["--in-place=.bak", ".a = 5", &long], "");
    assert_eq!(code3, 0);
    assert_eq!(
        std::fs::read_to_string(format!("{long}.bak")).unwrap(),
        "{\"a\": 4}\n"
    );

    // `-i=.bak` (short form with the equals) is not double-mangled: the backup
    // lands at `FILE.bak`, never `FILE=.bak`.
    let eq = format!("{dir}/bak-eq.json");
    std::fs::write(&eq, "{\"a\": 1}\n").unwrap();
    let (_o4, _e4, code4) = run(&["-i=.bak", ".a = 2", &eq], "");
    assert_eq!(code4, 0);
    assert_eq!(
        std::fs::read_to_string(format!("{eq}.bak")).unwrap(),
        "{\"a\": 1}\n"
    );
    assert!(
        !std::fs::exists(format!("{eq}=.bak")).unwrap(),
        "the `-i=SUFFIX` spelling must not create a `FILE=SUFFIX` backup"
    );
}

#[test]
fn dashdash_keeps_dash_operands_untouched() {
    // The `-i.bak` argv rewrite is limited to the option region: a positional
    // operand that merely *looks* like a short flag (here, a file named
    // `-ifoo`) survives after `--`, so it is read as itself, not as
    // `-i=foo`.
    let dir = env!("CARGO_TARGET_TMPDIR");
    let weird = format!("{dir}/-ifoo");
    std::fs::write(&weird, "{\"x\": 1}\n").unwrap();
    let (out, _e, code) = run(&["-t", "json", ".x", "--", &weird], "");
    assert_eq!(
        out, "1\n",
        "the operand after `--` must reach the file reader"
    );
    assert_eq!(code, 0);
}

#[test]
fn del_iterate_empties_containers_across_formats() {
    // `del(.a[])` empties an array (jq), leaving the container.
    let (out, _e, code) = run(&["-t", "jsonc", "del(.a[])"], "{\"a\":[1,2,3]}");
    assert_eq!(out, "{\"a\":[]}");
    assert_eq!(code, 0);

    // YAML block sequence -> inline empty form; file keeps its newline.
    let dir = env!("CARGO_TARGET_TMPDIR");
    let y = format!("{dir}/del.yaml");
    std::fs::write(&y, "a:\n  - 1\n  - 2\n").unwrap();
    let (_o, _e2, code2) = run(&["-i", "del(.a[])", &y], "");
    assert_eq!(code2, 0);
    assert_eq!(std::fs::read_to_string(&y).unwrap(), "a: []\n");

    // TOML array-of-tables: every `[[bin]]` block goes.
    let (out3, _e3, code3) = run(
        &["-t", "toml", "del(.bin[])"],
        "[[bin]]\nname = \"a\"\n[[bin]]\nname = \"b\"\n",
    );
    assert_eq!(out3, "");
    assert_eq!(code3, 0);

    // KDL: repeated node occurrences all removed.
    let (out4, _e4, code4) = run(&["-t", "kdl", "del(.n[])"], "n 1\nn 2\n");
    assert_eq!(out4, "");
    assert_eq!(code4, 0);
}

#[test]
fn in_place_on_stdin_errors() {
    let (_o, err, code) = run(&["-t", "jsonc", "-i", ".a = 1"], "{\"a\":0}");
    assert_eq!(code, 2);
    assert!(err.contains("stdin"));
}

#[test]
fn del_via_cli() {
    let (out, _e, code) = run(&["-t", "jsonc", "del(.a)"], "{ \"a\": 1, \"b\": 2 }");
    assert_eq!(out, "{ \"b\": 2 }");
    assert_eq!(code, 0);
}

#[test]
fn del_then_set_pipeline() {
    let (out, _e, code) = run(
        &["-t", "jsonc", "del(.a) | .b = 9"],
        "{ \"a\": 1, \"b\": 2 }",
    );
    assert_eq!(out, "{ \"b\": 9 }");
    assert_eq!(code, 0);
}

#[test]
fn append_via_cli() {
    let (out, _e, code) = run(
        &["-t", "jsonc", r#".lib += ["X"]"#],
        "{ \"lib\": [\"A\", \"B\"] }",
    );
    assert_eq!(out, "{ \"lib\": [\"A\", \"B\", \"X\"] }");
    assert_eq!(code, 0);
}

#[test]
fn add_assign_number_via_cli() {
    let (out, _e, code) = run(&["-t", "jsonc", ".count += 1"], "{ \"count\": 41 }");
    assert_eq!(out, "{ \"count\": 42 }");
    assert_eq!(code, 0);
}

#[test]
fn ini_query_via_type() {
    let (out, _e, code) = run(&["-t", "ini", ".server.port"], "[server]\nport = 8080\n");
    assert_eq!(out, "8080\n");
    assert_eq!(code, 0);
}

#[test]
fn ini_infers_by_extension() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/q.ini");
    std::fs::write(&path, "[a]\nb = c\n").unwrap();
    let (out, _e, code) = run(&[".a.b", &path], "");
    assert_eq!(out, "c\n");
    assert_eq!(code, 0);
}

#[test]
fn ini_edit_in_place_preserves_layout() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/edit.ini");
    std::fs::write(&path, "; keep me\n[server]\nport = 8080\n").unwrap();
    let (_o, _e, code) = run(&["-i", r#".server.port = "9090""#, &path], "");
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "; keep me\n[server]\nport = 9090\n"
    );
}

#[test]
fn env_query_via_type() {
    let (out, _e, code) = run(&["-t", "env", ".DEBUG"], "# c\nDEBUG=true\n");
    assert_eq!(out, "true\n");
    assert_eq!(code, 0);
}

#[test]
fn env_detects_dotenv_by_name() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/.env");
    std::fs::write(&path, "PORT=8080\n").unwrap();
    let (out, _e, code) = run(&[".PORT", &path], "");
    assert_eq!(out, "8080\n");
    assert_eq!(code, 0);
}

#[test]
fn env_edit_in_place() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/edit.env");
    std::fs::write(&path, "# keep\nDATABASE_URL=old\nDEBUG=true\n").unwrap();
    let (_o, _e, code) = run(&["-i", "del(.DEBUG)", &path], "");
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# keep\nDATABASE_URL=old\n"
    );
}

#[test]
fn creates_new_key_jsonc() {
    let (out, _e, code) = run(
        &["-t", "jsonc", ".compilerOptions.noEmit = true"],
        "{ \"compilerOptions\": { \"strict\": true } }",
    );
    assert_eq!(
        out,
        "{ \"compilerOptions\": { \"strict\": true, \"noEmit\": true } }"
    );
    assert_eq!(code, 0);
}

#[test]
fn creates_new_key_env() {
    // The exact case the demo hit; now works.
    let (out, _e, code) = run(&["-t", "env", r#".K2 = "x""#], "K=v\n");
    assert_eq!(out, "K=v\nK2=x\n");
    assert_eq!(code, 0);
}

#[test]
fn convert_ini_to_json() {
    let (out, _e, code) = run(&["-t", "ini", "-T", "json"], "[a]\nb = c\n");
    assert_eq!(out, "{\n  \"a\": {\n    \"b\": \"c\"\n  }\n}\n");
    assert_eq!(code, 0);
}

#[test]
fn convert_jsonc_to_env_flattens_with_warning() {
    let (out, err, code) = run(&["-t", "jsonc", "-T", "env"], "{ \"A\": { \"B\": 1 } }");
    assert_eq!(out, "A.B=1\n");
    assert!(err.contains("flattened"));
    assert_eq!(code, 0);
}

#[test]
fn json5_nonfinite_degrades_only_to_strict_json_with_a_warning() {
    // Only a JSON5 source can carry `Infinity`/`NaN`. Converting it to strict
    // JSON must warn (it encodes as null, JSON.stringify parity) and be fatal
    // under --strict.
    let (out, err, code) = run(&["-t", "json5", "-T", "json"], "{a: Infinity, b: NaN}");
    assert_eq!(out, "{\n  \"a\": null,\n  \"b\": null\n}\n");
    assert!(err.contains("non-finite"));
    assert_eq!(code, 0);

    let (_o, err2, code2) = run(&["-t", "json5", "--strict", "-T", "json"], "{a: Infinity}");
    assert!(err2.contains("non-finite"));
    assert_eq!(code2, 2);

    // The JSONC/JSON5 family keeps the number: no warning, literal preserved.
    let (out3, err3, code3) = run(&["-t", "json5", "-T", "jsonc"], "{a: Infinity}");
    assert!(out3.contains("Infinity"), "got: {out3}");
    assert!(!err3.contains("non-finite"));
    assert_eq!(code3, 0);

    // A bare query renders the JSON5 spelling raw.
    let (out4, _e, code4) = run(&["-t", "json5", ".a"], "{a: Infinity}");
    assert_eq!(out4, "Infinity\n");
    assert_eq!(code4, 0);
}

#[test]
fn nonfinite_insert_keeps_literal_in_json5_and_errors_in_strict_json() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    // JSON5 document: assigning the literal writes it, no null in sight.
    let j5 = format!("{dir}/nf.json5");
    std::fs::write(&j5, "{a: 1}\n").unwrap();
    let (_o, _e, code) = run(&["-i", ".b = Infinity", &j5], "");
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&j5).unwrap(),
        "{a: 1, \"b\": Infinity}\n"
    );

    // Strict JSON document (not even a comment): the same edit is refused and
    // the file is left untouched.
    let strict = format!("{dir}/nf-strict.json");
    std::fs::write(&strict, "{\"a\": 1}\n").unwrap();
    let (_o2, err2, code2) = run(&["-i", ".a = Infinity", &strict], "");
    assert_eq!(code2, 2);
    assert!(err2.contains("strict JSON"), "got: {err2}");
    assert_eq!(std::fs::read_to_string(&strict).unwrap(), "{\"a\": 1}\n");

    // A `.jsonc` file whose only JSON5-ish feature is a comment is still
    // strict for the purposes of the literal (VS Code JSONC rejects Infinity).
    let commented = format!("{dir}/nf.jsonc");
    std::fs::write(&commented, "// keep\n{\"a\": 1}\n").unwrap();
    let (_o3, _e3, code3) = run(&["-i", ".b = NaN", &commented], "");
    assert_eq!(code3, 2);

    // TOML/YAML have their own native non-finite spellings (inf /.inf).
    let toml = format!("{dir}/nf.toml");
    std::fs::write(&toml, "a = 1\n").unwrap();
    let (_o4, _e4, code4) = run(&["-i", ".b = Infinity", &toml], "");
    assert_eq!(code4, 0);
    assert_eq!(std::fs::read_to_string(&toml).unwrap(), "a = 1\nb = inf\n");
    let yaml = format!("{dir}/nf.yaml");
    std::fs::write(&yaml, "a: 1\n").unwrap();
    let (_o5, _e5, code5) = run(&["-i", ".b = Infinity", &yaml], "");
    assert_eq!(code5, 0);
    assert_eq!(std::fs::read_to_string(&yaml).unwrap(), "a: 1\nb: .inf\n");
}

#[test]
fn regex_builtins_via_cli() {
    // select + test: the filter real configs reach for.
    let y = "services:\n  web:\n    image: nginx:1.25\n  db:\n    image: postgres:16\n";
    let (out, _e, code) = run(
        &[
            "-t",
            "yaml",
            r#".services[] | select(.image | test("^nginx")) | .image"#,
        ],
        y,
    );
    assert_eq!(out, "nginx:1.25\n");
    assert_eq!(code, 0);

    // sub on the right side of a format-preserving mutation.
    let (out2, _e, c2) = run(
        &["-t", "env", r#".VERSION |= sub("^v"; "")"#],
        "# release\nVERSION=v1.2.3\n",
    );
    assert_eq!(out2, "# release\nVERSION=1.2.3\n");
    assert_eq!(c2, 0);

    // split / join round trip in an edit.
    let (out3, _e, c3) = run(
        &["-t", "env", r#".P |= (split(":") + ["/sbin"] | join(":"))"#],
        "P=/usr/bin:/bin\n",
    );
    assert_eq!(out3, "P=/usr/bin:/bin:/sbin\n");
    assert_eq!(c3, 0);

    // capture extracts named groups; a no-match is a grep-shaped miss.
    let (out4, _e, c4) = run(
        &["-t", "json", r#".image | capture("(?<tag>[^:]+$)") | .tag"#],
        "{\"image\": \"nginx:1.25\"}",
    );
    assert_eq!(out4, "1.25\n");
    assert_eq!(c4, 0);
    // A no-match `match` is a miss: silent by default, 1 under --exit-status.
    let (_o, _e, c5) = run(
        &["-t", "json", "--exit-status", r#".a | match("z")"#],
        "{\"a\": \"x\"}",
    );
    assert_eq!(c5, 1);

    // A bad regex is a clean expression error.
    let (_o, err, c6) = run(&["-t", "json", r#".a | test("(")"#], "{\"a\": \"x\"}");
    assert_eq!(c6, 2);
    assert!(err.contains("invalid regex"), "got: {err}");
}

#[test]
fn convert_carries_comments_across_formats() {
    // JSONC -> YAML: head and inline comments arrive in YAML syntax, silently
    // (nothing was lost, so nothing warns).
    let src =
        "{\n  // compiler settings\n  \"opts\": { \"target\": \"ES2020\" /* language level */ }\n}";
    let (out, err, code) = run(&["-t", "jsonc", "-T", "yaml"], src);
    assert_eq!(
        out,
        "# compiler settings\nopts:\n  target: ES2020 # language level\n"
    );
    assert_eq!(err, "", "a fully-carried conversion must not warn");
    assert_eq!(code, 0);

    // YAML -> TOML and YAML -> JSONC carry them too.
    let y = "# stack\nweb:\n  image: nginx # pinned\n";
    let (toml, err2, c2) = run(&["-t", "yaml", "-T", "toml"], y);
    assert!(toml.contains("# stack\n[web]"), "got: {toml}");
    assert!(toml.contains("image = \"nginx\" # pinned"), "got: {toml}");
    assert_eq!(err2, "");
    assert_eq!(c2, 0);

    let (jsonc, _e, c3) = run(&["-t", "yaml", "-T", "jsonc"], y);
    assert!(jsonc.contains("// stack"), "got: {jsonc}");
    assert!(
        jsonc.contains("\"image\": \"nginx\" // pinned"),
        "got: {jsonc}"
    );
    assert_eq!(c3, 0);
}

#[test]
fn convert_comment_remap_warns_and_strict_errors() {
    // env has no inline comments: the comment moves to its own line, warned.
    let (out, err, code) = run(&["-t", "yaml", "-T", "env"], "A: 1 # why\n");
    assert_eq!(out, "# why\nA=1\n");
    assert!(err.contains("inline comments moved"), "got: {err}");
    assert_eq!(code, 0);

    let (_o, err2, c2) = run(&["-t", "yaml", "-T", "env", "--strict"], "A: 1 # why\n");
    assert_eq!(c2, 2);
    assert!(err2.contains("inline comments moved"), "got: {err2}");
}

#[test]
fn synthesized_conversion_still_warns_on_commented_source() {
    // A computed result carries no comments; converting a commented source
    // through one stays an honest, warned drop.
    let y = "# stack\nweb:\n  a: 1\n  b: 2\n";
    let (out, err, code) = run(&["-t", "yaml", "-T", "json", ".web | keys"], y);
    assert_eq!(out, "[\n  \"a\",\n  \"b\"\n]\n");
    assert!(err.contains("comments were dropped"), "got: {err}");
    assert_eq!(code, 0);
}

#[test]
fn convert_warns_on_dropped_comments() {
    let (out, err, code) = run(&["-t", "jsonc", "-T", "json"], "{ /* c */ \"a\": 1 }");
    assert_eq!(out, "{\n  \"a\": 1\n}\n");
    assert!(err.contains("comments were dropped"));
    assert_eq!(code, 0);
}

#[test]
fn convert_strict_errors_on_loss() {
    let (_o, err, code) = run(
        &["-t", "jsonc", "-T", "json", "--strict"],
        "{ /* c */ \"a\": 1 }",
    );
    assert_eq!(code, 2);
    assert!(err.contains("comments"));
}

#[test]
fn convert_subtree_via_expr() {
    let (out, _e, code) = run(
        &["-t", "ini", "-T", "env", "-e", ".server"],
        "[server]\nhost=x\nport=8080\n",
    );
    assert_eq!(out, "host=x\nport=8080\n");
    assert_eq!(code, 0);
}

#[test]
fn ini_creates_new_section() {
    let (out, _e, code) = run(&["-t", "ini", r#".db.url = "pg""#], "[server]\nhost = x\n");
    assert_eq!(out, "[server]\nhost = x\n\n[db]\nurl = pg\n");
    assert_eq!(code, 0);
}

#[test]
fn toml_query_and_edit_keeps_comment() {
    let (out, _e, code) = run(
        &["-t", "toml", ".package.version"],
        "[package]\nversion = \"1.0.0\"\n",
    );
    assert_eq!(out, "1.0.0\n");
    assert_eq!(code, 0);

    let (edited, _e, code2) = run(
        &["-t", "toml", r#".version = "2.0.0""#],
        "version = \"1.0.0\"  # semver\n",
    );
    assert_eq!(edited, "version = \"2.0.0\"  # semver\n");
    assert_eq!(code2, 0);
}

#[test]
fn convert_toml_to_json_and_back() {
    let (out, _e, code) = run(&["-t", "toml", "-T", "json"], "[a]\nb = 1\n");
    assert_eq!(out, "{\n  \"a\": {\n    \"b\": 1\n  }\n}\n");
    assert_eq!(code, 0);

    let (toml, _e, code2) = run(
        &["-t", "jsonc", "-T", "toml"],
        "{ \"package\": { \"name\": \"x\", \"version\": \"1\" } }",
    );
    assert!(toml.contains("[package]"), "got: {toml}");
    assert!(toml.contains("name = \"x\""));
    assert_eq!(code2, 0);
}

#[test]
fn yaml_query_and_convert() {
    let (out, _e, code) = run(&["-t", "yaml", ".web.replicas"], "web:\n  replicas: 3\n");
    assert_eq!(out, "3\n");
    assert_eq!(code, 0);

    let (json, _e, c2) = run(&["-t", "yaml", "-T", "json"], "a:\n  b: 1\n");
    assert_eq!(json, "{\n  \"a\": {\n    \"b\": 1\n  }\n}\n");
    assert_eq!(c2, 0);

    let (yaml, _e, c3) = run(&["-t", "jsonc", "-T", "yaml"], "{ \"a\": { \"b\": 1 } }");
    assert_eq!(yaml, "a:\n  b: 1\n");
    assert_eq!(c3, 0);
}

#[test]
fn yaml_edit_is_lossless() {
    // A one-scalar edit preserves the comment and every untouched byte.
    let (out, _e, code) = run(
        &["-t", "yaml", ".replicas = 5"],
        "# stack\nreplicas: 3   # count\ndebug: false\n",
    );
    assert_eq!(out, "# stack\nreplicas: 5   # count\ndebug: false\n");
    assert_eq!(code, 0);

    // del + append through the CLI.
    let (out2, _e, c2) = run(&["-t", "yaml", "del(.debug)"], "a: 1\ndebug: true\n");
    assert_eq!(out2, "a: 1\n");
    assert_eq!(c2, 0);
}

#[test]
fn comments_stream_query_and_bulk_edit() {
    // comment -> key: which keys carry a TODO?
    let y =
        "web:\n  image: nginx  # TODO pin\n  port: 80  # ok\ndb:\n  image: pg  # TODO upgrade\n";
    let (out, _e, code) = run(
        &[
            "-t",
            "yaml",
            r#"comments | select(.text | test("TODO")) | .path"#,
        ],
        y,
    );
    assert_eq!(out, ".web.image\n.db.image\n");
    assert_eq!(code, 0);

    // Bulk edit every comment's text (TODO -> DONE), format-preserving.
    let (edited, _e, c2) = run(
        &["-t", "yaml", r#"comments |= gsub("TODO"; "DONE")"#],
        "a: 1  # TODO a\nb: 2  # TODO b\n",
    );
    assert_eq!(edited, "a: 1  # DONE a\nb: 2  # DONE b\n");
    assert_eq!(c2, 0);

    // Bulk delete removes all comments (JSONC, through the rowan re-parse path).
    let (stripped, _e, c3) = run(
        &["-t", "jsonc", "del(comments)"],
        "{\n  // drop\n  \"a\": 1,\n  \"b\": 2 // and this\n}\n",
    );
    assert_eq!(stripped, "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
    assert_eq!(c3, 0);

    // `comments` as JSON does not spuriously warn "comments were dropped".
    let (_o, err, c4) = run(&["-t", "yaml", "--json", "comments"], "a: 1  # note\n");
    assert!(!err.contains("dropped"), "stray warning: {err}");
    assert_eq!(c4, 0);

    // Bulk append to every comment.
    let (app, _e, c5) = run(
        &["-t", "toml", r#"comments += " (done)""#],
        "# a\nk = 1  # b\n",
    );
    assert!(
        app.contains("# a (done)") && app.contains("# b (done)"),
        "got: {app}"
    );
    assert_eq!(c5, 0);
}

#[test]
fn comment_mutation_add_assign_pipe_and_errors() {
    // += appends to a single comment; piped comment edits chain.
    let (out, _e, code) = run(&["-t", "toml", r#".k.# = "note" | .k.# += "!""#], "k = 1\n");
    assert_eq!(out, "# note!\nk = 1\n");
    assert_eq!(code, 0);

    // `del` with two (`;`-separated) arguments is an arity error, not a panic.
    let (_o, err, c2) = run(&["-t", "toml", "del(.k.#; .j.#)"], "k = 1\n");
    assert_eq!(c2, 2);
    assert!(err.contains("one"), "got: {err}");
}

#[test]
fn comment_query_reads_by_kind() {
    // Head comment via `#` (JSONC).
    let (out, _e, code) = run(
        &["-t", "jsonc", ".strict.#"],
        "{\n  // the strict flag\n  \"strict\": true\n}",
    );
    assert_eq!(out, "the strict flag\n");
    assert_eq!(code, 0);

    // Inline via `#.inline` (YAML), and transform through a pipe.
    let (up, _e, c2) = run(
        &["-t", "yaml", ".web.image.#.inline | ascii_upcase"],
        "web:\n  image: nginx # pinned\n",
    );
    assert_eq!(up, "PINNED\n");
    assert_eq!(c2, 0);

    // Iterate comments in one path: `.services[].#.inline`.
    let (each, _e, c3) = run(
        &["-t", "yaml", ".services[].#.inline"],
        "services:\n  web: 1 # frontend\n  db: 2 # store\n",
    );
    assert_eq!(each, "frontend\nstore\n");
    assert_eq!(c3, 0);

    // A missing comment is a sed-shaped miss (silent, exit 0); `//` defaults it.
    let (miss, _e, c4) = run(&["-t", "jsonc", ".a.#"], "{ \"a\": 1 }");
    assert_eq!(miss, "");
    assert_eq!(c4, 0);
    let (dflt, _e, c5) = run(&["-t", "jsonc", r#".a.# // "none""#], "{ \"a\": 1 }");
    assert_eq!(dflt, "none\n");
    assert_eq!(c5, 0);
}

#[test]
fn comment_mutation_on_decor_formats() {
    // TOML: set a head comment in place, everything else byte-identical.
    let (out, _e, code) = run(
        &["-t", "toml", r#".b.z.# = "annotated""#],
        "# banner\n[a]\nx = 1  # inline x\n[b]\nz = 3\n",
    );
    assert_eq!(
        out,
        "# banner\n[a]\nx = 1  # inline x\n[b]\n# annotated\nz = 3\n"
    );
    assert_eq!(code, 0);

    // KDL: inline comment, then read it back through a pipe.
    let (out2, _e, c2) = run(
        &["-t", "kdl", r#".server.port.#.inline = "listen""#],
        "server {\n    port 8080\n}\n",
    );
    assert_eq!(out2, "server {\n    port 8080 // listen\n}\n");
    assert_eq!(c2, 0);

    // JSONC (rowan) head comment in place; surrounding bytes untouched.
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/comment-edit.jsonc");
    std::fs::write(&path, "{\n  // keep\n  \"a\": 1,\n  \"b\": 2\n}\n").unwrap();
    let (_o, _e, c3) = run(&["-i", r#".b.# = "note""#, &path], "");
    assert_eq!(c3, 0);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\n  // keep\n  \"a\": 1,\n  // note\n  \"b\": 2\n}\n"
    );

    // A compact object defers to the layout-reflow follow-up with a clear error.
    let (_o, err, c4) = run(&["-t", "jsonc", r#".b.# = "x""#], "{ \"a\": 1, \"b\": 2 }");
    assert_eq!(c4, 2);
    assert!(err.contains("layout expansion"), "got: {err}");
}

#[test]
fn kdl_query_edit_and_convert() {
    // Query a nested child node's argument.
    let src = "server {\n    port 8080\n    host \"0.0.0.0\"\n}\n";
    let (out, _e, code) = run(&["-t", "kdl", ".server.port"], src);
    assert_eq!(out, "8080\n");
    assert_eq!(code, 0);

    // Lossless edit: the leading + inline comments and layout survive.
    let (edited, _e, c2) = run(
        &["-t", "kdl", ".server.port = 9090"],
        "// prod\nserver {\n    port 8080 // listen\n}\n",
    );
    assert_eq!(edited, "// prod\nserver {\n    port 9090 // listen\n}\n");
    assert_eq!(c2, 0);

    // KDL -> JSON and JSON -> KDL.
    let (json, _e, c3) = run(&["-t", "kdl", "-T", "json"], src);
    assert_eq!(
        json,
        "{\n  \"server\": {\n    \"port\": 8080,\n    \"host\": \"0.0.0.0\"\n  }\n}\n"
    );
    assert_eq!(c3, 0);

    let (kdl, _e, c4) = run(
        &["-t", "json", "-T", "kdl"],
        "{ \"pkg\": { \"name\": \"edikt\" } }",
    );
    assert!(kdl.contains("pkg {"), "got: {kdl}");
    assert!(kdl.contains("name edikt"), "got: {kdl}");
    assert_eq!(c4, 0);

    // Comment carries KDL -> YAML (`//` becomes `#`).
    let (yaml, err, c5) = run(
        &["-t", "kdl", "-T", "yaml"],
        "// the server\nserver {\n    port 8080\n}\n",
    );
    assert_eq!(yaml, "# the server\nserver:\n  port: 8080\n");
    assert_eq!(err, "");
    assert_eq!(c5, 0);
}

#[test]
fn object_literal_and_bracket_key() {
    let (out, _e, code) = run(&["-t", "jsonc", ".config = {}"], "{ \"config\": 1 }");
    assert_eq!(out, "{ \"config\": {} }");
    assert_eq!(code, 0);

    let (out2, _e, code2) = run(
        &["-t", "properties", r#"."app.name""#],
        "app.name = edikt\n",
    );
    assert_eq!(out2, "edikt\n");
    assert_eq!(code2, 0);
}

#[test]
fn object_construct_pluck_shorthand() {
    // jq's `{a, b}` sugar, the usual way to pluck a few keys out of a config.
    let src = "{ \"MemoryMiB\": 32768, \"UseGrpcfuse\": true, \"SwapMiB\": 1024 }";
    let (out, err, code) = run(&["-t", "jsonc", "{MemoryMiB, UseGrpcfuse}"], src);
    assert_eq!(
        out,
        "{\n  \"MemoryMiB\": 32768,\n  \"UseGrpcfuse\": true\n}\n"
    );
    assert_eq!(err, "");
    assert_eq!(code, 0);

    // Mixes with explicit pairs; a key the document lacks comes back null.
    let (out2, _e, code2) = run(
        &["-t", "jsonc", "{MemoryMiB, swap: .SwapMiB, Missing}"],
        src,
    );
    assert_eq!(
        out2,
        "{\n  \"MemoryMiB\": 32768,\n  \"swap\": 1024,\n  \"Missing\": null\n}\n"
    );
    assert_eq!(code2, 0);
}

#[test]
fn reads_a_file_and_infers_by_extension() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/sample.jsonc");
    std::fs::write(&path, "// comment\n{ \"a\": { \"b\": 42 } }\n").unwrap();
    let (out, _e, code) = run(&[".a.b", &path], "");
    assert_eq!(out, "42\n");
    assert_eq!(code, 0);
}

#[test]
fn unknown_extension_errors() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/sample.weird");
    std::fs::write(&path, "{}").unwrap();
    let (_o, err, code) = run(&[".a", &path], "");
    assert_eq!(code, 2);
    assert!(err.contains("-t"));
    // The error lists the formats to choose from.
    assert!(err.contains("yaml") && err.contains("toml"), "got: {err}");
}

#[test]
fn structural_query_returns_source_slice() {
    // A pure-path structural query stays in-format: exact source bytes,
    // comments included, not re-serialized JSON.
    let src = "{\n  \"lib\": [\"ES2020\", \"DOM\"], // pinned\n  \"opts\": { \"strict\": true }\n}";
    let (out, _e, code) = run(&["-t", "jsonc", ".lib"], src);
    assert_eq!(out, "[\"ES2020\", \"DOM\"]\n");
    assert_eq!(code, 0);

    // YAML block collections come back dedented, comments intact.
    let y = "web:\n  image: nginx   # pinned\n  ports:\n    - 80\n";
    let (yo, _e, yc) = run(&["-t", "yaml", ".web"], y);
    assert_eq!(yo, "image: nginx   # pinned\nports:\n  - 80\n");
    assert_eq!(yc, 0);
}

#[test]
fn synthesized_query_stays_in_format() {
    // A computed result has no source slice; it renders via the input format's
    // emitter: YAML in, YAML out.
    let y = "web:\n  image: nginx\n  ports:\n    - 80\n";
    let (out, _e, code) = run(&["-t", "yaml", ".web | keys"], y);
    assert_eq!(out, "- image\n- ports\n");
    assert_eq!(code, 0);
}

#[test]
fn infeasible_output_names_candidates() {
    // An array result cannot be a top-level env document; the error suggests
    // formats that can hold a top-level array (json/jsonc/yaml) and NOT the
    // ones that can't (toml/kdl require a table at the root), and never the
    // target that just failed (edikt-049 review fix F).
    let (_o, err, code) = run(&["-t", "yaml", "-T", "env", ".xs"], "xs:\n  - 1\n  - 2\n");
    assert_eq!(code, 2);
    assert!(err.contains("cannot represent"), "got: {err}");
    assert!(err.contains("json") && err.contains("yaml"), "got: {err}");
    assert!(
        !err.contains("toml") && !err.contains("kdl"),
        "top-level array: {err}"
    );
    // The target format that failed is never suggested back.
    let (_o, err2, _c) = run(&["-t", "toml", "-T", "toml", ".xs"], "xs = [1, 2]\n");
    assert!(
        !err2.contains("-T toml"),
        "must not suggest the failed format: {err2}"
    );
}

#[test]
fn script_directives_bake_formats() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let script = format!("{dir}/to_json.edk");
    std::fs::write(
        &script,
        "#!/usr/bin/env -S edikt -f\n# a comment\ntoFormat: json\ntype: yaml\n.web\n",
    )
    .unwrap();
    // The script bakes input+output formats; no -t/-T needed.
    let (out, _e, code) = run(&["-f", &script], "web:\n  a: 1\n");
    assert_eq!(out, "{\n  \"a\": 1\n}\n");
    assert_eq!(code, 0);

    // CLI flags override script directives.
    let (out2, _e2, c2) = run(&["-f", &script, "-T", "yaml"], "web:\n  a: 1\n");
    assert_eq!(out2, "a: 1\n");
    assert_eq!(c2, 0);
}

#[test]
fn to_with_expression_and_file() {
    // `-T json EXPR FILE`: the first operand is the expression (not a file).
    let dir = env!("CARGO_TARGET_TMPDIR");
    let path = format!("{dir}/tq.yaml");
    std::fs::write(&path, "a:\n  b: 2\n").unwrap();
    let (out, _e, code) = run(&["-T", "json", ".a", &path], "");
    assert_eq!(out, "{\n  \"b\": 2\n}\n");
    assert_eq!(code, 0);

    // `-T json FILE` still converts the whole document.
    let (out2, _e2, c2) = run(&["-T", "json", &path], "");
    assert_eq!(out2, "{\n  \"a\": {\n    \"b\": 2\n  }\n}\n");
    assert_eq!(c2, 0);
}

#[test]
fn output_file_infers_format_from_extension() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let out = format!("{dir}/o1.json");
    // YAML in, -o file.json -> JSON written to the file, nothing on stdout.
    let (stdout, _e, code) = run(&["-t", "yaml", "-o", &out, ".a"], "a:\n  b: 1\n");
    assert_eq!(stdout, "");
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "{\n  \"b\": 1\n}\n");

    // Explicit -T beats the extension.
    let out2 = format!("{dir}/o2.json");
    let (_s, _e, c2) = run(
        &["-t", "yaml", "-T", "yaml", "-o", &out2, ".a"],
        "a:\n  b: 1\n",
    );
    assert_eq!(c2, 0);
    assert_eq!(std::fs::read_to_string(&out2).unwrap(), "b: 1\n");
}

#[test]
fn output_file_takes_mutation_result() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let out = format!("{dir}/edited.yaml");
    // A mutation with -o writes the edited document to FILE (extension is a
    // sink here, not a conversion).
    let (stdout, _e, code) = run(&["-t", "yaml", "-o", &out, ".a = 2"], "# keep\na: 1\n");
    assert_eq!(stdout, "");
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "# keep\na: 2\n");
}

#[test]
fn output_file_untouched_on_miss() {
    let dir = env!("CARGO_TARGET_TMPDIR");
    let out = format!("{dir}/miss.json");
    let _ = std::fs::remove_file(&out);
    let (_s, _e, code) = run(&["-t", "yaml", "-o", &out, ".nope"], "a: 1\n");
    assert_eq!(code, 0); // a miss is a silent no-op...
    assert!(
        !std::path::Path::new(&out).exists(),
        "...and writes no file"
    );
}

#[test]
fn identity_expression_is_never_a_directory() {
    // `.` names a directory that always exists, but as an operand it is the
    // identity *expression*; with -T it must convert stdin, not try to read
    // the current directory.
    let (out, _e, code) = run(&["-t", "jsonc", "-T", "json", "."], "{ \"a\": 1 }");
    assert_eq!(out, "{\n  \"a\": 1\n}\n");
    assert_eq!(code, 0);
}

#[test]
fn packager_flags_emit_docs() {
    // The binary is its own doc generator: completions and the man page come
    // from hidden flags, so release archives and package builds need no
    // extra tooling.
    let (out, _e, code) = run(&["--completions", "zsh"], "");
    assert!(out.starts_with("#compdef edikt"), "got: {out:.40}");
    assert_eq!(code, 0);
    let (man, _e, c2) = run(&["--manpage"], "");
    assert!(man.contains(".TH edikt 1"), "got: {man:.80}");
    assert_eq!(c2, 0);
}

#[test]
fn helpful_error_messages() {
    // Unknown format names the valid choices.
    let (_o, err, code) = run(&["-t", "bogus", "."], "a: 1\n");
    assert_eq!(code, 2);
    assert!(err.contains("unknown format `bogus`"), "got: {err}");
    assert!(err.contains("jsonc") && err.contains("yaml"), "got: {err}");

    // A failed edit path is named in the error. (Deleting a *missing* key is a
    // no-op, jq-style, so use a create-through-scalar, which genuinely fails.)
    let (_o, err2, c2) = run(&["-t", "yaml", ".a.b = 1"], "a: 1\n");
    assert_eq!(c2, 2);
    assert!(err2.contains(".a.b"), "got: {err2}");

    // A bad expression chains its context.
    let (_o, err3, c3) = run(&["-t", "yaml", ".a |"], "a: 1\n");
    assert_eq!(c3, 2);
    assert!(err3.contains("bad expression"), "got: {err3}");
}

#[test]
fn toml_assignment_ergonomics() {
    let cargo = "[package]\nname = \"demo\"\n";

    // Assigning under a missing table creates it (jq parity).
    let (out, _e, code) = run(&["-t", "toml", r#".features.default = ["std"]"#], cargo);
    assert_eq!(code, 0);
    assert!(out.contains("[features]"), "got: {out}");
    assert!(out.contains("default = [\"std\"]"), "got: {out}");

    // TOML inline-table spelling works as an object value.
    let (out2, _e, c2) = run(
        &[
            "-t",
            "toml",
            r#".dependencies.bar = {version = "1", optional = true}"#,
        ],
        cargo,
    );
    assert_eq!(c2, 0);
    assert!(
        out2.contains("bar = { version = \"1\", optional = true }"),
        "got: {out2}"
    );

    // A hyphenated bare key needs no quoting on an assignment LHS: a target must
    // be a path, so `-` cannot be subtraction there.
    let (out3, err3, c3) = run(&["-t", "toml", r#".package.rust-version = "1.85""#], cargo);
    assert_eq!(c3, 0, "stderr: {err3}");
    assert!(out3.contains("rust-version = \"1.85\""), "got: {out3}");

    // Including when a path follows it, which used to die at the next `.`.
    let (out4, err4, c4) = run(
        &["-t", "toml", r#".dev-dependencies.serde_json = "9.9""#],
        cargo,
    );
    assert_eq!(c4, 0, "stderr: {err4}");
    assert!(out4.contains("serde_json = \"9.9\""), "got: {out4}");

    // A query is still genuinely ambiguous (`.total-length` is a real
    // subtraction), so it explains rather than guesses.
    let (_o, err5, c5) = run(&["-t", "toml", ".dev-dependencies.serde_json"], cargo);
    assert_eq!(c5, 2);
    assert!(err5.contains("contains `-`"), "got: {err5}");
    assert!(err5.contains(r#"."dev-dependencies""#), "got: {err5}");
}

#[test]
fn frontmatter_query_and_edit() {
    let md = "---\ntitle: Hello\nstatus: Drafted\n---\n# Body\n\nProse.\n";

    // Query a frontmatter key (forced type over stdin).
    let (out, _e, code) = run(&["-t", "markdown", ".status"], md);
    assert_eq!(code, 0);
    assert_eq!(out, "Drafted\n");

    // Convert the block to JSON (extract-and-convert).
    let (json, _e, c2) = run(&["-t", "markdown", "-T", "json", "."], md);
    assert_eq!(c2, 0);
    assert!(json.contains("\"title\": \"Hello\""), "got: {json}");

    // Converting *to* frontmatter is rejected early with ONE clean message -
    // no double-wrap, no jargon, no contradictory "try" clauses (review fix H).
    let (_o, err, c3) = run(&["-t", "yaml", "-T", "markdown", "."], "a: 1\n");
    assert_eq!(c3, 2);
    assert!(err.contains("read-only view"), "got: {err}");
    assert!(
        !err.contains("cannot represent"),
        "must not double-wrap: {err}"
    );
    assert!(!err.contains("lens"), "no internal jargon: {err}");

    // No frontmatter present is a clear error.
    let (_o, err2, c4) = run(&["-t", "markdown", ".x"], "# just a heading\n");
    assert_eq!(c4, 2);
    assert!(err2.contains("no frontmatter block"), "got: {err2}");
}

#[test]
fn frontmatter_commented_pep723() {
    let py = "#!/usr/bin/env python\n# /// script\n# requires-python = \">=3.11\"\n# dependencies = [\"rich\"]\n# ///\nimport rich\n";

    // Scalar query of a commented (PEP 723) block.
    let (out, _e, c) = run(&["-t", "frontmatter", r#".["requires-python"]"#], py);
    assert_eq!(c, 0);
    assert_eq!(out, ">=3.11\n");

    // Whole block renders in its native TOML (a query over a lens defaults to
    // the block's own format, not "frontmatter").
    let (blk, _e, c2) = run(&["-t", "frontmatter", "."], py);
    assert_eq!(c2, 0);
    assert!(blk.contains("requires-python = \">=3.11\""), "got: {blk}");

    // A top-level array has no TOML representation, so an un-directed query
    // falls back to JSON (edikt-051) rather than erroring.
    let (arr, _e, c3) = run(&["-t", "frontmatter", ".dependencies"], py);
    assert_eq!(c3, 0);
    assert!(arr.contains("rich"), "got: {arr}");
    // An explicit -T json is of course still fine.
    let (arr2, _e, c4) = run(&["-t", "frontmatter", "-T", "json", ".dependencies"], py);
    assert_eq!(c4, 0);
    assert!(arr2.contains("rich"), "got: {arr2}");
}

#[test]
fn query_falls_back_to_json_when_source_cant_hold_it() {
    // A TOML top-level array has no TOML representation. An un-directed *query*
    // shows the value as JSON instead of erroring (edikt-051).
    let (out, _e, code) = run(&["-t", "toml", ".xs"], "xs = [1, 2, 3]\n");
    assert_eq!(code, 0);
    assert!(
        out.contains('[') && out.contains('3'),
        "json array out: {out}"
    );

    // A representable result still renders in the source format (unchanged): an
    // object queried from TOML comes back as TOML.
    let (obj, _e, c2) = run(&["-t", "toml", ".srv"], "[srv]\nport = 8080\n");
    assert_eq!(c2, 0);
    assert!(obj.contains("port = 8080"), "toml object out: {obj}");

    // An *explicit* -T that can't hold the value still errors; the fallback is
    // only for the defaulted output of a read.
    let (_o, err, c3) = run(&["-t", "yaml", "-T", "toml", ".xs"], "xs:\n  - 1\n");
    assert_eq!(c3, 2);
    assert!(err.contains("cannot represent"), "got: {err}");
}

#[test]
fn yaml_multidoc_query_and_edit() {
    let stream =
        "---\nkind: Deployment\nspec:\n  replicas: 2\n---\nkind: Service\nspec:\n  port: 80\n";

    // A query yields one result per document.
    let (out, _e, c) = run(&["-t", "yaml", ".kind"], stream);
    assert_eq!(c, 0);
    assert_eq!(out, "Deployment\nService\n");

    // A bare edit maps over every document (both have `.spec`).
    let (edited, _e, c2) = run(&["-t", "yaml", ".spec.tier = \"prod\""], stream);
    assert_eq!(c2, 0);
    assert_eq!(edited.matches("tier: prod").count(), 2, "got: {edited}");

    // `select(pred)` targets by content: only the Service is edited.
    let (sel, _e, c3) = run(
        &[
            "-t",
            "yaml",
            "select(.kind == \"Service\") | .spec.port = 443",
        ],
        stream,
    );
    assert_eq!(c3, 0);
    assert!(sel.contains("port: 443"), "got: {sel}");
    assert!(sel.contains("replicas: 2"), "deployment untouched: {sel}");

    // `^dN` positional select for query and edit.
    let (k1, _e, c4) = run(&["-t", "yaml", "^d1.kind"], stream);
    assert_eq!(c4, 0);
    assert_eq!(k1, "Service\n");
    let (e1, _e, c5) = run(&["-t", "yaml", "^d0 | .spec.replicas = 9"], stream);
    assert_eq!(c5, 0);
    assert!(
        e1.contains("replicas: 9") && e1.contains("port: 80"),
        "got: {e1}"
    );
}

// --- Regression tests for the edikt-049 pre-release review findings. Each
//     asserts the corrected behavior of a defect the merciless-critic/UX review
//     found; every one would have failed before its fix. ------------------------

#[test]
fn review_fix_docselect_edit_works_on_any_format() {
    // Finding B: `^d0` edit was broken on non-YAML formats with a misleading
    // "expected an assignment" error. `^d0` names the only document.
    let (out, _e, c) = run(&["-t", "jsonc", "^d0 | .a = 2"], "{\"a\":1}");
    assert_eq!(c, 0);
    assert_eq!(out, "{\"a\":2}");
    // Out of range on a single-document input errors clearly.
    let (_o, err, c2) = run(&["-t", "jsonc", "^d1 | .a = 2"], "{\"a\":1}");
    assert_eq!(c2, 2);
    assert!(err.contains("out of range"), "got: {err}");
    // PEP 723 (TOML-backed frontmatter) edit via ^d0 works too.
    let py = "# /// script\n# a = 1\n# ///\nx = 2\n";
    let (o3, _e, c3) = run(&["-t", "frontmatter", "^d0 | .a = 9"], py);
    assert_eq!(c3, 0);
    assert!(o3.contains("# a = 9"), "got: {o3}");
}

#[test]
fn review_fix_docselect_query_out_of_range_errors() {
    // Finding E: an out-of-range `^dN` read was a silent empty; now it errors
    // like the edit does (an explicit selector is strict).
    let stream = "---\nk: 1\n---\nk: 2\n";
    let (_o, err, c) = run(&["-t", "yaml", "^d5.k"], stream);
    assert_eq!(c, 2);
    assert!(err.contains("out of range"), "got: {err}");
}

#[test]
fn review_fix_query_fallback_is_scoped_to_top_level_arrays() {
    // Finding D: the JSON fallback swallowed genuine value-fidelity errors. A
    // top-level array still falls back to JSON...
    let (out, _e, c) = run(&["-t", "toml", ".xs"], "xs = [1, 2, 3]\n");
    assert_eq!(c, 0);
    assert!(
        out.contains('[') && out.contains('3'),
        "array as json: {out}"
    );
    // ...but an object that can't be represented for a *value* reason (a null in
    // TOML) is a real error, not silently rendered as JSON.
    let (_o, err, c2) = run(&["-t", "toml", "{x: .a, y: null}"], "a = 1\n");
    assert_eq!(c2, 2);
    assert!(
        err.contains("null"),
        "null must error, not fall back: {err}"
    );
}

#[test]
fn review_fix_multidoc_comment_ops_touch_every_document() {
    // Finding A: comment ops silently scoped to document 0. Now they map over
    // every document.
    let s = "---\nkind: A  # one\n---\nkind: B  # two\n";
    // Query yields a comment per document.
    let (q, _e, c) = run(&["-t", "yaml", ".kind.#.inline"], s);
    assert_eq!(c, 0);
    assert_eq!(q, "one\ntwo\n");
    // Set applies to both.
    let (set, _e, c2) = run(&["-t", "yaml", ".kind.#.inline = \"NOTE\""], s);
    assert_eq!(c2, 0);
    assert_eq!(set.matches("# NOTE").count(), 2, "got: {set}");
    // del(comments) clears both.
    let (del, _e, c3) = run(&["-t", "yaml", "del(comments)"], s);
    assert_eq!(c3, 0);
    assert!(!del.contains('#'), "all comments gone: {del}");
}

#[test]
fn review_fix_docselect_and_select_with_comments_error_clearly() {
    // Finding C: `^dN`/`select` combined with a comment edit gave a misleading
    // "use a comment path" error even when the user used one. Now it names the
    // real limitation.
    let s = "---\nkind: A  # one\n---\nkind: B  # two\n";
    let (_o, e1, c1) = run(&["-t", "yaml", "^d1 | .kind.# = \"x\""], s);
    assert_eq!(c1, 2);
    assert!(e1.contains("^dN") && e1.contains("comment"), "got: {e1}");
    let (_o, e2, c2) = run(
        &["-t", "yaml", "select(.kind == \"A\") | .kind.# = \"x\""],
        s,
    );
    assert_eq!(c2, 2);
    assert!(e2.contains("select") && e2.contains("comment"), "got: {e2}");
}

#[test]
fn review_fix_select_skips_and_warns_on_predicate_error() {
    // Finding G: `select` aborted the whole edit if the predicate type-errored
    // on one document. Now it skips that document (with a warning) and edits the
    // ones that match.
    let s = "---\nkind: Deployment\n---\njustascalar\n";
    let (out, err, c) = run(
        &["-t", "yaml", "select(.kind == \"Deployment\") | .x = 1"],
        s,
    );
    assert_eq!(c, 0);
    assert!(out.contains("x: 1"), "matching doc edited: {out}");
    assert!(out.contains("justascalar"), "scalar doc preserved: {out}");
    assert!(err.contains("skipped document"), "warns on skip: {err}");
}

#[test]
fn review_fix_bulk_comment_transform_over_multidoc_is_per_document() {
    // The carve-out resolved: bulk `comments |= f` over a multi-document stream
    // transforms each comment from ITS OWN text (per-document scoping), not one
    // document's text applied to another.
    let s = "---\nkind: Deployment  # the deployment\n---\nkind: Service  # the service\n";
    let (out, _e, c) = run(&["-t", "yaml", "comments |= ascii_upcase"], s);
    assert_eq!(c, 0);
    assert!(out.contains("# THE DEPLOYMENT"), "got: {out}");
    assert!(out.contains("# THE SERVICE"), "got: {out}");
    // Append also scopes per document.
    let s2 = "---\na: 1  # one\n---\nb: 2  # two\n";
    let (out2, _e, c2) = run(&["-t", "yaml", r#"comments += " !""#], s2);
    assert_eq!(c2, 0);
    assert!(
        out2.contains("# one !") && out2.contains("# two !"),
        "got: {out2}"
    );
}

// ---- hyphenated-key diagnostics (jhheider/edikt#63) ----

const HYPHEN_TOML: &str =
    "[dev-dependencies]\nserde_json = \"1\"\n\n[dependencies]\ntotal = 5\nlength = 2\n";

#[test]
fn bare_hyphenated_key_query_gets_the_quoting_hint() {
    // The form people type first parses fine as `.dev - dependencies()` and
    // failed at eval with a bare "unknown function `dependencies`", which named
    // neither the hyphen nor the fix.
    let (_o, err, code) = run(&["-t", "toml", ".dev-dependencies"], HYPHEN_TOML);
    assert_eq!(code, 2);
    assert!(
        err.contains("unknown function `dependencies`"),
        "got: {err}"
    );
    assert!(err.contains("contains `-`"), "got: {err}");
    assert!(err.contains(r#"."dev-dependencies""#), "got: {err}");
}

#[test]
fn the_hint_does_not_fire_on_an_unrelated_eval_failure() {
    // A legitimate hyphen subtraction plus an unrelated unknown function: the
    // hint must stay quiet, which is the objection the issue raised against
    // enriching eval errors at all.
    let (_o, err, code) = run(
        &["-t", "toml", ".dependencies | (.total-length) | nosuchfn"],
        HYPHEN_TOML,
    );
    assert_eq!(code, 2);
    assert!(err.contains("unknown function `nosuchfn`"), "got: {err}");
    assert!(!err.contains("contains `-`"), "got: {err}");
}

#[test]
fn a_real_hyphen_subtraction_still_evaluates() {
    // `length` is a builtin, so `.total-length` is arithmetic and never reaches
    // the diagnostic at all.
    let (out, _e, code) = run(
        &["-t", "toml", ".dependencies | .total-length"],
        HYPHEN_TOML,
    );
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "3");
}

// ---- documented .env quoting behaviour (edikt-087 BUG-3, a non-bug) ----

#[test]
fn env_quotes_are_value_bytes_not_syntax() {
    // There is no single .env grammar, so edikt interprets nothing: the quotes
    // are part of the value. Pinned because it reads like a bug in a report.
    let (out, _e, code) = run(&["-t", "env", ".APP_NAME"], "APP_NAME=\"my app\"\n");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "\"my app\"");
}

#[test]
fn env_assignment_replaces_the_whole_byte_run_after_the_separator() {
    let (out, _e, code) = run(
        &["-t", "env", ".APP_NAME = \"my app\""],
        "APP_NAME=\"my app\"\n",
    );
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "APP_NAME=my app");

    // Including the quotes in the new value keeps them.
    let (out, _e, code) = run(
        &["-t", "env", ".APP_NAME = \"\\\"my app\\\"\""],
        "APP_NAME=\"my app\"\n",
    );
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "APP_NAME=\"my app\"");
}

#[test]
fn env_setting_a_value_to_its_current_text_is_a_no_op() {
    // The losslessness property that actually matters for a quoted value.
    let src = "APP_NAME=\"my app\"\nBARE=plain\n";
    let (out, _e, code) = run(&["-t", "env", ".APP_NAME = \"\\\"my app\\\"\""], src);
    assert_eq!(code, 0);
    assert_eq!(out, src);
}

// ---- documented presence test (edikt-087 BUG-4, a non-bug) ----

#[test]
fn a_query_miss_is_a_silent_no_op_until_exit_status_is_asked_for() {
    let (out, _e, code) = run(&["-t", "jsonc", ".missing"], "{\"a\": 1}");
    assert_eq!((out.trim(), code), ("", 0), "sed-shaped by default");

    let (_o, _e, code) = run(&["-t", "jsonc", "--exit-status", ".missing"], "{\"a\": 1}");
    assert_eq!(code, 1, "--exit-status opts into jq's 1-on-no-results");

    let (_o, _e, code) = run(&["-t", "jsonc", "--exit-status", ".a"], "{\"a\": 1}");
    assert_eq!(code, 0, "and stays 0 when the key is present");
}

// ---- the envspaced dialect (edikt-087 BUG-2) ----

const SSHD: &str = "# managed\nPort 22\nPermitRootLogin\tyes\nHostKey    /etc/ssh/k\n";

#[test]
fn envspaced_queries_and_edits_from_the_cli() {
    let (out, _e, code) = run(&["-t", "envspaced", ".Port"], SSHD);
    assert_eq!((out.trim(), code), ("22", 0));

    // `spaced` is accepted as a shorthand.
    let (out, _e, code) = run(&["-t", "spaced", ".HostKey"], SSHD);
    assert_eq!((out.trim(), code), ("/etc/ssh/k", 0));
}

#[test]
fn envspaced_is_never_auto_detected() {
    // `sshd_config` has no extension, `.conf` already means INI, and a
    // `key value` line is indistinguishable from a malformed `.env` line, so
    // guessing would silently edit the wrong bytes.
    let (_o, err, code) = run(&["-t", "conf", ".Port"], SSHD);
    assert_eq!(code, 2, "a .conf/INI read of a spaced file must fail");
    assert!(err.contains("INI"), "got: {err}");
}

#[test]
fn envspaced_converts_in_both_directions() {
    let (out, _e, code) = run(&["-t", "json", "-T", "envspaced", "."], r#"{"Port": 22}"#);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "Port 22", "a spaced target must not emit `=`");

    let (out, _e, code) = run(&["-t", "envspaced", "-T", "json", "."], "Port 22\n");
    assert_eq!(code, 0);
    assert!(out.contains("\"Port\": \"22\""), "got: {out}");
}

// ---- auto-vivified nodes follow the file's layout (edikt-087 BUG-5) ----

#[test]
fn autovivified_object_matches_a_pretty_file() {
    let (out, _e, code) = run(
        &["-t", "jsonc", ".nonexistent.path = 1"],
        "{\n  \"include\": [\"src/**/*\"],\n}\n",
    );
    assert_eq!(code, 0);
    // Laid out in the file's own style, not `{"path":1}`; the trailing-comma
    // style and the untouched member both survive.
    assert_eq!(
        out,
        "{\n  \"include\": [\"src/**/*\"],\n  \"nonexistent\": {\n    \"path\": 1\n  },\n}\n"
    );
}

#[test]
fn autovivified_object_uses_one_level_not_the_full_depth() {
    // The indent unit is the member indent minus the closing brace's indent, so
    // new content inside a nested object indents by one level, not by its depth.
    let (out, _e, code) = run(
        &["-t", "jsonc", ".outer.new.deep = true"],
        "{\n  \"outer\": {\n    \"a\": 1\n  }\n}\n",
    );
    assert_eq!(code, 0);
    assert!(
        out.contains("\n    \"new\": {\n      \"deep\": true\n    }"),
        "got: {out}"
    );
}

#[test]
fn autovivified_object_follows_tabs() {
    let (out, _e, code) = run(&["-t", "jsonc", ".b.c = 2"], "{\n\t\"a\": 1\n}\n");
    assert_eq!(code, 0);
    assert!(
        out.contains("\n\t\"b\": {\n\t\t\"c\": 2\n\t}"),
        "got: {out:?}"
    );
}

#[test]
fn a_single_line_object_still_gets_a_compact_insert() {
    // Compact IS the file's style there, so matching it is the correct answer.
    let (out, _e, code) = run(&["-t", "jsonc", ".b.c = 2"], "{\"a\": 1}");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "{\"a\": 1, \"b\": {\"c\":2}}");
}

#[test]
fn a_scalar_assignment_is_unchanged_by_the_layout_work() {
    let (out, _e, code) = run(&["-t", "jsonc", ".b = 2"], "{\n  \"a\": 1\n}\n");
    assert_eq!(code, 0);
    assert_eq!(out, "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
}

// ---- duplicate keys collapse on the way out of the flat family ----

const DUP_ENV: &str = "PORT=22\nPORT=2222\nHOST=a\n";

#[test]
fn duplicate_keys_collapse_first_wins_with_a_warning() {
    let (out, err, code) = run(&["-t", "env", "-T", "json", "."], DUP_ENV);
    assert_eq!(code, 0);
    assert!(err.contains("duplicate key `PORT`"), "stderr: {err}");
    // First wins, matching what a query over the same file returns.
    assert!(out.contains("\"PORT\": \"22\""), "got: {out}");
    assert!(!out.contains("2222"), "got: {out}");
}

#[test]
fn a_query_and_a_conversion_agree_on_which_duplicate_wins() {
    // The reason first-wins rather than last-wins: `.PORT` already returns the
    // first, and a conversion disagreeing with a query would be indefensible.
    let (out, _e, _c) = run(&["-t", "env", ".PORT"], DUP_ENV);
    assert_eq!(out.trim(), "22");
}

#[test]
fn the_flat_family_keeps_its_duplicates() {
    // Duplicates are legal and meaningful there, so `-T env` must not collapse.
    let (out, err, code) = run(&["-t", "env", "-T", "env", "."], DUP_ENV);
    assert_eq!(code, 0);
    assert!(!err.contains("duplicate"), "stderr: {err}");
    assert!(
        out.contains("PORT=22") && out.contains("PORT=2222"),
        "got: {out}"
    );

    let (out, _e, _c) = run(
        &["-t", "envspaced", "-T", "envspaced", "."],
        "Port 22\nPort 2222\n",
    );
    assert!(
        out.contains("Port 22") && out.contains("Port 2222"),
        "got: {out}"
    );
}

#[test]
fn strict_promotes_the_duplicate_key_warning() {
    let (_o, err, code) = run(&["--strict", "-t", "env", "-T", "json", "."], DUP_ENV);
    assert_eq!(code, 2);
    assert!(err.contains("duplicate key"), "stderr: {err}");
}

#[test]
fn a_document_without_duplicates_converts_silently() {
    let (_o, err, code) = run(&["-t", "env", "-T", "json", "."], "A=1\nB=2\n");
    assert_eq!(code, 0);
    assert!(!err.contains("duplicate"), "stderr: {err}");
}
