//! Syntax kinds and the rowan `Language` for INI.

use rowan::Language;

/// Token and node kinds. Tokens first, then nodes; discriminants are contiguous
/// from 0 so [`IniLang::kind_from_raw`] can index a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum Sk {
    // tokens
    Ws,
    Newline,
    Comment,
    Open,   // `[`
    Close,  // `]`
    Name,   // section name
    Key,    // entry key
    Sep,    // `=` or `:`
    ValStr, // entry value text (trimmed core)
    Error,
    // nodes
    Value,   // wraps the value text of an entry (possibly empty)
    Entry,   // one `key = value` line, including its terminator
    Header,  // one `[section]` line, including its terminator
    Section, // a header (optional, absent for the preamble) plus its entries/trivia
    Root,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IniLang {}

impl Language for IniLang {
    type Kind = Sk;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Sk {
        const KINDS: [Sk; 15] = [
            Sk::Ws,
            Sk::Newline,
            Sk::Comment,
            Sk::Open,
            Sk::Close,
            Sk::Name,
            Sk::Key,
            Sk::Sep,
            Sk::ValStr,
            Sk::Error,
            Sk::Value,
            Sk::Entry,
            Sk::Header,
            Sk::Section,
            Sk::Root,
        ];
        KINDS[raw.0 as usize]
    }

    fn kind_to_raw(kind: Sk) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<IniLang>;

pub(crate) fn sk(kind: Sk) -> rowan::SyntaxKind {
    rowan::SyntaxKind(kind as u16)
}
