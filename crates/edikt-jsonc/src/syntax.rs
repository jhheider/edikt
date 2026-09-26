//! Syntax kinds and the rowan `Language` for JSONC.

edikt_syntax::syntax_kinds! {
    /// Token and node kinds: tokens first, then composite nodes.
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
}

pub type JsoncLang = edikt_syntax::Lang<Sk>;

pub type SyntaxNode = rowan::SyntaxNode<JsoncLang>;
pub type SyntaxToken = rowan::SyntaxToken<JsoncLang>;
pub(crate) type SyntaxElement = rowan::NodeOrToken<SyntaxNode, SyntaxToken>;

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
