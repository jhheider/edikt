//! Line scanner + rowan tree builder for `.env` / `.properties`.
//!
//! Flat and honest: `key=value` / `key:value` entries, `#`/`!` comment lines,
//! blanks. No sections. **No inline comments and no interpolation** — a value is
//! the raw text after the separator (trimmed for the projected value, preserved
//! verbatim for round-trip). Each `Entry` spans its whole line including the
//! terminator, so deletion is a single `detach`.

use crate::syntax::{Sk, sk};
use rowan::{GreenNode, GreenNodeBuilder};

pub(crate) fn build(src: &str) -> GreenNode {
    let mut b = GreenNodeBuilder::new();
    b.start_node(sk(Sk::Root));
    for line in src.split_inclusive('\n') {
        let (content, term) = split_terminator(line);
        process_line(&mut b, content, term);
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

fn process_line(b: &mut GreenNodeBuilder<'static>, content: &str, term: &str) {
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
    build_entry(b, rest);
    emit_newline(b, term);
    b.finish_node(); // Entry
}

fn build_entry(b: &mut GreenNodeBuilder<'static>, rest: &str) {
    let Some(sep_idx) = rest.find(['=', ':']) else {
        // No separator — keep the bytes losslessly, but flag it as malformed.
        b.token(sk(Sk::Error), rest);
        return;
    };

    let key_region = &rest[..sep_idx];
    let key = key_region.trim_end();
    if !key.is_empty() {
        b.token(sk(Sk::Key), key);
    }
    emit_ws(b, &key_region[key.len()..]);

    b.token(sk(Sk::Sep), &rest[sep_idx..sep_idx + 1]);

    // Everything after the separator is the value — no inline-comment parsing.
    let val_region = &rest[sep_idx + 1..];
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
