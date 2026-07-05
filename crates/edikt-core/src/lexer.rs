//! Tokens for the expression language.
//!
//! Whitespace is insignificant *inside an expression* (unlike file content), so
//! it is skipped here. `true`/`false`/`null` are lexed as `Ident` and resolved
//! to literals by the parser, avoiding token-priority ambiguity.

use logos::Logos;

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq)]
#[logos(skip r"[ \t\r\n]+")]
pub enum Lx {
    #[token(".")]
    Dot,
    #[token("[")]
    LBrack,
    #[token("]")]
    RBrack,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token(":")]
    Colon,
    #[token("|")]
    Pipe,
    #[token("|=")]
    PipeAssign,
    #[token(",")]
    Comma,
    #[token(";")]
    Semi,
    #[token("=")]
    Assign,
    #[token("==")]
    EqEq,
    #[token("!=")]
    Ne,
    #[token("<=")]
    Le,
    #[token(">=")]
    Ge,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("+")]
    Plus,
    #[token("+=")]
    PlusAssign,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("//")]
    Alt,
    #[token("%")]
    Percent,
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")]
    Ident,
    #[regex(r"[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?")]
    Num,
    #[regex(r#""([^"\\]|\\.)*""#, allow_greedy = true)]
    Str,
}
