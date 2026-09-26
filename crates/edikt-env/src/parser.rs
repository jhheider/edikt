//! Line scanner + rowan tree builder for `.env` / `.properties` / `envspaced`.
//!
//! Flat and honest: `key=value` / `key:value` entries, `#`/`!` comment lines,
//! blanks. No sections. **No inline comments and no interpolation**: a value is
//! the raw text after the separator (trimmed for the projected value, preserved
//! verbatim for round-trip). Each `Entry` spans its whole line including the
//! terminator, so deletion is a single `detach`.
//!
//! [`Dialect`] picks only how the key ends. Everything downstream - trivia,
//! comments, the `Value` slot, deletion, round-trip - is shared, because the
//! separator is the entire difference between `PORT=22` and `Port 22`.

use crate::syntax::Sk;
use edikt_syntax::Builder;
use rowan::GreenNode;

/// Which separator spelling a document uses.
///
/// Not auto-detected: a `key value` line is indistinguishable from a malformed
/// `.env` line, and guessing wrong would silently edit the wrong bytes. The
/// caller states it, exactly as `-t` does for every other format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    /// `.env` / `.properties`: the first `=` or `:` ends the key.
    #[default]
    Punctuated,
    /// `envspaced`: the first run of spaces or tabs ends the key
    /// (`sshd_config`, `ssh_config`, `zoo.cfg`-adjacent daemon configs).
    ///
    /// Deliberately *not* an ssh_config parser: `Match` / `Host` blocks scope
    /// the keys beneath them, and this model is flat, so a file using them is
    /// out of scope rather than half-supported.
    Spaced,
}

pub(crate) fn build(src: &str, dialect: Dialect) -> GreenNode {
    let mut b = Builder::new();
    b.start_node(Sk::Root);
    for line in src.split_inclusive('\n') {
        let (content, term) = edikt_core::text::split_ending(line);
        process_line(&mut b, content, term, dialect);
    }
    b.finish_node(); // Root
    b.finish()
}

fn process_line(b: &mut Builder<Sk>, content: &str, term: &str, dialect: Dialect) {
    let rest = content.trim_start();
    let indent = &content[..content.len() - rest.len()];

    // Blank line.
    if rest.is_empty() {
        b.trivia(Sk::Ws, indent);
        b.trivia(Sk::Newline, term);
        return;
    }

    // Comment line (`#` or `!`, the `.properties` comment chars; `.env` uses `#`).
    if matches!(rest.as_bytes()[0], b'#' | b'!') {
        b.trivia(Sk::Ws, indent);
        b.token(Sk::Comment, rest);
        b.trivia(Sk::Newline, term);
        return;
    }

    // Entry line.
    b.start_node(Sk::Entry);
    b.trivia(Sk::Ws, indent);
    build_entry(b, rest, dialect);
    b.trivia(Sk::Newline, term);
    b.finish_node(); // Entry
}

fn build_entry(b: &mut Builder<Sk>, rest: &str, dialect: Dialect) {
    // (start of separator, length). For the spaced dialect the separator IS the
    // whitespace run, so it has no leading gap of its own to emit.
    let found = match dialect {
        Dialect::Punctuated => rest.find(['=', ':']).map(|i| (i, 1)),
        Dialect::Spaced => rest.find([' ', '\t']).map(|i| {
            let len = rest[i..]
                .find(|c| c != ' ' && c != '\t')
                .unwrap_or(rest.len() - i);
            (i, len)
        }),
    };
    let Some((sep_idx, sep_len)) = found else {
        // No separator: keep the bytes losslessly, but flag it as malformed.
        b.token(Sk::Error, rest);
        return;
    };

    let key_region = &rest[..sep_idx];
    let key = key_region.trim_end();
    if !key.is_empty() {
        b.token(Sk::Key, key);
    }
    b.trivia(Sk::Ws, &key_region[key.len()..]);

    b.token(Sk::Sep, &rest[sep_idx..sep_idx + sep_len]);

    // Everything after the separator is the value; no inline-comment parsing.
    let val_region = &rest[sep_idx + sep_len..];
    let core = val_region.trim();
    let lead_len = val_region.len() - val_region.trim_start().len();
    b.trivia(Sk::Ws, &val_region[..lead_len]);
    b.start_node(Sk::Value);
    if !core.is_empty() {
        b.token(Sk::ValStr, core);
    }
    b.finish_node(); // Value
    b.trivia(Sk::Ws, &val_region[lead_len + core.len()..]);
}
