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
use edikt_core::{Document, Value};
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

    /// Read an expression/script from a file (repeatable). A script may open
    /// with header directives — `toFormat: FMT` / `type: FMT` — which CLI flags
    /// override; `#` header lines (comments, shebangs) are skipped.
    #[arg(short = 'f', long = "file", value_name = "PATH")]
    script_files: Vec<PathBuf>,

    /// Edit files in place (requires a mutating expression or a conversion -T).
    #[arg(short = 'i', long = "in-place")]
    in_place: bool,

    /// Write output to FILE instead of stdout. For queries/conversions the
    /// output format is inferred from FILE's extension (explicit -T/--fmt wins);
    /// for mutations FILE is just the destination. Nothing is written on a
    /// query miss.
    #[arg(
        short = 'o',
        long = "output",
        value_name = "FILE",
        conflicts_with = "in_place"
    )]
    output: Option<PathBuf>,

    /// Force the input format: jsonc | json5 | json | ini | env | properties | toml | yaml.
    #[arg(short = 't', long = "type", value_name = "FMT")]
    format: Option<String>,

    /// Output format. Default: the input format is preserved. (`-T FMT` and the
    /// `--json`/`--jsonc`/`--ini`/`--toml`/`--yaml` shorthands are equivalent.)
    #[arg(short = 'T', long = "to", value_name = "FMT", group = "outfmt")]
    to: Option<String>,

    /// Output as JSON (shorthand for `-T json`).
    #[arg(long, group = "outfmt")]
    json: bool,
    /// Output as JSONC (shorthand for `-T jsonc`).
    #[arg(long, group = "outfmt")]
    jsonc: bool,
    /// Output as INI (shorthand for `-T ini`).
    #[arg(long, group = "outfmt")]
    ini: bool,
    /// Output as TOML (shorthand for `-T toml`).
    #[arg(long, group = "outfmt")]
    toml: bool,
    /// Output as YAML (shorthand for `-T yaml`).
    #[arg(long, group = "outfmt")]
    yaml: bool,

    /// When the output format differs from the input, treat lossy degradations
    /// (dropped comments, flattening) as errors instead of warnings.
    #[arg(long)]
    strict: bool,

    /// Output raw scalars (the default; explicit opt-in).
    #[arg(short = 'r', long, conflicts_with = "outfmt")]
    raw: bool,
}

impl Args {
    /// The explicitly-requested output format, if any (`-T` or a `--fmt`
    /// shorthand). `None` means "preserve the input format".
    fn output_format(&self) -> Result<Option<Format>> {
        if self.json {
            Ok(Some(Format::Json))
        } else if self.jsonc {
            Ok(Some(Format::Jsonc))
        } else if self.ini {
            Ok(Some(Format::Ini))
        } else if self.toml {
            Ok(Some(Format::Toml))
        } else if self.yaml {
            Ok(Some(Format::Yaml))
        } else {
            self.to.as_deref().map(format_from_name).transpose()
        }
    }
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

/// The supported formats. `Json` shares JSONC's engine (JSON is a subset) but is
/// a distinct format: it has no `Comments` capability, so JSONC → JSON is a real
/// conversion that (warns and) drops comments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Jsonc,
    Ini,
    Env,
    Toml,
    Yaml,
}

/// Plain JSON's capabilities: everything JSONC has except comments.
const JSON_FEATURES: &[edikt_core::Feature] = &[
    edikt_core::Feature::Nesting,
    edikt_core::Feature::Arrays,
    edikt_core::Feature::TypedScalars,
];

impl Format {
    /// The format's capability set (used to pick candidate output formats).
    fn features(self) -> &'static [edikt_core::Feature] {
        match self {
            Format::Json => JSON_FEATURES,
            Format::Jsonc => edikt_jsonc::FEATURES,
            Format::Ini => edikt_ini::FEATURES,
            Format::Env => edikt_env::FEATURES,
            Format::Toml => edikt_toml::FEATURES,
            Format::Yaml => edikt_yaml::FEATURES,
        }
    }
    /// The canonical name, for messages.
    fn name(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Jsonc => "jsonc",
            Format::Ini => "ini",
            Format::Env => "env",
            Format::Toml => "toml",
            Format::Yaml => "yaml",
        }
    }
}

/// All formats, for candidate suggestions.
const ALL_FORMATS: [Format; 6] = [
    Format::Jsonc,
    Format::Json,
    Format::Ini,
    Format::Env,
    Format::Toml,
    Format::Yaml,
];

/// Every format name accepted by `-t`/`-T`, for error messages.
const FORMAT_NAMES: &str = "jsonc, json5, json, ini, cfg, conf, env, properties, toml, yaml, yml";

/// Resolve a `-t`/`-T` format name.
fn format_from_name(name: &str) -> Result<Format> {
    match name.to_ascii_lowercase().as_str() {
        "json" => Ok(Format::Json),
        "jsonc" | "json5" => Ok(Format::Jsonc),
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
        // JSON is read by the JSONC parser (it's a subset with no comments).
        Format::Json | Format::Jsonc => Box::new(edikt_jsonc::parse(src)?),
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
        Format::Json | Format::Jsonc => (edikt_jsonc::emit(value), Vec::new()),
        Format::Ini => edikt_ini::emit(value)?,
        Format::Env => edikt_env::emit(value)?,
        Format::Toml => edikt_toml::emit(value)?,
        Format::Yaml => edikt_yaml::emit(value)?,
    })
}

