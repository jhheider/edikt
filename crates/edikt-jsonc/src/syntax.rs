//! Syntax kinds and the rowan `Language` for JSONC.

use rowan::Language;

/// Token and node kinds. Tokens come first, then composite nodes; discriminants
/// are contiguous from 0 so [`JsoncLang::kind_from_raw`] can index a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum Sk {
    // tokens
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    True,
    False,
    Null,
    Str,
    Num,
    LineComment,
    BlockComment,
    Ws,
    Error,
    // nodes
    Value,
    Object,
    Member,
    Array,
    Root,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JsoncLang {}

impl Language for JsoncLang {
    type Kind = Sk;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Sk {
        const KINDS: [Sk; 20] = [
            Sk::LBrace,
            Sk::RBrace,
            Sk::LBracket,
            Sk::RBracket,
            Sk::Colon,
            Sk::Comma,
            Sk::True,
            Sk::False,
            Sk::Null,
            Sk::Str,
            Sk::Num,
            Sk::LineComment,
            Sk::BlockComment,
            Sk::Ws,
            Sk::Error,
            Sk::Value,
            Sk::Object,
            Sk::Member,
            Sk::Array,
            Sk::Root,
        ];
        KINDS[raw.0 as usize]
    }

    fn kind_to_raw(kind: Sk) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<JsoncLang>;

/// Raw kind for the green-tree builder.
pub(crate) fn sk(kind: Sk) -> rowan::SyntaxKind {
    rowan::SyntaxKind(kind as u16)
}

/// Is this kind trivia (whitespace or a comment)?
pub(crate) fn is_trivia(kind: Sk) -> bool {
    matches!(kind, Sk::Ws | Sk::LineComment | Sk::BlockComment)
}
