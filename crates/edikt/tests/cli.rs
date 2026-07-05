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
    let (out, _e, code) = run(&["-t", "jsonc", "--json", ".compilerOptions.lib"], TSCONFIG);
    assert_eq!(out, "[\"ES2020\",\"DOM\"]\n");
    assert_eq!(code, 0);
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
fn yaml_edit_is_refused_clearly() {
    let (_o, err, code) = run(&["-t", "yaml", "-i", ".a = 1"], "a: 0\n");
    assert_eq!(code, 2);
    assert!(err.contains("YAML"));
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
}
