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
use std::path::Path;
use std::process::ExitCode;

mod cli;
mod format;
mod io;
mod render;
mod vivify;

use cli::{Args, Directives, join_pipe, munge_args, parse_script};
use format::{detect_format, format_from_name, parse_document};
use io::{display_path, read_inputs, write_in_place};
use render::{render_value, terminated, unknown_function_name, warn};
use vivify::{create_note, scoped_edit, unmatched_targets, would_create};

fn main() -> ExitCode {
    let args = Args::parse_from(munge_args());
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
    edikt_core::check_doc_index(*idx, count)?;
    if count == 1 {
        Ok(Some((**body).clone()))
    } else {
        Ok(None)
    }
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
    if args.in_place.is_some() && !is_mutation && explicit_out.is_none() {
        bail!("in-place (-i) needs a mutating expression or an output format (-T)");
    }

    let inputs = read_inputs(&files)?;

    // With -o, everything accumulates here and is written once at the end
    // (nothing is written on a query miss).
    let mut file_out: Vec<String> = Vec::new();
    let mut emitted = false;
    // An edit through a path expression that matched nothing (#88), for
    // `--exit-status`.
    let mut unmatched_edit = false;
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
                warn(args.strict, &loc, &warnings)?;
            } else {
                // `=` auto-vivifies missing paths (jq-style). Surface every
                // path a plain assignment would create - a mistyped or wrongly
                // scoped path writes a key you didn't intend, and an edit
                // shouldn't do that silently. `--no-vivify` turns any would-be
                // creation into a hard error *before* anything is written.
                //
                // An edit through a path expression (`(.xs[] | select(...) |
                // .n) = v`) that matches nothing is a no-op, and says so:
                // silently doing nothing hides a wrong key as well as
                // silently creating one would.
                let (creating, unmatched) = if scoped_edit(mexpr) {
                    (Vec::<String>::new(), Vec::<String>::new())
                } else {
                    let values = doc.to_values();
                    let mut notes = Vec::new();
                    for (di, path) in would_create(mexpr, &values) {
                        notes.push(create_note(di, values.len(), &path));
                    }
                    (notes, unmatched_targets(mexpr, &values))
                };
                if args.no_vivify
                    && let Some(note) = creating.first()
                {
                    bail!("{loc}: {note}; not auto-creating (--no-vivify)");
                }
                let warnings = doc.apply(mexpr).with_context(|| loc.clone())?;
                for note in &creating {
                    eprintln!("edikt: note: {loc}: {note}");
                }
                for lhs in &unmatched {
                    eprintln!("edikt: note: {loc}: `{lhs}` matched nothing; no change");
                }
                unmatched_edit |= !unmatched.is_empty();
                warn(args.strict, &loc, &warnings)?;
            }
            let out = doc.to_source();
            if args.in_place.is_some() {
                let p = path
                    .as_ref()
                    .context("cannot edit stdin in place; pass a file")?;
                write_in_place(p, &args.in_place, &out)?;
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
                    edikt_core::check_doc_index(*idx, all.len()).with_context(|| loc.clone())?;
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
                    edikt_core::check_doc_index(*idx, values.len()).with_context(|| loc.clone())?;
                    (values.into_iter().nth(*idx).into_iter().collect(), body)
                }
                _ => (values, &expr),
            };
            let mut out = Vec::new();
            for value in selected {
                let evaluated = edikt_core::eval(body, &value).map_err(|e| {
                    // The CLI is the only layer holding both the expression
                    // source and the evaluation error, so the hyphenated-key
                    // hint for a bare `.dev-dependencies` is composed here
                    // (jhheider/edikt#63). The helper self-limits to the case
                    // where the unknown function is the key's own tail.
                    match unknown_function_name(&e.to_string()).and_then(|name| {
                        edikt_core::hyphen_hint_for_unknown_function(&program, name)
                    }) {
                        Some(hint) => anyhow::anyhow!("{e}; {hint}"),
                        None => anyhow::Error::new(e),
                    }
                });
                out.extend(evaluated.with_context(|| loc.clone())?);
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
            warn(args.strict, &loc, &["comments were dropped"])?;
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

        if args.in_place.is_some() {
            let p = path
                .as_ref()
                .context("cannot convert stdin in place; pass a file")?;
            let joined = terminated(&outputs);
            write_in_place(p, &args.in_place, &joined)?;
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

    if args.exit_status && unmatched_edit {
        // --exit-status: an edit whose path expression matched nothing is
        // the mutation analogue of a query miss.
        return Ok(ExitCode::from(1));
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
