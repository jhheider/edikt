//! JSONC/JSON5 lexer.
//!
//! JSONC = JSON + `//` line and `/* */` block comments + trailing commas.
//! JSON5 adds, on top of that: unquoted object keys, single-quoted strings,
//! hex/leading-dot/trailing-dot/`+`-signed numbers, `Infinity`/`NaN`, and
//! backslash-newline line continuations inside strings.
//!
//! One lexer serves the whole family, matching how the crate already reads
//! `.json` with the JSONC parser: the grammar is a superset and structural
//! validity is checked separately in `lib::parse`. A `.jsonc` file that uses a
//! JSON5 spelling therefore lexes rather than erroring - leniency on input, and
//! never on output, since edits only rewrite the nodes they target.
//!
//! `Ident` is the ECMAScript `IdentifierName` of practical configs: ASCII
//! letters, `_`, `$`, then alphanumerics. The spec also admits Unicode letters
//! and `\u` escapes in identifiers; those stay unlexed (an error token) rather
//! than half-supported.
//!
//! Whitespace and comments are captured (not skipped) so they land in the CST as
//! trivia. The lexer stays total: an unrecognized byte is lexed as an error
//! token so no input is ever dropped.

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
    // `\\\r?\n` is JSON5's line continuation: regex `.` does not match a
    // newline, so the escaped-newline case needs its own alternative.
    #[regex(r#""([^"\\]|\\.|\\\r?\n)*""#, allow_greedy = true)]
    Str,
    /// JSON5 single-quoted string.
    #[regex(r#"'([^'\\]|\\.|\\\r?\n)*'"#, allow_greedy = true)]
    SingleStr,
    // Decimal (JSON5 allows a `+` sign and a bare leading or trailing point),
    // then hex, then the non-finite literals. `Infinity`/`NaN` outrank `Ident`
    // on logos' length-derived priority; `ident_does_not_swallow_infinity_or_nan`
    // pins that so a logos upgrade cannot silently flip it.
    #[regex(r"[+-]?[0-9]+(\.[0-9]*)?([eE][+-]?[0-9]+)?")]
    #[regex(r"[+-]?\.[0-9]+([eE][+-]?[0-9]+)?")]
    #[regex(r"[+-]?0[xX][0-9a-fA-F]+")]
    #[regex(r"[+-]?Infinity")]
    #[token("NaN")]
    Num,
    /// JSON5 unquoted object key (ASCII `IdentifierName`; see the module note).
    #[regex(r"[A-Za-z_$][A-Za-z0-9_$]*")]
    Ident,
    #[regex(r"//[^\n]*", allow_greedy = true)]
    LineComment,
    #[regex(r"/\*([^*]|\*+[^*/])*\*+/", allow_greedy = true)]
    BlockComment,
    #[regex(r"[ \t\r\n]+")]
    Ws,
}
