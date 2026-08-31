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

use crate::syntax::{Sk, sk};
use rowan::{GreenNode, GreenNodeBuilder};

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
    let mut b = GreenNodeBuilder::new();
    b.start_node(sk(Sk::Root));
    for line in src.split_inclusive('\n') {
        let (content, term) = split_terminator(line);
        process_line(&mut b, content, term, dialect);
    }
    b.finish_node(); // Root
    b.finish()
}

fn split_terminator(line: &str) -> (&str, &str) {
    if let Some(rest) = line.strip_suffix("\r\n") {
        (rest, &line[rest.len()..])
    } else if let Some(rest) = line.strip_suffix('\n') {
        (rest, &line[rest.len()..])
    } else {
        (line, "")
    }
}

fn process_line(b: &mut GreenNodeBuilder<'static>, content: &str, term: &str, dialect: Dialect) {
    let rest = content.trim_start();
    let indent = &content[..content.len() - rest.len()];

    // Blank line.
    if rest.is_empty() {
        emit_ws(b, indent);
        emit_newline(b, term);
        return;
    }

    // Comment line (`#` or `!`, the `.properties` comment chars; `.env` uses `#`).
    if matches!(rest.as_bytes()[0], b'#' | b'!') {
        emit_ws(b, indent);
        b.token(sk(Sk::Comment), rest);
        emit_newline(b, term);
        return;
    }

    // Entry line.
    b.start_node(sk(Sk::Entry));
    emit_ws(b, indent);
    build_entry(b, rest, dialect);
    emit_newline(b, term);
    b.finish_node(); // Entry
}

fn build_entry(b: &mut GreenNodeBuilder<'static>, rest: &str, dialect: Dialect) {
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
        b.token(sk(Sk::Error), rest);
        return;
    };

    let key_region = &rest[..sep_idx];
    let key = key_region.trim_end();
    if !key.is_empty() {
        b.token(sk(Sk::Key), key);
    }
    emit_ws(b, &key_region[key.len()..]);

    b.token(sk(Sk::Sep), &rest[sep_idx..sep_idx + sep_len]);

    // Everything after the separator is the value; no inline-comment parsing.
    let val_region = &rest[sep_idx + sep_len..];
    let core = val_region.trim();
    let lead_len = val_region.len() - val_region.trim_start().len();
    emit_ws(b, &val_region[..lead_len]);
    b.start_node(sk(Sk::Value));
    if !core.is_empty() {
        b.token(sk(Sk::ValStr), core);
    }
    b.finish_node(); // Value
    emit_ws(b, &val_region[lead_len + core.len()..]);
}

fn emit_ws(b: &mut GreenNodeBuilder<'static>, ws: &str) {
    if !ws.is_empty() {
        b.token(sk(Sk::Ws), ws);
    }
}

fn emit_newline(b: &mut GreenNodeBuilder<'static>, term: &str) {
    if !term.is_empty() {
        b.token(sk(Sk::Newline), term);
    }
}
