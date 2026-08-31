//! The `edikt` CLI.
//!
//! Execution model is sed-shaped: read stdin (or files), apply an expression,
//! write stdout. Dispatches query, mutation, and conversion modes across all
//! seven formats (JSONC/JSON5, INI, `.env`/`.properties`, TOML, YAML, KDL) over the
//! format-agnostic `Document` seam.
//!
//! Exit codes are sed-shaped: 0 = success, including a query that matched
//! nothing (a silent no-op, like sed with no matching address); 2 = parse /
//! evaluation / I/O error. `--exit-status` opts into jq's 1-on-no-results for
//! presence tests.

use anyhow::{Context, Result, bail};
use clap::Parser;
use edikt_core::{Commented, Document, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "edikt",
    version,
    about = "Lossless, format-preserving config editor: JSONC, INI, .env, TOML, YAML, KDL.",
    long_about = "Query and losslessly edit JSONC/JSON5, INI, .env/.properties, TOML, \
YAML, and KDL with a jq-flavored expression language, changing only the bytes you \
target and leaving comments and layout untouched. Convert between formats with -T. \
Reads stdin and writes stdout by default, like sed.",
    after_help = "Examples:
  edikt '.compilerOptions.target' tsconfig.json         query: raw scalar out
  edikt -i '.compilerOptions.strict = true' tsconfig.jsonc
                                                        edit in place; comments,
                                                        commas, indent all survive
  edikt -i '.VERSION |= sub(\"^v\"; \"\")' .env             regex edit
  edikt '.services.web' compose.yaml                    structural get: exact
                                                        source bytes
  edikt -T yaml tsconfig.jsonc                          convert; comments carried
  cat app.cfg | edikt -t ini '.server.port'             sed-shaped stdin
  edikt '.title' post.md                                Markdown frontmatter
  edikt '.kind' k8s.yaml                                 one result per YAML doc
  edikt -i '^d1.spec.replicas = 3' k8s.yaml             edit the 2nd doc of a stream
  edikt -i 'select(.kind==\"Service\") | .x = 1' k8s.yaml   target docs by content"
)]
struct Args {
    /// Expression, then files. With -e/-f present, ALL operands are files.
    #[arg(value_name = "EXPR|FILE")]
    operands: Vec<String>,

    /// Expression to apply (repeatable). Composes with -f in order.
    #[arg(short = 'e', long = "expr", value_name = "EXPR")]
    exprs: Vec<String>,

    /// Read an expression/script from a file (repeatable). A script may open
    /// with header directives (`toFormat: FMT` / `type: FMT`) which CLI flags
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

    /// Force the input format: jsonc | json5 | json | ini | env | properties | toml | yaml | kdl | markdown.
    #[arg(short = 't', long = "type", value_name = "FMT")]
    format: Option<String>,

    /// Output format. Default: the input format is preserved. (`-T FMT` and the
    /// `--json`/`--jsonc`/`--ini`/`--toml`/`--yaml`/`--kdl` shorthands are equivalent.)
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
    /// Output as KDL (shorthand for `-T kdl`).
    #[arg(long, group = "outfmt")]
    kdl: bool,

    /// When the output format differs from the input, treat lossy degradations
    /// (dropped comments, flattening) as errors instead of warnings.
    #[arg(long)]
    strict: bool,

    /// Output raw scalars (the default; explicit opt-in).
    #[arg(short = 'r', long, conflicts_with = "outfmt")]
    raw: bool,

    /// Exit 1 when a query produces no results (jq-style, for presence
    /// tests). The default is sed-shaped: a miss is a silent no-op, exit 0.
    #[arg(long = "exit-status")]
    exit_status: bool,

    /// Print shell completions to stdout (for packagers; bash|zsh|fish|...).
    #[arg(long, value_name = "SHELL", hide = true)]
    completions: Option<clap_complete::Shell>,

