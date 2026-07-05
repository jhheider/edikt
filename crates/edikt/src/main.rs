//! The `edikt` CLI.
//!
//! Execution model is sed-shaped: read stdin (or files), apply an expression,
//! write stdout. M1 wires **query mode** on JSONC end-to-end. Mutation (`-i`,
//! `set`/`del`) and the other formats arrive in later slices; unimplemented
//! surface errors clearly rather than misleading.
//!
//! Exit codes are grep-shaped: 0 = at least one result, 1 = query miss (no
//! results), 2 = parse / evaluation / I/O error.

use clap::Parser;
use edikt_core::{Document, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "edikt",
    version,
    about = "Format-preserving structured-config editor (JSONC and INI; .env coming).",
    long_about = "Query and edit JSONC/JSON5 and INI files with a jq-flavored \
expression language, preserving comments and layout. Reads stdin and writes \
stdout by default, like sed."
)]
struct Args {
    /// Expression, then files. With -e/-f present, ALL operands are files.
    #[arg(value_name = "EXPR|FILE")]
    operands: Vec<String>,

    /// Expression to apply (repeatable). Composes with -f in order.
    #[arg(short = 'e', long = "expr", value_name = "EXPR")]
    exprs: Vec<String>,

    /// Read an expression/script from a file (repeatable).
    #[arg(short = 'f', long = "file", value_name = "PATH")]
    script_files: Vec<PathBuf>,

    /// Edit files in place (requires a mutating expression — arrives in M2).
    #[arg(short = 'i', long = "in-place")]
    in_place: bool,

    /// Force the input format: jsonc | json5 | json (ini/env coming).
    #[arg(short = 't', long = "type", value_name = "FMT")]
    format: Option<String>,

    /// Output JSON-encoded values.
    #[arg(long, conflicts_with = "raw")]
    json: bool,

    /// Output raw scalars (the default; explicit opt-in).
    #[arg(short = 'r', long)]
    raw: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("edikt: {e}");
            ExitCode::from(2)
        }
    }
}

/// The supported formats.
enum Format {
    Jsonc,
    Ini,
}

/// Parse `src` in the given format into a boxed, format-agnostic document.
fn parse_document(format: Format, src: &str) -> Result<Box<dyn Document>, String> {
    match format {
        Format::Jsonc => Ok(Box::new(
            edikt_jsonc::parse(src).map_err(|e| e.to_string())?,
        )),
        Format::Ini => Ok(Box::new(edikt_ini::parse(src).map_err(|e| e.to_string())?)),
    }
}

fn run(args: Args) -> Result<ExitCode, String> {
    // Resolve the program and the file list. Expression sources (-f then -e) win
    // over the positional; when any are present, every operand is a file.
    let mut sources: Vec<String> = Vec::new();
    for path in &args.script_files {
        sources.push(
            fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?,
        );
    }
    sources.extend(args.exprs.iter().cloned());

    let (program, files): (String, Vec<String>) = if sources.is_empty() {
        let mut it = args.operands.iter().cloned();
        let program = it.next().ok_or("no expression given")?;
        (program, it.collect())
    } else {
        // M1 composes multiple expression sources with a pipe (natural for
        // queries); the sed-style sequence semantics finalize with mutation.
        (join_pipe(&sources), args.operands.clone())
    };

    let expr = edikt_core::parse(&program).map_err(|e| format!("bad expression: {e}"))?;
    let is_mutation = expr.is_mutation();

    if args.in_place && !is_mutation {
        return Err("in-place (-i) needs a mutating expression, e.g. `.a.b = false`".to_string());
    }

    let inputs = read_inputs(&files)?;
    let as_json = matches!((args.json, args.raw), (true, _));

    let mut emitted = false;
    for (path, src) in &inputs {
        let loc = display_path(path.as_deref());
        let format = detect_format(path.as_deref(), args.format.as_deref())?;
        let mut doc = parse_document(format, src).map_err(|e| format!("{loc}: {e}"))?;
        if is_mutation {
            doc.apply(&expr).map_err(|e| format!("{loc}: {e}"))?;
            let out = doc.to_source();
            if args.in_place {
                let p = path
                    .as_ref()
                    .ok_or("cannot edit stdin in place; pass a file")?;
                std::fs::write(p, out).map_err(|e| format!("writing {}: {e}", p.display()))?;
            } else {
                print!("{out}");
            }
            emitted = true;
        } else {
            let value = doc.to_value();
            let results = edikt_core::eval(&expr, &value).map_err(|e| format!("{loc}: {e}"))?;
            for r in &results {
                println!("{}", render(r, as_json));
                emitted = true;
            }
        }
    }

    Ok(if emitted {
        ExitCode::SUCCESS
    } else {
        // Grep-shaped miss (query with no results).
        ExitCode::from(1)
    })
}

/// Read each input as (path, contents). No files (or `-`) means stdin.
fn read_inputs(files: &[String]) -> Result<Vec<(Option<PathBuf>, String)>, String> {
    if files.is_empty() {
        return Ok(vec![(None, read_stdin()?)]);
    }
    let mut out = Vec::new();
    for f in files {
        if f == "-" {
            out.push((None, read_stdin()?));
        } else {
            let path = PathBuf::from(f);
            let src = fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            out.push((Some(path), src));
        }
    }
    Ok(out)
}

fn read_stdin() -> Result<String, String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("reading stdin: {e}"))?;
    Ok(buf)
}

fn detect_format(path: Option<&Path>, forced: Option<&str>) -> Result<Format, String> {
    if let Some(t) = forced {
        return match t.to_ascii_lowercase().as_str() {
            "jsonc" | "json5" | "json" => Ok(Format::Jsonc),
            "ini" | "cfg" | "conf" => Ok(Format::Ini),
            "env" | "properties" | "props" => {
                Err(".env/.properties support arrives in M5".to_string())
            }
            other => Err(format!("unknown format `{other}`")),
        };
    }
    match path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        Some("jsonc" | "json5" | "json") => Ok(Format::Jsonc),
        Some("ini" | "cfg" | "conf") => Ok(Format::Ini),
        Some("env" | "properties") => Err(".env/.properties support arrives in M5".to_string()),
        Some(ext) => Err(format!("cannot infer format from `.{ext}`; pass -t")),
        None => Err("cannot infer format (no extension); pass -t".to_string()),
    }
}

fn render(v: &Value, json: bool) -> String {
    if json { v.to_json() } else { v.to_raw_string() }
}

fn join_pipe(sources: &[String]) -> String {
    sources
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn display_path(path: Option<&Path>) -> String {
    match path {
        Some(p) => p.display().to_string(),
        None => "<stdin>".to_string(),
    }
}
