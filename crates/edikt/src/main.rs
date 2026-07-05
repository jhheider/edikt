//! The `edikt` CLI.
//!
//! Execution model is sed-shaped: read stdin (or files), apply an expression,
//! write stdout. Dispatches query, mutation, and conversion modes across all six
//! formats (JSONC/JSON5, INI, `.env`/`.properties`, TOML, YAML) over the
//! format-agnostic `Document` seam.
//!
//! Exit codes are grep-shaped: 0 = at least one result, 1 = query miss (no
//! results), 2 = parse / evaluation / I/O error.

use anyhow::{Context, Result, bail};
use clap::Parser;
use edikt_core::{Document, Expr, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "edikt",
    version,
    about = "Lossless, format-preserving config editor: JSONC, INI, .env, TOML, YAML.",
    long_about = "Query and losslessly edit JSONC/JSON5, INI, .env/.properties, TOML, \
and YAML with a jq-flavored expression language, changing only the bytes you target \
and leaving comments and layout untouched. Convert between formats with -T. Reads \
stdin and writes stdout by default, like sed."
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

    /// Edit files in place (requires a mutating expression or a conversion -T).
    #[arg(short = 'i', long = "in-place")]
    in_place: bool,

    /// Force the input format: jsonc | json5 | json | ini | env | properties | toml | yaml.
    #[arg(short = 't', long = "type", value_name = "FMT")]
    format: Option<String>,

    /// Convert to this output format (data-model; drops comments/trivia).
    #[arg(short = 'T', long = "to", value_name = "FMT")]
    to: Option<String>,

    /// In conversion, treat lossy degradations (dropped comments, flattening) as
    /// errors instead of warnings.
    #[arg(long)]
    strict: bool,

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
            // `{:#}` prints the full context chain (e.g. "<stdin>: path not
            // found: .a.b"), so the location and the cause both surface.
            eprintln!("edikt: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// The supported formats.
#[derive(Clone, Copy)]
enum Format {
    Jsonc,
    Ini,
    Env,
    Toml,
    Yaml,
}

/// Every format name accepted by `-t`/`-T`, for error messages.
const FORMAT_NAMES: &str = "jsonc, json5, json, ini, cfg, conf, env, properties, toml, yaml, yml";

/// Resolve a `-t`/`-T` format name.
fn format_from_name(name: &str) -> Result<Format> {
    match name.to_ascii_lowercase().as_str() {
        "jsonc" | "json5" | "json" => Ok(Format::Jsonc),
        "ini" | "cfg" | "conf" => Ok(Format::Ini),
        "env" | "properties" | "props" => Ok(Format::Env),
        "toml" => Ok(Format::Toml),
        "yaml" | "yml" => Ok(Format::Yaml),
        other => bail!("unknown format `{other}` (expected one of: {FORMAT_NAMES})"),
    }
}

/// Parse `src` in the given format into a boxed, format-agnostic document.
fn parse_document(format: Format, src: &str) -> Result<Box<dyn Document>> {
    Ok(match format {
        Format::Jsonc => Box::new(edikt_jsonc::parse(src)?),
        Format::Ini => Box::new(edikt_ini::parse(src)?),
        Format::Env => Box::new(edikt_env::parse(src)?),
        Format::Toml => Box::new(edikt_toml::parse(src)?),
        Format::Yaml => Box::new(edikt_yaml::parse(src)?),
    })
}

/// Emit a value in the target format, returning the text and any lossy-conversion
/// warnings.
fn emit(format: Format, value: &Value) -> Result<(String, Vec<String>)> {
    Ok(match format {
        Format::Jsonc => (edikt_jsonc::emit(value), Vec::new()),
        Format::Ini => edikt_ini::emit(value)?,
        Format::Env => edikt_env::emit(value)?,
        Format::Toml => edikt_toml::emit(value)?,
        Format::Yaml => edikt_yaml::emit(value)?,
    })
}

fn run(args: Args) -> Result<ExitCode> {
    let to_format = args.to.as_deref().map(format_from_name).transpose()?;

    // Resolve the program and the file list. Expression sources (-f then -e) win
    // over the positional; when any are present, every operand is a file.
    let mut sources: Vec<String> = Vec::new();
    for path in &args.script_files {
        sources.push(
            fs::read_to_string(path)
                .with_context(|| format!("reading script {}", path.display()))?,
        );
    }
    sources.extend(args.exprs.iter().cloned());

    let (program, files): (String, Vec<String>) = if !sources.is_empty() {
        // M1 composes multiple expression sources with a pipe (natural for
        // queries); the sed-style sequence semantics finalize with mutation.
        (join_pipe(&sources), args.operands.clone())
    } else if to_format.is_some() {
        // Conversion defaults to whole-document (`.`); all operands are files.
        (".".to_string(), args.operands.clone())
    } else {
        let mut it = args.operands.iter().cloned();
        let program = it
            .next()
            .context("no expression given (pass an expression, or -e/-f)")?;
        (program, it.collect())
    };

    let expr =
        edikt_core::parse(&program).with_context(|| format!("bad expression `{program}`"))?;
    let is_mutation = expr.is_mutation();

    if args.in_place && !is_mutation && to_format.is_none() {
        bail!("in-place (-i) needs a mutating expression or a conversion (-T)");
    }

    let inputs = read_inputs(&files)?;

    if let Some(target) = to_format {
        return convert(&args, &expr, &inputs, target);
    }

    let as_json = matches!((args.json, args.raw), (true, _));

    let mut emitted = false;
    for (path, src) in &inputs {
        let loc = display_path(path.as_deref());
        let format = detect_format(path.as_deref(), args.format.as_deref())?;
        let mut doc = parse_document(format, src).with_context(|| loc.clone())?;
        if is_mutation {
            doc.apply(&expr).with_context(|| loc.clone())?;
            let out = doc.to_source();
            if args.in_place {
                let p = path
                    .as_ref()
                    .context("cannot edit stdin in place; pass a file")?;
                std::fs::write(p, out).with_context(|| format!("writing {}", p.display()))?;
            } else {
                print!("{out}");
            }
            emitted = true;
        } else {
            let value = doc.to_value();
            let results = edikt_core::eval(&expr, &value).with_context(|| loc.clone())?;
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

/// Conversion mode: parse each input, evaluate `expr` against its value model,
/// and emit the result in `target`. Warns (or errors, with `--strict`) on lossy
/// degradations.
fn convert(
    args: &Args,
    expr: &Expr,
    inputs: &[(Option<PathBuf>, String)],
    target: Format,
) -> Result<ExitCode> {
    let mut emitted = false;
    for (path, src) in inputs {
        let loc = display_path(path.as_deref());
        let format = detect_format(path.as_deref(), args.format.as_deref())?;
        let doc = parse_document(format, src).with_context(|| loc.clone())?;
        let out_value = edikt_core::eval(expr, &doc.to_value())
            .with_context(|| loc.clone())?
            .into_iter()
            .next()
            .with_context(|| format!("{loc}: expression produced no value to convert"))?;

        let (output, mut warnings) = emit(target, &out_value).with_context(|| loc.clone())?;
        if doc.has_comments() {
            warnings.insert(0, "comments were dropped".to_string());
        }
        if args.strict && !warnings.is_empty() {
            bail!("{loc}: {} (--strict)", warnings.join("; "));
        }
        for w in &warnings {
            eprintln!("edikt: warning: {loc}: {w}");
        }

        if args.in_place {
            let p = path
                .as_ref()
                .context("cannot convert stdin in place; pass a file")?;
            std::fs::write(p, output).with_context(|| format!("writing {}", p.display()))?;
        } else {
            print!("{output}");
        }
        emitted = true;
    }
    Ok(if emitted {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Read each input as (path, contents). No files (or `-`) means stdin.
fn read_inputs(files: &[String]) -> Result<Vec<(Option<PathBuf>, String)>> {
    if files.is_empty() {
        return Ok(vec![(None, read_stdin()?)]);
    }
    let mut out = Vec::new();
    for f in files {
        if f == "-" {
            out.push((None, read_stdin()?));
        } else {
            let path = PathBuf::from(f);
            let src =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            out.push((Some(path), src));
        }
    }
    Ok(out)
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    Ok(buf)
}

fn detect_format(path: Option<&Path>, forced: Option<&str>) -> Result<Format> {
    if let Some(t) = forced {
        return format_from_name(t);
    }
    // `.env` (and `.env.local`, …) are dotfiles with no extension — match by name.
    if let Some(name) = path.and_then(|p| p.file_name()).and_then(|n| n.to_str())
        && (name == ".env" || name.starts_with(".env."))
    {
        return Ok(Format::Env);
    }
    match path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        Some("jsonc" | "json5" | "json") => Ok(Format::Jsonc),
        Some("ini" | "cfg" | "conf") => Ok(Format::Ini),
        Some("env" | "properties" | "props") => Ok(Format::Env),
        Some("toml") => Ok(Format::Toml),
        Some("yaml" | "yml") => Ok(Format::Yaml),
        Some(ext) => bail!("cannot infer format from `.{ext}`; pass -t (one of: {FORMAT_NAMES})"),
        None => bail!("cannot infer format (no extension); pass -t (one of: {FORMAT_NAMES})"),
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
