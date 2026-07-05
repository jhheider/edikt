//! Syntax kinds and the rowan `Language` for `.env` / `.properties`.

use rowan::Language;

/// Token and node kinds. Tokens first, then nodes; discriminants contiguous
/// from 0 so [`EnvLang::kind_from_raw`] can index a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum Sk {
    // tokens
    Ws,
    Newline,
    Comment,
    Key,
    Sep,    // `=` or `:`
    ValStr, // value text (trimmed core)
    Error,
    // nodes
    Value, // wraps an entry's value text (possibly empty)
    Entry, // one `key=value` line, including its terminator
    Root,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EnvLang {}

impl Language for EnvLang {
    type Kind = Sk;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Sk {
        const KINDS: [Sk; 10] = [
            Sk::Ws,
            Sk::Newline,
            Sk::Comment,
            Sk::Key,
            Sk::Sep,
            Sk::ValStr,
            Sk::Error,
            Sk::Value,
            Sk::Entry,
            Sk::Root,
        ];
        KINDS[raw.0 as usize]
    }

    fn kind_to_raw(kind: Sk) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<EnvLang>;

pub(crate) fn sk(kind: Sk) -> rowan::SyntaxKind {
    rowan::SyntaxKind(kind as u16)
}