    /// Print the man page (roff) to stdout (for packagers).
    #[arg(long, hide = true)]
    manpage: bool,
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
        } else if self.kdl {
            Ok(Some(Format::Kdl))
        } else {
            let fmt = self.to.as_deref().map(format_from_name).transpose()?;
            // `frontmatter` is a read-only lens over a metadata block, not an
            // emittable format. Reject it here so the failure is one clean
            // sentence, not a doubled message from the emit path.
            if fmt == Some(Format::Frontmatter) {
                bail!(
                    "cannot convert to frontmatter: it is a read-only view of a Markdown/script \
                     metadata block, not an output format; use -T with the block's own \
                     language, e.g. -T yaml"
                );
            }
            Ok(fmt)
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    // Packager outputs (hidden flags): the binary is its own doc generator,
    // so release archives and package builds need no extra tooling.
    if let Some(shell) = args.completions {
        use clap::CommandFactory;
        clap_complete::generate(shell, &mut Args::command(), "edikt", &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }
    if args.manpage {
        use clap::CommandFactory;
        let man = clap_mangen::Man::new(Args::command());
        if let Err(e) = man.render(&mut std::io::stdout()) {
            eprintln!("edikt: rendering man page: {e}");
            return ExitCode::from(2);
        }
        return ExitCode::SUCCESS;
    }
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
/// a distinct format: it has no `Comments` capability, so JSONC -> JSON is a real
/// conversion that (warns and) drops comments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Jsonc,
    Ini,
    Env,
    Toml,
    Yaml,
    Kdl,
    /// The frontmatter lens: edit the metadata block of a Markdown file, body
    /// left opaque. An input-only format; never a conversion target.
    Frontmatter,
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
            Format::Kdl => edikt_kdl::FEATURES,
            Format::Frontmatter => edikt_frontmatter::FEATURES,
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
            Format::Kdl => "kdl",
            Format::Frontmatter => "frontmatter",
        }
    }
}

/// All formats, for candidate suggestions.
const ALL_FORMATS: [Format; 7] = [
    Format::Jsonc,
    Format::Json,
    Format::Ini,
    Format::Env,
    Format::Toml,
    Format::Yaml,
    Format::Kdl,
];

/// Every format name accepted by `-t`/`-T`, for error messages.
const FORMAT_NAMES: &str =
    "jsonc, json5, json, ini, cfg, conf, env, properties, toml, yaml, yml, kdl, markdown";

/// Resolve a `-t`/`-T` format name.
fn format_from_name(name: &str) -> Result<Format> {
    match name.to_ascii_lowercase().as_str() {
        "json" => Ok(Format::Json),
        "jsonc" | "json5" => Ok(Format::Jsonc),
        "ini" | "cfg" | "conf" => Ok(Format::Ini),
        "env" | "properties" | "props" => Ok(Format::Env),
        "toml" => Ok(Format::Toml),
        "yaml" | "yml" => Ok(Format::Yaml),
        "kdl" => Ok(Format::Kdl),
        "markdown" | "md" | "mdx" | "qmd" | "frontmatter" | "fm" => Ok(Format::Frontmatter),
        other => bail!("unknown format `{other}` (expected one of: {FORMAT_NAMES})"),
    }
}

/// Parse `src` in the given format into a boxed, format-agnostic document.
/// Resolve a leading `^dN` document selector for a mutation. Errors if the
/// index is out of range; on a single-document input returns the unwrapped body
/// (so `^d0` works on any format, not just multi-document YAML); on a genuine
/// multi-document input returns `None`, keeping the selector for the edit path
/// to scope to that document.
fn resolve_doc_select(
    expr: &edikt_core::Expr,
    doc: &dyn Document,
) -> Result<Option<edikt_core::Expr>> {
    let edikt_core::Expr::DocSelect(idx, body) = expr else {
        return Ok(None);
    };
    let count = doc.to_values().len();
    if *idx >= count {
        bail!(
            "document `^d{idx}` is out of range ({count} document{})",
            if count == 1 { "" } else { "s" }
        );
    }
    if count == 1 {
        Ok(Some((**body).clone()))
    } else {
        Ok(None)
    }
}

