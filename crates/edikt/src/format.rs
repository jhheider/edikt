//! The formats: names and aliases, detection from a file name, and the
//! per-format parse and emit dispatch.

use anyhow::{Result, bail};
use edikt_core::{Commented, Document};
use std::path::Path;

/// The supported formats. `Json` shares JSONC's engine (JSON is a subset) but is
/// a distinct format: it has no `Comments` capability, so JSONC -> JSON is a real
/// conversion that (warns and) drops comments.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Json,
    Jsonc,
    Ini,
    Env,
    /// The `.env` model with a whitespace separator (`Port 22`): sshd_config
    /// and other space-separated daemon configs. Same flat, string-valued
    /// document, so it shares `edikt-env` entirely.
    EnvSpaced,
    Toml,
    Yaml,
    Kdl,
    /// The frontmatter lens: edit the metadata block of a Markdown file, body
    /// left opaque. An input-only format; never a conversion target.
    Frontmatter,
}

/// Plain JSON's capabilities: everything JSONC has except comments.
pub(crate) const JSON_FEATURES: &[edikt_core::Feature] = &[
    edikt_core::Feature::Nesting,
    edikt_core::Feature::Arrays,
    edikt_core::Feature::TypedScalars,
];

impl Format {
    /// The format's capability set (used to pick candidate output formats).
    pub(crate) fn features(self) -> &'static [edikt_core::Feature] {
        match self {
            Format::Json => JSON_FEATURES,
            Format::Jsonc => edikt_jsonc::FEATURES,
            Format::Ini => edikt_ini::FEATURES,
            Format::Env | Format::EnvSpaced => edikt_env::FEATURES,
            Format::Toml => edikt_toml::FEATURES,
            Format::Yaml => edikt_yaml::FEATURES,
            Format::Kdl => edikt_kdl::FEATURES,
            Format::Frontmatter => edikt_frontmatter::FEATURES,
        }
    }
    /// The canonical name, for messages.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Jsonc => "jsonc",
            Format::Ini => "ini",
            Format::Env => "env",
            Format::EnvSpaced => "envspaced",
            Format::Toml => "toml",
            Format::Yaml => "yaml",
            Format::Kdl => "kdl",
            Format::Frontmatter => "frontmatter",
        }
    }
}

/// All formats, for candidate suggestions.
pub(crate) const ALL_FORMATS: [Format; 8] = [
    Format::Jsonc,
    Format::Json,
    Format::Ini,
    Format::Env,
    Format::EnvSpaced,
    Format::Toml,
    Format::Yaml,
    Format::Kdl,
];

/// Whether a [`FORMAT_ALIASES`] name is also a file extension that
/// auto-detects, or a `-t`/`-T` name only.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Alias {
    /// A `-t`/`-T` name and a detected extension (`.yml`).
    Ext,
    /// A `-t`/`-T` name only: `envspaced` is never auto-detected, and
    /// `frontmatter`/`fm` are not extensions.
    Name,
}

/// The one table of format names: `-t`/`-T` accept every entry, extension
/// detection accepts the [`Alias::Ext`] ones, and error messages list them
/// all. Matching is case-insensitive in both places.
pub(crate) const FORMAT_ALIASES: &[(&str, Format, Alias)] = &[
    ("jsonc", Format::Jsonc, Alias::Ext),
    ("json5", Format::Jsonc, Alias::Ext),
    ("json", Format::Json, Alias::Ext),
    ("ini", Format::Ini, Alias::Ext),
    ("cfg", Format::Ini, Alias::Ext),
    ("conf", Format::Ini, Alias::Ext),
    ("env", Format::Env, Alias::Ext),
    ("properties", Format::Env, Alias::Ext),
    ("props", Format::Env, Alias::Ext),
    ("envspaced", Format::EnvSpaced, Alias::Name),
    ("spaced", Format::EnvSpaced, Alias::Name),
    ("toml", Format::Toml, Alias::Ext),
    ("yaml", Format::Yaml, Alias::Ext),
    ("yml", Format::Yaml, Alias::Ext),
    ("kdl", Format::Kdl, Alias::Ext),
    ("markdown", Format::Frontmatter, Alias::Ext),
    ("md", Format::Frontmatter, Alias::Ext),
    ("mdx", Format::Frontmatter, Alias::Ext),
    ("qmd", Format::Frontmatter, Alias::Ext),
    ("rmd", Format::Frontmatter, Alias::Ext),
    ("frontmatter", Format::Frontmatter, Alias::Name),
    ("fm", Format::Frontmatter, Alias::Name),
];