/// Directives a script file may declare in a leading header, before its first
/// expression. CLI flags override; across multiple `-f` files the last wins.
#[derive(Default)]
struct Directives {
    to: Option<String>,
    ty: Option<String>,
}

/// Split a script into (header directives, body). The header is any run of
/// blank lines, `#` comments (so shebang scripts work), and `key: value`
/// directive lines (`toFormat:`/`to:`, `type:`/`from:`); the body starts at the
/// first line that is none of those and is passed to the parser verbatim.
fn parse_script(src: &str) -> (Directives, String) {
    let mut d = Directives::default();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty() || t.starts_with('#') {
            i += 1;
            continue;
        }
        if let Some((k, v)) = t.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            // A directive value is a bare format word; anything else is body.
            if !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric()) {
                match k {
                    "toFormat" | "to" => {
                        d.to = Some(v.to_string());
                        i += 1;
                        continue;
                    }
                    "type" | "from" => {
                        d.ty = Some(v.to_string());
                        i += 1;
                        continue;
                    }
                    _ => {}
                }
            }
        }
        break;
    }
    (d, lines[i..].join("\n"))
}

fn run(args: Args) -> Result<ExitCode> {
    // Gather expression sources (-f script bodies, then -e), collecting script
    // header directives along the way.
    let mut directives = Directives::default();
    let mut sources: Vec<String> = Vec::new();
    for path in &args.script_files {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading script {}", path.display()))?;
        let (d, body) = parse_script(&raw);
        if d.to.is_some() {
            directives.to = d.to;
        }
        if d.ty.is_some() {
            directives.ty = d.ty;
        }
        if !body.trim().is_empty() {
            sources.push(body);
        }
    }
    sources.extend(args.exprs.iter().cloned());

    // Output format precedence: CLI (-T / --json/--jsonc/--ini/--toml/--yaml)
    // → `-o` FILE's extension → a script's `toFormat:` → input preserved.
    // Flag- and directive-requested formats are "hard" (a mutation cleanly
    // errors on them); an `-o`-derived format is advisory — the file is a sink,
    // and an unrecognized extension just means "keep the input format".
    let out_from_directive = directives.to.as_deref().map(format_from_name).transpose()?;
    let hard_out = args.output_format()?.or(out_from_directive);
    let explicit_out = args
        .output_format()?
        .or_else(|| {
            args.output
                .as_deref()
                .and_then(|p| detect_format(Some(p), None).ok())
        })
        .or(out_from_directive);
    // Input format: -t beats a script's `type:`; absent both, detect by name.
    let forced_type: Option<String> = args.format.clone().or(directives.ty);

    // Operands → program + files (sed-shaped: first operand is the expression
    // when no -e/-f). With an output format set, a whole-document conversion is
    // the common intent, so a first operand that names a file (or `-`) is taken
    // as a file with program `.` — while `-T json '.a' f.yaml` still reads `.a`
    // as the expression.
    let (program, files): (String, Vec<String>) = if !sources.is_empty() {
        (join_pipe(&sources), args.operands.clone())
    } else if let Some(first) = args.operands.first() {
        if explicit_out.is_some() && (first == "-" || Path::new(first).exists()) {
            (".".to_string(), args.operands.clone())
        } else {
            (first.clone(), args.operands[1..].to_vec())
        }
    } else if explicit_out.is_some() {
        (".".to_string(), Vec::new())
    } else {
        bail!("no expression given (pass an expression, or -e/-f)");
    };

    let expr =
        edikt_core::parse(&program).with_context(|| format!("bad expression `{program}`"))?;
    let is_mutation = expr.is_mutation();

    if is_mutation && let Some(out) = hard_out {
        bail!(
            "cannot combine a mutation with an output format (--{}); edit first, then convert",
            out.name()
        );
    }
    if args.in_place && !is_mutation && explicit_out.is_none() {
        bail!("in-place (-i) needs a mutating expression or an output format (-T)");
    }

    let inputs = read_inputs(&files)?;

    // With -o, everything accumulates here and is written once at the end
    // (nothing is written on a query miss).
    let mut file_out: Vec<String> = Vec::new();
    let mut emitted = false;
    for (path, src) in &inputs {
        let loc = display_path(path.as_deref());
        let in_fmt = detect_format(path.as_deref(), forced_type.as_deref())?;
        let mut doc = parse_document(in_fmt, src).with_context(|| loc.clone())?;

        if is_mutation {
            doc.apply(&expr).with_context(|| loc.clone())?;
            let out = doc.to_source();
            if args.in_place {
                let p = path
                    .as_ref()
                    .context("cannot edit stdin in place; pass a file")?;
                std::fs::write(p, out).with_context(|| format!("writing {}", p.display()))?;
            } else if args.output.is_some() {
                file_out.push(out);
            } else {
                print!("{out}");
            }
            emitted = true;
            continue;
        }

        // Query / conversion (one unified mode: output format = explicit or
        // input-preserved).
        let target = explicit_out.unwrap_or(in_fmt);
        let results = edikt_core::eval(&expr, &doc.to_value()).with_context(|| loc.clone())?;

        // Format-preserving get: a pure-path query staying in-format returns the
        // original source slices (comments and layout intact). The counts must
        // align 1:1 with the evaluator; otherwise fall back to emitting.
        let slices: Option<Vec<String>> = if target == in_fmt {
            expr.as_path()
                .map(|p| doc.source_slice(p))
                .filter(|s| s.len() == results.len())
        } else {
            None
        };

        // Converting away from a commented source drops the comments.
        if target != in_fmt && doc.has_comments() {
            let w = "comments were dropped";
            if args.strict {
                bail!("{loc}: {w} (--strict)");
            }
            eprintln!("edikt: warning: {loc}: {w}");
        }

        let mut outputs: Vec<String> = Vec::new();
        for (i, r) in results.iter().enumerate() {
            // Slices serve structural results (that's where layout lives);
            // scalars always render raw, matching the output contract.
            if matches!(r, Value::Array(_) | Value::Object(_))
                && let Some(s) = slices.as_ref().map(|s| &s[i])
            {
                outputs.push(s.clone());
                continue;
            }
            outputs.push(render_value(
                &args,
                r,
                target,
                explicit_out.is_some(),
                &loc,
            )?);
        }

        if args.in_place {
            let p = path
                .as_ref()
                .context("cannot convert stdin in place; pass a file")?;
            let joined = terminated(&outputs);
            std::fs::write(p, joined).with_context(|| format!("writing {}", p.display()))?;
            emitted = true;
        } else if args.output.is_some() {
            emitted |= !outputs.is_empty();
            file_out.extend(outputs);
        } else {
            for out in &outputs {
                if out.ends_with('\n') {
                    print!("{out}");
                } else {
                    println!("{out}");
                }
                emitted = true;
            }
        }
    }

    // Write the -o sink once, after all inputs — and only if something matched.
    if let Some(p) = &args.output
        && emitted
    {
        std::fs::write(p, terminated(&file_out))
            .with_context(|| format!("writing {}", p.display()))?;
    }

    Ok(if emitted {
        ExitCode::SUCCESS
    } else {
        // Grep-shaped miss (query with no results).
        ExitCode::from(1)
    })
}

