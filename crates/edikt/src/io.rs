//! Reading inputs and writing results: stdin or files in, `-i` in place
//! (with an optional backup), and how an input is named in messages.

use anyhow::{Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Write an in-place result, optionally backing up the pre-edit bytes first
/// (`-i.SUFFIX` -> `PATH.SUFFIX`, sed/perl style; bare `-i` backs nothing up).
pub(crate) fn write_in_place(p: &Path, suffix: &Option<String>, out: &str) -> Result<()> {
    if let Some(suffix) = suffix.as_deref()
        && !suffix.is_empty()
    {
        let mut backup = p.as_os_str().to_os_string();
        backup.push(suffix);
        fs::copy(p, PathBuf::from(&backup)).with_context(|| {
            format!("backing up {} to {}", p.display(), backup.to_string_lossy())
        })?;
    }
    fs::write(p, out).with_context(|| format!("writing {}", p.display()))
}

/// Read each input as (path, contents). No files (or `-`) means stdin.
pub(crate) fn read_inputs(files: &[String]) -> Result<Vec<(Option<PathBuf>, String)>> {
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

pub(crate) fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    Ok(buf)
}

pub(crate) fn display_path(path: Option<&Path>) -> String {
    match path {
        Some(p) => p.display().to_string(),
        None => "<stdin>".to_string(),
    }
}
