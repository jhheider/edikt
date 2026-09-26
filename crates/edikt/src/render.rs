//! Rendering query results: raw scalars, structural values through the
//! target's emitter with comments carried, and the warnings (or `--strict`
//! errors) a lossy rendering owes.

use crate::cli::Args;
use crate::format::{ALL_FORMATS, Format, emit};
use anyhow::{Result, bail};
use edikt_core::{Commented, Value};

/// Surface non-fatal warnings (a lossy conversion, a remapped comment): one
/// stderr line each, or, under `--strict`, all of them as the error.
pub(crate) fn warn(strict: bool, loc: &str, warnings: &[impl AsRef<str>]) -> Result<()> {
    if warnings.is_empty() {
        return Ok(());
    }
    if strict {
        let all: Vec<&str> = warnings.iter().map(AsRef::as_ref).collect();
        bail!("{loc}: {} (--strict)", all.join("; "));
    }
    for w in warnings {
        eprintln!("edikt: warning: {loc}: {}", w.as_ref());
    }
    Ok(())
}

/// Render one query result in `target`: scalars raw (JSON-encoded only when
/// JSON was explicitly requested); structural values via the target's emitter,
/// carrying comments when `annotated` selected them, with lossy warnings
/// surfaced (or fatal under --strict) and an infeasible emit turned into an
/// error naming the formats that *can* hold the value.
pub(crate) fn render_value(
    args: &Args,
    value: &Value,
    annotated: Option<&Commented>,
    target: Format,
    explicit: bool,
    loc: &str,
) -> Result<String> {
    // A non-finite number (`Infinity`/`-Infinity`/`NaN`) exists only in JSON5;
    // strict JSON has no literal for it, so encoding one there degrades it to
    // `null`. That drop must be visible (a warning, fatal under --strict), and
    // the JSONC/JSON5 family keeps the number instead (the emitters use the
    // JSON5 spelling). Only reachable when the source carried one.
    let nonfinite = target == Format::Json && edikt_core::convert::contains_non_finite(value);
    if nonfinite {
        let w =
            "non-finite numbers (Infinity/NaN) were encoded as null; JSON has no literal for them";
        warn(args.strict, loc, &[w])?;
    }
    if !matches!(value, Value::Array(_) | Value::Object(_)) {
        // Scalars are raw text, except an explicitly-requested JSON-family
        // output JSON-encodes them (strings quoted) for machine consumers.
        return Ok(if explicit {
            match target {
                // Strict JSON can't hold a non-finite; it's already warned.
                Format::Json => value.to_json(),
                // JSONC/JSON5 keep the literal.
                Format::Jsonc => value.to_json5(),
                _ => value.to_raw_string(),
            }
        } else {
            value.to_raw_string()
        });
    }
    let plain;
    let mut commented = match annotated {
        Some(c) => c,
        None => {
            plain = Commented::from_value(value);
            &plain
        }
    };
    // Duplicate keys are legal in the flat key-value family (a second `Port 22`
    // line is ordinary in a daemon config) and are a map collision everywhere
    // else, so any target but that family collapses them and says which key.
    // Keyed on the target rather than on "is this a conversion", because
    // `-T env` on a duplicated document should keep both lines. Same shape as
    // the Comments degradation below.
    //
    // It rewrites `commented`, not the plain value: a comment-carrying
    // selection is emitted from the annotated tree, so deduping the `Value`
    // alone warned and changed nothing in the output.
    let deduped;
    if !matches!(target, Format::Env | Format::EnvSpaced)
        && let Some(key) = edikt_core::convert::duplicate_key(value)
    {
        let w = format!("duplicate key `{key}` collapsed (kept the first)");
        warn(args.strict, loc, &[w])?;
        deduped = commented.dedupe_keys();
        commented = &deduped;
    }

    // Feature-derived degradation: a target with no Comments capability (JSON)
    // drops them: warn (or error under --strict), then emit comment-free.
    let stripped;
    if commented.has_comments() && !target.features().contains(&edikt_core::Feature::Comments) {
        warn(args.strict, loc, &["comments were dropped"])?;
        stripped = Commented::from_value(value);
        commented = &stripped;
    }
    // Non-finite numbers degrade to `null` only under strict JSON (warned
    // above); swap the nulled value in so the shared emitter never writes an
    // `Infinity` literal into a `.json` target.
    let jsoned;
    if nonfinite {
        jsoned = Commented::from_value(&edikt_core::convert::nullify_non_finite(value));
        commented = &jsoned;
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
    warn(args.strict, loc, &warnings)?;
    Ok(text)
}

/// The function name out of an `unknown function `X`` evaluation error.
///
/// Reading it back out of the rendered message keeps `EvalError` a plain string
/// error; a typed variant would be the cleaner seam if more callers ever need
/// this, but one caller does not earn the churn.
pub(crate) fn unknown_function_name(msg: &str) -> Option<&str> {
    msg.strip_prefix("unknown function `")?.strip_suffix('`')
}

/// Join outputs, newline-terminating each (for `-i` writes).
pub(crate) fn terminated(outputs: &[String]) -> String {
    let mut joined = String::new();
    for out in outputs {
        joined.push_str(out);
        if !out.ends_with('\n') {
            joined.push('\n');
        }
    }
    joined
}