/// Render one query result in `target`: scalars raw (JSON-encoded only when
/// JSON was explicitly requested); structural values via the target's emitter,
/// with flatten warnings surfaced (or fatal under --strict) and an infeasible
/// emit turned into an error naming the formats that *can* hold the value.
fn render_value(
    args: &Args,
    value: &Value,
    target: Format,
    explicit: bool,
    loc: &str,
) -> Result<String> {
    if !matches!(value, Value::Array(_) | Value::Object(_)) {
        // Scalars are raw text, except an explicitly-requested JSON-family
        // output JSON-encodes them (strings quoted) for machine consumers.
        return Ok(
            if explicit && matches!(target, Format::Json | Format::Jsonc) {
                value.to_json()
            } else {
                value.to_raw_string()
            },
        );
    }
    let (text, warnings) = match emit(target, value) {
        Ok(ok) => ok,
        Err(e) => {
            let needed = edikt_core::convert::features_used(value);
            let candidates: Vec<&str> = ALL_FORMATS
                .iter()
                .filter(|f| needed.iter().all(|n| f.features().contains(n)))
                .map(|f| f.name())
                .collect();
            bail!(
                "{loc}: cannot represent this result as {}: {e}; try an output format \
                 that can hold it (-T {})",
                target.name(),
                candidates.join(", -T ")
            );
        }
    };
    if args.strict && !warnings.is_empty() {
        bail!("{loc}: {} (--strict)", warnings.join("; "));
    }
    for w in &warnings {
        eprintln!("edikt: warning: {loc}: {w}");
    }
    Ok(text)
}

/// Join outputs, newline-terminating each (for `-i` writes).
fn terminated(outputs: &[String]) -> String {
    let mut joined = String::new();
    for out in outputs {
        joined.push_str(out);
        if !out.ends_with('\n') {
            joined.push('\n');
        }
    }
    joined
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
        Some("json") => Ok(Format::Json),
        Some("jsonc" | "json5") => Ok(Format::Jsonc),
        Some("ini" | "cfg" | "conf") => Ok(Format::Ini),
        Some("env" | "properties" | "props") => Ok(Format::Env),
        Some("toml") => Ok(Format::Toml),
        Some("yaml" | "yml") => Ok(Format::Yaml),
        Some(ext) => bail!("cannot infer format from `.{ext}`; pass -t (one of: {FORMAT_NAMES})"),
        None => bail!("cannot infer format (no extension); pass -t (one of: {FORMAT_NAMES})"),
    }
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
