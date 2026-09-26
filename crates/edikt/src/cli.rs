//! The command line: clap arguments, sed's `-i.SUFFIX` spelling, and `-f`
//! script files (header directives, body, joining sources).

use crate::format::{Format, format_from_name};
use anyhow::{Result, bail};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "edikt",
    version,
    about = "Lossless, format-preserving config editor: JSONC, INI, .env, TOML, YAML, KDL, Markdown frontmatter.",
    long_about = "Query and losslessly edit JSONC/JSON5, INI, .env/.properties, TOML, \
YAML, KDL, and the frontmatter of Markdown files and PEP 723 scripts with a \
jq-flavored expression language, changing only the bytes you \
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
  edikt -i 'select(.kind==\"Service\") | .x = 1' k8s.yaml   target docs by content
  edikt -i '(.npcs[] | select(.id==\"b\") | .hp) = 5' npcs.yaml
                                                        edit a list item by key
  edikt 'path(.npcs[] | select(.id==\"b\"))' npcs.yaml     which paths would change"
)]
pub(crate) struct Args {
    /// Expression, then files. With -e/-f present, ALL operands are files.
    #[arg(value_name = "EXPR|FILE")]
    pub(crate) operands: Vec<String>,

    /// Expression to apply (repeatable). Composes with -f in order.
    #[arg(short = 'e', long = "expr", value_name = "EXPR")]
    pub(crate) exprs: Vec<String>,

    /// Read an expression/script from a file (repeatable). A script may open
    /// with header directives (`toFormat: FMT` / `type: FMT`) which CLI flags
    /// override; `#` header lines (comments, shebangs) are skipped.
    #[arg(short = 'f', long = "file", value_name = "PATH")]
    pub(crate) script_files: Vec<PathBuf>,

    /// Edit files in place (requires a mutating expression or a conversion -T).
    /// `-i.SUFFIX` keeps a backup of the pre-edit file as `FILE.SUFFIX`
    /// (sed/perl style); bare `-i` does not.
    #[arg(
        short = 'i',
        long = "in-place",
        value_name = "SUFFIX",
        num_args = 0..=1,
        default_missing_value = "",
        require_equals = true,
    )]
    pub(crate) in_place: Option<String>,

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
    pub(crate) output: Option<PathBuf>,

    /// Force the input format: jsonc | json5 | json | ini | env | properties | envspaced | toml | yaml | kdl | markdown
    /// (aliases: cfg, conf, props, spaced, yml, md, mdx, qmd, rmd, frontmatter, fm; any case).
    #[arg(short = 't', long = "type", value_name = "FMT")]
    pub(crate) format: Option<String>,

    /// Output format. Default: the input format is preserved. (`-T FMT` and the
    /// `--json`/`--jsonc`/`--ini`/`--toml`/`--yaml`/`--kdl` shorthands are equivalent.)
    #[arg(short = 'T', long = "to", value_name = "FMT", group = "outfmt")]
    pub(crate) to: Option<String>,

    /// Output as JSON (shorthand for `-T json`).
    #[arg(long, group = "outfmt")]
    pub(crate) json: bool,
    /// Output as JSONC (shorthand for `-T jsonc`).
    #[arg(long, group = "outfmt")]
    pub(crate) jsonc: bool,
    /// Output as INI (shorthand for `-T ini`).
    #[arg(long, group = "outfmt")]
    pub(crate) ini: bool,
    /// Output as TOML (shorthand for `-T toml`).
    #[arg(long, group = "outfmt")]
    pub(crate) toml: bool,
    /// Output as YAML (shorthand for `-T yaml`).
    #[arg(long, group = "outfmt")]
    pub(crate) yaml: bool,
    /// Output as KDL (shorthand for `-T kdl`).
    #[arg(long, group = "outfmt")]
    pub(crate) kdl: bool,

    /// When the output format differs from the input, treat lossy degradations
    /// (dropped comments, flattening) as errors instead of warnings.
    #[arg(long)]
    pub(crate) strict: bool,

    /// Don't auto-create missing keys: an assignment whose path doesn't
    /// already exist fails (exit 2) instead of creating it. Plain `=` is
    /// jq-style and creates by default (with a stderr note); this opts out so
    /// a mistyped or wrongly-scoped path can't silently write a new key.
    /// A `select(...)` inside an assignment path is checked like any path;
    /// a document-level `select(`/`^dN` scope is unaffected. `|=`/`+=`
    /// already error on a missing path and `del` stays a no-op.
    #[arg(long)]
    pub(crate) no_vivify: bool,

    /// Output raw scalars (the default; explicit opt-in).
    #[arg(short = 'r', long, conflicts_with = "outfmt")]
    pub(crate) raw: bool,

    /// Exit 1 when a query produces no results (jq-style, for presence
    /// tests), or when an edit through a path expression
    /// (`(.xs[] | select(...) | .n) = v`) matched nothing. The default is
    /// sed-shaped: a miss is a no-op, exit 0 (an unmatched edit still notes
    /// it on stderr).
    #[arg(long = "exit-status")]
    pub(crate) exit_status: bool,

    /// Print shell completions to stdout (for packagers; bash|zsh|fish|...).
    #[arg(long, value_name = "SHELL", hide = true)]
    pub(crate) completions: Option<clap_complete::Shell>,

    /// Print the man page (roff) to stdout (for packagers).
    #[arg(long, hide = true)]
    pub(crate) manpage: bool,
}

impl Args {
    /// The explicitly-requested output format, if any (`-T` or a `--fmt`
    /// shorthand). `None` means "preserve the input format".
    pub(crate) fn output_format(&self) -> Result<Option<Format>> {
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

/// Normalize sed/perl's `-i.SUFFIX` spelling before clap parses. clap's
/// optional-value flags (`num_args 0..=1`) greedily consume the *next operand*
/// when a value isn't attached, which would swallow the expression in the
/// headline `edikt -i '.expr' file` form. Rather than lose that, we take
/// `require_equals = true` (so `-i` alone never eats a token) and map the
/// classic attached spelling onto `-i=SUFFIX` here.
pub(crate) fn munge_args() -> Vec<String> {
    // Only the option region is rewritten: everything at or after a `--`
    // terminator is positional (a file name), so an operand genuinely named
    // `-ifoo` stays intact.
    let raw = std::env::args().collect::<Vec<_>>();
    let cut = raw.iter().position(|a| a == "--").unwrap_or(raw.len());
    let mut out: Vec<String> = raw[..cut]
        .iter()
        .map(|a| {
            if let Some(rest) = a.strip_prefix("-i")
                && !rest.is_empty()
                && !rest.starts_with(['-', '='])
            {
                format!("-i={rest}")
            } else {
                a.clone()
            }
        })
        .collect();
    for a in &raw[cut..] {
        out.push(a.clone());
    }
    out
}

/// Directives a script file may declare in a leading header, before its first
/// expression. CLI flags override; across multiple `-f` files the last wins.
#[derive(Default)]
pub(crate) struct Directives {
    pub(crate) to: Option<String>,
    pub(crate) ty: Option<String>,
}

/// Split a script into (header directives, body). The header is any run of
/// blank lines, `#` comments (so shebang scripts work), and `key: value`
/// directive lines (`toFormat:`/`to:`, `type:`/`from:`); the body starts at the
/// first line that is none of those and is passed to the parser verbatim.
pub(crate) fn parse_script(src: &str) -> (Directives, String) {
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

pub(crate) fn join_pipe(sources: &[String]) -> String {
    sources
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}
