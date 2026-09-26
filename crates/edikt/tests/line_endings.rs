//! Line endings, end to end over the fixture corpus.
//!
//! The contract: an untouched line keeps its own line ending, a line an edit
//! adds takes the file's dominant one.
//! Each CRLF fixture is edited twice through the binary: once with a no-op
//! assignment (the output must be the input, byte for byte), and once with
//! edits that insert lines (no bare `\n` may appear).

use std::path::PathBuf;
use std::process::Command;

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
}

/// Run edikt over a fixture; return (stdout, stderr, exit code).
fn run(args: &[&str], rel: &str) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_edikt"))
        .args(args)
        .arg(fixture(rel))
        .output()
        .expect("spawn edikt");
    (
        String::from_utf8(out.stdout).expect("utf-8 stdout"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(fixture(rel)).unwrap()
}

/// The byte offsets of every `\n` not preceded by `\r`.
fn bare_lfs(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    (0..b.len())
        .filter(|&i| b[i] == b'\n' && (i == 0 || b[i - 1] != b'\r'))
        .collect()
}

/// (fixture, leading args, a no-op assignment, programs that add lines; a
/// comment edit can't share a program with a value edit, so it runs alone).
type Case = (
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static [&'static str],
);
const CRLF_CASES: &[Case] = &[
    (
        "toml/crlf.toml",
        &[],
        r#".title = "crlf""#,
        &[
            r#".server.added = 1 | .fresh.k = "v" | .text.basic = "a\nb""#,
            r#".server.# = "c""#,
        ],
    ),
    (
        "jsonc/crlf.jsonc",
        &[],
        r#".name = "crlf""#,
        &[
            r#".nested.obj = {"x": [1, 2]} | .nested.list += [3] | .top = 1"#,
            r#".nested.# = "c""#,
        ],
    ),
    (
        "ini/crlf.ini",
        &[],
        r#".server.port = "8080""#,
        &[
            r#".server.added = "1" | .fresh.k = "v""#,
            r#".server.port.# = "c""#,
        ],
    ),
    (
        "env/crlf.env",
        &[],
        r#".DEBUG = "true""#,
        &[r#".ADDED = "1""#, r#".DEBUG.# = "c""#],
    ),
    (
        "env/crlf.envspaced",
        &["-t", "envspaced"],
        r#".Port = "22""#,
        &[r#".Added = "1""#, r#".PermitRootLogin.# = "c""#],
    ),
    (
        "yaml/crlf.yaml",
        &[],
        r#".name = "crlf""#,
        &[
            r#".server.extra = {"a": [1, 2]} | .server.ports += [1]"#,
            r#".server.# = "c""#,
        ],
    ),
    (
        "kdl/crlf.kdl",
        &[],
        r#".title = "crlf""#,
        &[r#".server.added = 1 | .fresh = 2"#, r#".server.# = "c""#],
    ),
    (
        "markdown/crlf-yaml.md",
        &[],
        r#".title = "CRLF post""#,
        &[r#".extra.k = 1 | .tags += ["b"]"#],
    ),
    (
        "markdown/crlf-toml.md",
        &[],
        r#".title = "CRLF post""#,
        &[r#".extra.added = 1 | .fresh.k = 2"#],
    ),
    (
        "markdown/crlf-pep723.py",
        &["-t", "markdown"],
        r#".requires-python = ">=3.11""#,
        &[r#".tool.uv.added = 1 | .dependencies += ["httpx"]"#],
    ),
];

#[test]
fn crlf_fixtures_survive_a_no_op_edit_byte_for_byte() {
    for (rel, pre, noop, _) in CRLF_CASES {
        let mut args = pre.to_vec();
        args.push(noop);
        let (out, err, code) = run(&args, rel);
        assert_eq!(code, 0, "{rel}: {err}");
        assert_eq!(out, read(rel), "{rel}: a no-op edit must not change a byte");
    }
}

#[test]
fn lines_an_edit_adds_to_a_crlf_file_end_in_crlf() {
    for (rel, pre, _, programs) in CRLF_CASES {
        for edits in *programs {
            let mut args = pre.to_vec();
            args.push(edits);
            let (out, err, code) = run(&args, rel);
            assert_eq!(code, 0, "{rel} `{edits}`: {err}");
            assert!(out.len() > read(rel).len(), "{rel} `{edits}` added nothing");
            assert_eq!(
                bare_lfs(&out),
                Vec::<usize>::new(),
                "{rel} `{edits}`: bare LF in\n{out}"
            );
        }
    }
}

#[test]
fn mixed_endings_keep_each_lines_own_ending() {
    let rel = "toml/mixed-endings.toml";
    let src = read(rel);
    let (out, _, code) = run(&[".a = 1"], rel);
    assert_eq!(code, 0);
    assert_eq!(out, src, "identity must keep every line's ending");

    // The file is mostly CRLF: the rewritten line and the new key are CRLF,
    // and the LF lines around them (one inside the multi-line string, one
    // just before the new key) stay LF.
    let (out, _, _) = run(&[".b = 3 | .t.n = 1"], rel);
    assert_eq!(
        out,
        "# mostly CRLF\r\na = 1\nb = 3\r\ns = \"\"\"\r\nx\ny\r\n\"\"\"\r\n[t]\r\nk = \"v\"\nn = 1\r\n"
    );
}
