//! Line scanner + rowan tree builder for INI.
//!
//! INI is line-oriented and its value grammar is context-sensitive, so a plain
//! regex lexer fights it. Instead we scan line by line and hand-build a lossless
//! rowan tree: every byte lands in a token in order, so the tree round-trips
//! byte-for-byte. Each `Entry`/`Header` node spans its whole line *including* the
//! terminator, which makes line deletion a single `detach`.

use crate::syntax::Sk;
use edikt_syntax::Builder;
use rowan::GreenNode;

/// Scan and build a green tree from INI source.
pub(crate) fn build(src: &str) -> GreenNode {
    let mut b = Builder::new();
    b.start_node(Sk::Root);
    b.start_node(Sk::Section); // preamble section (no header)

    for line in src.split_inclusive('\n') {
        let (content, term) = edikt_core::text::split_ending(line);
        process_line(&mut b, content, term);
    }

    b.finish_node(); // close the last section
    b.finish_node(); // Root
    b.finish()
}

/// Split a line into its content and terminator (`\n`, `\r\n`, or empty).
fn process_line(b: &mut Builder<Sk>, content: &str, term: &str) {
    let rest = content.trim_start();
    let indent = &content[..content.len() - rest.len()];

    // Blank line.
    if rest.is_empty() {
        b.trivia(Sk::Ws, indent);
        b.trivia(Sk::Newline, term);
        return;
    }

    match rest.as_bytes()[0] {
        // Comment line.
        b';' | b'#' => {
            b.trivia(Sk::Ws, indent);
            b.token(Sk::Comment, rest);
            b.trivia(Sk::Newline, term);
        }
        // Section header: close the current section, open a new one.
        b'[' => {
            b.finish_node(); // close current Section
            b.start_node(Sk::Section);
            b.start_node(Sk::Header);
            b.trivia(Sk::Ws, indent);
            build_header(b, rest);
            b.trivia(Sk::Newline, term);
            b.finish_node(); // Header
        }
        // Entry line.
        _ => {
            b.start_node(Sk::Entry);
            b.trivia(Sk::Ws, indent);
            build_entry(b, rest);
            b.trivia(Sk::Newline, term);
            b.finish_node(); // Entry
        }
    }
}

fn build_header(b: &mut Builder<Sk>, rest: &str) {
    // `rest` starts with `[`.
    b.token(Sk::Open, "[");
    let after = &rest[1..];
    if let Some(close) = after.find(']') {
        let name = &after[..close];
        if !name.is_empty() {
            b.token(Sk::Name, name);
        }
        b.token(Sk::Close, "]");
        emit_trailing(b, &after[close + 1..]);
    } else {
        // No closing bracket: keep the bytes so round-trip holds.
        if !after.is_empty() {
            b.token(Sk::Error, after);
        }
    }
}

fn build_entry(b: &mut Builder<Sk>, rest: &str) {
    let Some(sep_idx) = rest.find(['=', ':']) else {
        // No separator: not a valid entry; keep the bytes losslessly.
        b.token(Sk::Error, rest);
        return;
    };

    let key_region = &rest[..sep_idx];
    let key = key_region.trim_end();
    if !key.is_empty() {
        b.token(Sk::Key, key);
    }
    b.trivia(Sk::Ws, &key_region[key.len()..]);

    b.token(Sk::Sep, &rest[sep_idx..sep_idx + 1]);

    let val_region = &rest[sep_idx + 1..];
    // An unquoted `;`/`#` preceded by whitespace starts an inline comment; the
    // value is what comes before it. (A `;`/`#` inside `a;b` stays in the value.)
    let (value_part, comment) = match find_inline_comment(val_region) {
        Some(i) => (&val_region[..i], &val_region[i..]),
        None => (val_region, ""),
    };
    let core = value_part.trim();
    let lead_len = value_part.len() - value_part.trim_start().len();
    b.trivia(Sk::Ws, &value_part[..lead_len]);
    b.start_node(Sk::Value);
    if !core.is_empty() {
        b.token(Sk::ValStr, core);
    }
    b.finish_node(); // Value
    b.trivia(Sk::Ws, &value_part[lead_len + core.len()..]);
    if !comment.is_empty() {
        b.token(Sk::Comment, comment);
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
fn emit_trailing(b: &mut Builder<Sk>, s: &str) {
    let rest = s.trim_start();
    b.trivia(Sk::Ws, &s[..s.len() - rest.len()]);
    if !rest.is_empty() {
        b.token(Sk::Comment, rest);
    }
}
