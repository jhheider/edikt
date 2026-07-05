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
        // expected — ignore it. Dropping the handle closes stdin (EOF).
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
fn miss_is_exit_1() {
    let (out, _e, code) = run(&["-t", "jsonc", ".nope"], TSCONFIG);
    assert_eq!(out, "");
    assert_eq!(code, 1);
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
    // The exact case the demo hit — now works.
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
    // comments included — not re-serialized JSON.
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
    // emitter — YAML in, YAML out.
    let y = "web:\n  image: nginx\n  ports:\n    - 80\n";
    let (out, _e, code) = run(&["-t", "yaml", ".web | keys"], y);
    assert_eq!(out, "- image\n- ports\n");
    assert_eq!(code, 0);
}

#[test]
fn infeasible_output_names_candidates() {
    // An array result cannot be a top-level env document; the error suggests
    // formats whose feature set can hold it.
    let (_o, err, code) = run(&["-t", "yaml", "-T", "env", ".xs"], "xs:\n  - 1\n  - 2\n");
    assert_eq!(code, 2);
    assert!(err.contains("cannot represent"), "got: {err}");
    assert!(err.contains("yaml") && err.contains("toml"), "got: {err}");
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
    // YAML in, -o file.json → JSON written to the file, nothing on stdout.
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
    assert_eq!(code, 1); // grep-shaped miss
    assert!(!std::path::Path::new(&out).exists(), "no file on a miss");
}

#[test]
fn helpful_error_messages() {
    // Unknown format names the valid choices.
    let (_o, err, code) = run(&["-t", "bogus", "."], "a: 1\n");
    assert_eq!(code, 2);
    assert!(err.contains("unknown format `bogus`"), "got: {err}");
    assert!(err.contains("jsonc") && err.contains("yaml"), "got: {err}");

    // A failed edit path is named in the error.
    let (_o, err2, c2) = run(&["-t", "yaml", "del(.nope)"], "a: 1\n");
    assert_eq!(c2, 2);
    assert!(err2.contains(".nope"), "got: {err2}");

    // A bad expression chains its context.
    let (_o, err3, c3) = run(&["-t", "yaml", ".a |"], "a: 1\n");
    assert_eq!(c3, 2);
    assert!(err3.contains("bad expression"), "got: {err3}");
}
