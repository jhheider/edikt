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
