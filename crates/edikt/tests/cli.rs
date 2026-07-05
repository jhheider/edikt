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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
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
fn in_place_query_errors_until_m2() {
    let (_o, err, code) = run(&["-t", "jsonc", "-i", ".a"], "{\"a\":1}");
    assert_eq!(code, 2);
    assert!(err.contains("mutation"));
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