/// Every format name accepted by `-t`/`-T`, for error messages.
pub(crate) fn format_names() -> String {
    FORMAT_ALIASES
        .iter()
        .map(|(name, _, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Look `name` up in [`FORMAT_ALIASES`], case-insensitively; `ext_only`
/// restricts the match to names that are also file extensions.
pub(crate) fn lookup_format(name: &str, ext_only: bool) -> Option<Format> {
    FORMAT_ALIASES
        .iter()
        .find(|(n, _, kind)| n.eq_ignore_ascii_case(name) && (!ext_only || *kind == Alias::Ext))
        .map(|(_, f, _)| *f)
}

/// Resolve a `-t`/`-T` format name.
pub(crate) fn format_from_name(name: &str) -> Result<Format> {
    lookup_format(name, false).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown format `{}` (expected one of: {})",
            name.to_ascii_lowercase(),
            format_names()
        )
    })
}

pub(crate) fn detect_format(path: Option<&Path>, forced: Option<&str>) -> Result<Format> {
    if let Some(t) = forced {
        return format_from_name(t);
    }
    // `.env` (and `.env.local`, ...) are dotfiles with no extension; match by
    // name, ignoring case like extension detection does.
    if let Some(name) = path.and_then(|p| p.file_name()).and_then(|n| n.to_str())
        && let name = name.to_ascii_lowercase()
        && (name == ".env" || name.starts_with(".env."))
    {
        return Ok(Format::Env);
    }
    // envspaced is deliberately NOT auto-detected: `sshd_config` has no
    // extension, `.conf` already means INI, and a `key value` line is
    // indistinguishable from a malformed `.env` line. Guessing would silently
    // edit the wrong bytes, so it is `-t envspaced` or nothing.
    match path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        Some(ext) => lookup_format(ext, true).ok_or_else(|| {
            anyhow::anyhow!(
                "cannot infer format from `.{ext}`; pass -t (one of: {})",
                format_names()
            )
        }),
        None => bail!(
            "cannot infer format (no extension); pass -t (one of: {})",
            format_names()
        ),
    }
}

/// Parse `src` in the given format into a boxed, format-agnostic document.
pub(crate) fn parse_document(format: Format, src: &str) -> Result<Box<dyn Document>> {
    Ok(match format {
        // JSON is read by the JSONC parser (it's a subset with no comments).
        Format::Json | Format::Jsonc => Box::new(edikt_jsonc::parse(src)?),
        Format::Ini => Box::new(edikt_ini::parse(src)?),
        Format::Env => Box::new(edikt_env::parse(src)?),
        Format::EnvSpaced => Box::new(edikt_env::parse_spaced(src)?),
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
pub(crate) fn emit(format: Format, c: &Commented) -> Result<(String, Vec<String>)> {
    Ok(match format {
        Format::Json | Format::Jsonc => (edikt_jsonc::emit_commented(c), Vec::new()),
        Format::Ini => edikt_ini::emit_commented(c)?,
        Format::Env => edikt_env::emit_commented(c)?,
        Format::EnvSpaced => edikt_env::emit_commented_with(c, edikt_env::Dialect::Spaced)?,
        Format::Toml => edikt_toml::emit_commented(c)?,
        Format::Yaml => edikt_yaml::emit_commented(c)?,
        Format::Kdl => edikt_kdl::emit_commented(c)?,
        Format::Frontmatter => bail!(
            "cannot convert to `frontmatter`: it is an input lens over a Markdown \
             block, not an output format; use -T with the block's own format (yaml, toml, json)"
        ),
    })
}
