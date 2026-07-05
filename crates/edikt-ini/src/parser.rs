//! Line scanner + rowan tree builder for INI.
//!
//! INI is line-oriented and its value grammar is context-sensitive, so a plain
//! regex lexer fights it. Instead we scan line by line and hand-build a lossless
//! rowan tree: every byte lands in a token in order, so the tree round-trips
//! byte-for-byte. Each `Entry`/`Header` node spans its whole line *including* the
//! terminator, which makes line deletion a single `detach`.

use crate::syntax::{Sk, sk};
use rowan::{GreenNode, GreenNodeBuilder};

/// Scan and build a green tree from INI source.
pub(crate) fn build(src: &str) -> GreenNode {
    let mut b = GreenNodeBuilder::new();
    b.start_node(sk(Sk::Root));
    b.start_node(sk(Sk::Section)); // preamble section (no header)

    for line in src.split_inclusive('\n') {
        let (content, term) = split_terminator(line);
        process_line(&mut b, content, term);
    }

    b.finish_node(); // close the last section
    b.finish_node(); // Root
    b.finish()
}

/// Split a line into its content and terminator (`\n`, `\r\n`, or empty).
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

    match rest.as_bytes()[0] {
        // Comment line.
        b';' | b'#' => {
            emit_ws(b, indent);
            b.token(sk(Sk::Comment), rest);
            emit_newline(b, term);
        }
        // Section header — close the current section, open a new one.
        b'[' => {
            b.finish_node(); // close current Section
            b.start_node(sk(Sk::Section));
            b.start_node(sk(Sk::Header));
            emit_ws(b, indent);
            build_header(b, rest);
            emit_newline(b, term);
            b.finish_node(); // Header
        }
        // Entry line.
        _ => {
            b.start_node(sk(Sk::Entry));
            emit_ws(b, indent);
            build_entry(b, rest);
            emit_newline(b, term);
            b.finish_node(); // Entry
        }
    }
}

fn build_header(b: &mut GreenNodeBuilder<'static>, rest: &str) {
    // `rest` starts with `[`.
    b.token(sk(Sk::Open), "[");
    let after = &rest[1..];
    if let Some(close) = after.find(']') {
        let name = &after[..close];
        if !name.is_empty() {
            b.token(sk(Sk::Name), name);
        }
        b.token(sk(Sk::Close), "]");
        emit_trailing(b, &after[close + 1..]);
    } else {
        // No closing bracket — keep the bytes so round-trip holds.
        if !after.is_empty() {
            b.token(sk(Sk::Error), after);
        }
    }
}

fn build_entry(b: &mut GreenNodeBuilder<'static>, rest: &str) {
    let Some(sep_idx) = rest.find(['=', ':']) else {
        // No separator — not a valid entry; keep the bytes losslessly.
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

    let val_region = &rest[sep_idx + 1..];
    // An unquoted `;`/`#` preceded by whitespace starts an inline comment; the
    // value is what comes before it. (A `;`/`#` inside `a;b` stays in the value.)
    let (value_part, comment) = match find_inline_comment(val_region) {
        Some(i) => (&val_region[..i], &val_region[i..]),
        None => (val_region, ""),
    };
    let core = value_part.trim();
    let lead_len = value_part.len() - value_part.trim_start().len();
    emit_ws(b, &value_part[..lead_len]);
    b.start_node(sk(Sk::Value));
    if !core.is_empty() {
        b.token(sk(Sk::ValStr), core);
    }
    b.finish_node(); // Value
    emit_ws(b, &value_part[lead_len + core.len()..]);
    if !comment.is_empty() {
        b.token(sk(Sk::Comment), comment);
    }
}

/// The byte index of an inline comment (`;`/`#` preceded by whitespace or at the
/// start of the value region), if any.
fn find_inline_comment(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    (0..bytes.len()).find(|&i| {
        matches!(bytes[i], b';' | b'#') && (i == 0 || bytes[i - 1].is_ascii_whitespace())
    })
}

/// Emit trailing content after a `]` (whitespace + optional comment).
fn emit_trailing(b: &mut GreenNodeBuilder<'static>, s: &str) {
    let rest = s.trim_start();
    emit_ws(b, &s[..s.len() - rest.len()]);
    if !rest.is_empty() {
        b.token(sk(Sk::Comment), rest);
    }
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