fn parse_document(format: Format, src: &str) -> Result<Box<dyn Document>> {
    Ok(match format {
        // JSON is read by the JSONC parser (it's a subset with no comments).
        Format::Json | Format::Jsonc => Box::new(edikt_jsonc::parse(src)?),
        Format::Ini => Box::new(edikt_ini::parse(src)?),
        Format::Env => Box::new(edikt_env::parse(src)?),
        Format::Toml => Box::new(edikt_toml::parse(src)?),
        Format::Yaml => Box::new(edikt_yaml::parse(src)?),
        Format::Kdl => Box::new(edikt_kdl::parse(src)?),
        Format::Frontmatter => Box::new(edikt_frontmatter::parse(src)?),
    })
}

/// Emit a commented value in the target format, returning the text and any
/// lossy-conversion warnings. Comments place natively per format; a kind the
/// format can't hold remaps or drops with a warning from its emitter. (JSON
/// shares JSONC's emitter; the caller strips comments first, since JSON lacks
/// the `Comments` feature entirely.)
fn emit(format: Format, c: &Commented) -> Result<(String, Vec<String>)> {
    Ok(match format {
        Format::Json | Format::Jsonc => (edikt_jsonc::emit_commented(c), Vec::new()),
        Format::Ini => edikt_ini::emit_commented(c)?,
        Format::Env => edikt_env::emit_commented(c)?,
        Format::Toml => edikt_toml::emit_commented(c)?,
        Format::Yaml => edikt_yaml::emit_commented(c)?,
        Format::Kdl => edikt_kdl::emit_commented(c)?,
        Format::Frontmatter => bail!(
            "cannot convert to `frontmatter`: it is an input lens over a Markdown \
             block, not an output format; use -T with the block's own format (yaml, toml, json)"
        ),
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
    // -> `-o` FILE's extension -> a script's `toFormat:` -> input preserved.
    // Flag- and directive-requested formats are "hard" (a mutation cleanly
    // errors on them); an `-o`-derived format is advisory; the file is a sink,
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

    // Operands -> program + files (sed-shaped: first operand is the expression
    // when no -e/-f). With an output format set, a whole-document conversion is
    // the common intent, so a first operand that names a readable path (or `-`)
    // is taken as a file with program `.`, while `-T json '.a' f.yaml` still
    // reads `.a` as the expression. Directories are excluded (`.` - a directory
    // that always exists) is the identity expression, never an input), but not
    // narrowed to regular files: process substitution hands us fifos.
    let (program, files): (String, Vec<String>) = if !sources.is_empty() {
        (join_pipe(&sources), args.operands.clone())
    } else if let Some(first) = args.operands.first() {
        let p = Path::new(first);
        if explicit_out.is_some() && (first == "-" || (p.exists() && !p.is_dir())) {
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
            // Resolve a leading `^dN` up front: out-of-range errors; on a
            // single-document input `^d0` unwraps to its body (so it works on
            // any format, not just multi-doc YAML); a genuine multi-document
            // input keeps the selector for the edit path to scope.
            let unwrapped = resolve_doc_select(&expr, doc.as_ref()).with_context(|| loc.clone())?;
            let mexpr: &edikt_core::Expr = unwrapped.as_ref().unwrap_or(&expr);
            // A comment mutation (`.foo.# = ...`) writes through the comment
            // methods; everything else through the value edit path.
            if mexpr.has_comment() {
                let warnings = edikt_core::apply_comment_mutation(doc.as_mut(), mexpr)
                    .with_context(|| loc.clone())?;
                if args.strict && !warnings.is_empty() {
                    bail!("{loc}: {} (--strict)", warnings.join("; "));
                }
                for w in &warnings {
                    eprintln!("edikt: warning: {loc}: {w}");
                }
            } else {
                let warnings = doc.apply(mexpr).with_context(|| loc.clone())?;
                if args.strict && !warnings.is_empty() {
                    bail!("{loc}: {} (--strict)", warnings.join("; "));
                }
                for w in &warnings {
                    eprintln!("edikt: warning: {loc}: {w}");
                }
            }
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
        // input-preserved). A lens (frontmatter) is not itself emittable, so an
        // un-directed query over it renders in the block's own format.
        let target = explicit_out
            .or_else(|| doc.inner_format().and_then(|n| format_from_name(n).ok()))
            .unwrap_or(in_fmt);
        // A comment query (`.foo.#`) resolves against the commented projection;
        // everything else over the value model.
        let results = if expr.has_comment() {
            // Map a comment query over each document's commented projection, so
            // `.foo.#` yields one result per document (single-document formats
            // have exactly one); a leading `^dN` selects one by position.
            let all = doc.to_commented_all();
            if all.is_empty() {
                bail!("{loc}: this format has no comments to query");
            }
            let (selected, body): (Vec<&edikt_core::Commented>, &edikt_core::Expr) = match &expr {
                edikt_core::Expr::DocSelect(idx, body) => {
                    if *idx >= all.len() {
                        bail!(
                            "{loc}: document `^d{idx}` is out of range ({} document{})",
                            all.len(),
                            if all.len() == 1 { "" } else { "s" }
                        );
                    }
                    (vec![&all[*idx]], body)
                }
                _ => (all.iter().collect(), &expr),
            };
            let mut out = Vec::new();
            for c in selected {
                out.extend(edikt_core::eval_with_comments(body, c).with_context(|| loc.clone())?);
            }
            out
        } else {
            // Evaluate against each top-level document and concatenate, so a
            // query over a multi-document YAML stream yields one result per
            // document (single-document formats have exactly one). A leading
            // `^dN` selects a single document by position; source slices below
            // are gathered in the same per-document order, staying aligned.
            let values = doc.to_values();
            let (selected, body): (Vec<edikt_core::Value>, &edikt_core::Expr) = match &expr {
                edikt_core::Expr::DocSelect(idx, body) => {
                    // An explicitly-named document out of range is an error (like
                    // an out-of-range edit), not a silent empty read.
                    if *idx >= values.len() {
                        bail!(
                            "{loc}: document `^d{idx}` is out of range ({} document{})",
                            values.len(),
                            if values.len() == 1 { "" } else { "s" }
                        );
                    }
                    (values.into_iter().nth(*idx).into_iter().collect(), body)
                }
                _ => (values, &expr),
            };
            let mut out = Vec::new();
            for value in selected {
                out.extend(edikt_core::eval(body, &value).with_context(|| loc.clone())?);
            }
            out
        };

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

        // Comment carrying: a pure-path query also selects from the commented
        // projection, so a structural result re-emits *with* its comments -
        // in the target format's own comment syntax.
        let annotated: Option<Vec<Commented>> = expr
            .as_path()
            .and_then(|p| {
                doc.to_commented()
                    .map(|c| c.descend(p).into_iter().cloned().collect::<Vec<_>>())
            })
            .filter(|a| a.len() == results.len());

        // A synthesized result carries no comments; converting a commented
        // source through one still loses them, and that stays honest. But a
        // comment query (`.foo.#`, `comments`) *surfaces* comments; its result
        // is the comment text, nothing is dropped, so it never warns.
        if target != in_fmt
            && annotated.is_none()
            && !expr.has_comment()
            && doc.has_comments()
            && results
                .iter()
                .any(|r| matches!(r, Value::Array(_) | Value::Object(_)))
        {
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
                annotated.as_ref().map(|a| &a[i]),
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

    // Write the -o sink once, after all inputs, and only if something matched.
    if let Some(p) = &args.output
        && emitted
    {
        std::fs::write(p, terminated(&file_out))
            .with_context(|| format!("writing {}", p.display()))?;
    }

    Ok(if emitted || !args.exit_status {
        // A query miss is a silent no-op by default, like sed with no
        // matching address.
        ExitCode::SUCCESS
    } else {
        // --exit-status: jq-shaped 1 on zero results, for presence tests.
        ExitCode::from(1)
    })
}

/// Render one query result in `target`: scalars raw (JSON-encoded only when
/// JSON was explicitly requested); structural values via the target's emitter,
/// carrying comments when `annotated` selected them, with lossy warnings
/// surfaced (or fatal under --strict) and an infeasible emit turned into an
/// error naming the formats that *can* hold the value.
fn render_value(
    args: &Args,
    value: &Value,
    annotated: Option<&Commented>,
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
    let plain;
    let mut commented = match annotated {
        Some(c) => c,
        None => {
            plain = Commented::from_value(value);
            &plain
        }
    };
    // Feature-derived degradation, same shape as the Comments case below.
    // Every format in the tree spells non-finite numbers in its own grammar
    // (`Infinity`, `.inf`, `inf`, `#inf`; the string-valued ones carry the
    // text), so plain JSON is the only target this fires for - it has no
    // literal, and `JSON.stringify` parity makes that `null`.
    //
    // It rewrites `commented`, not the plain value: this runs after that tree
    // is built, and a comment-carrying selection is emitted from it, so
    // rewriting the `Value` here warned without changing the output.
    let definite;
    if edikt_core::convert::has_non_finite(value)
        && !target.features().contains(&edikt_core::Feature::NonFinite)
    {
        let w = edikt_core::Feature::NonFinite.as_str();
        if args.strict {
            bail!("{loc}: {w} (--strict)");
        }
        eprintln!("edikt: warning: {loc}: {w}");
        definite = commented.strip_non_finite();
        commented = &definite;
    }

    // Feature-derived degradation: a target with no Comments capability (JSON)
    // drops them: warn (or error under --strict), then emit comment-free.
    let stripped;
    if commented.has_comments() && !target.features().contains(&edikt_core::Feature::Comments) {
        let w = "comments were dropped";
        if args.strict {
            bail!("{loc}: {w} (--strict)");
        }
        eprintln!("edikt: warning: {loc}: {w}");
        stripped = Commented::from_value(value);
        commented = &stripped;
    }
    let (text, warnings) = match emit(target, commented) {
        Ok(ok) => ok,
        // A **top-level array** has no representation in a table-only format
        // (TOML/KDL); but the read still wants the value. When the output
        // wasn't explicitly requested, render it as JSON (jq-shaped). This is
        // deliberately narrow: an object that fails to emit for a value-fidelity
        // reason (e.g. a null in TOML) is a real error and falls through below,
        // so `--strict` and a bare read both still surface it.
        Err(_) if !explicit && matches!(value, Value::Array(_)) => {
            let plain = Commented::from_value(value);
            emit(Format::Json, &plain)?
        }
        Err(e) => {
            let needed = edikt_core::convert::features_used(value);
            // A top-level array can only live in a format that allows one at the
            // root; suggesting the target that just failed (or another table-only
            // format) would send the user in a circle.
            let top_array = matches!(value, Value::Array(_));
            let candidates: Vec<&str> = ALL_FORMATS
                .iter()
                .filter(|f| **f != target)
                .filter(|f| needed.iter().all(|n| f.features().contains(n)))
                .filter(|f| !top_array || matches!(f, Format::Json | Format::Jsonc | Format::Yaml))
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
    // `.env` (and `.env.local`, ...) are dotfiles with no extension; match by name.
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
        Some("kdl") => Ok(Format::Kdl),
        Some("md" | "markdown" | "mdx" | "qmd" | "rmd") => Ok(Format::Frontmatter),
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
