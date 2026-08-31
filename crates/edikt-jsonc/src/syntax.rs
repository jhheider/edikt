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
    SingleStr,
    Num,
    Ident,
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
        const KINDS: [Sk; 22] = [
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
            Sk::SingleStr,
            Sk::Num,
            Sk::Ident,
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
pub type SyntaxToken = rowan::SyntaxToken<JsoncLang>;
pub(crate) type SyntaxElement = rowan::NodeOrToken<SyntaxNode, SyntaxToken>;

/// Raw kind for the green-tree builder.
pub(crate) fn sk(kind: Sk) -> rowan::SyntaxKind {
    rowan::SyntaxKind(kind as u16)
}

/// Is this kind trivia (whitespace or a comment)?
pub(crate) fn is_trivia(kind: Sk) -> bool {
    matches!(kind, Sk::Ws | Sk::LineComment | Sk::BlockComment)
}

/// Can this kind stand in an object's key position?
///
/// JSON has only the double-quoted string. JSON5 adds the bare identifier and
/// the single-quoted string, and since its keys are `IdentifierName` rather than
/// `Identifier`, the reserved words are legal keys too (`{ null: 1 }`) - those
/// lex as their keyword kinds, so they are listed explicitly.
pub(crate) fn is_key(kind: Sk) -> bool {
    matches!(
        kind,
        Sk::Str | Sk::SingleStr | Sk::Ident | Sk::True | Sk::False | Sk::Null
    )
}
