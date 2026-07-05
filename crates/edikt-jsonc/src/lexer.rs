//! JSONC lexer.
//!
//! JSONC = JSON + `//` line and `/* */` block comments + trailing commas.
//! Whitespace and comments are captured (not skipped) so they land in the CST as
//! trivia. Numbers may carry a leading `-`. The lexer stays total: an
//! unrecognized byte is lexed as an error token so no input is ever dropped.

use logos::Logos;

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(":")]
    Colon,
    #[token(",")]
    Comma,
    #[token("true")]
    True,
    #[token("false")]
    False,
    #[token("null")]
    Null,
    #[regex(r#""([^"\\]|\\.)*""#, allow_greedy = true)]
    Str,
    #[regex(r"-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?")]
    Num,
    #[regex(r"//[^\n]*", allow_greedy = true)]
    LineComment,
    #[regex(r"/\*([^*]|\*+[^*/])*\*+/", allow_greedy = true)]
    BlockComment,
    #[regex(r"[ \t\r\n]+")]
    Ws,
}
