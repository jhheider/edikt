//! Recursive-descent parser building a lossless rowan green tree.
//!
//! Grammar covers the JSONC/JSON5 family; see `lexer` for what JSON5 adds.
//!
//! Every token (including trivia) is added to the tree in lexical order, so the
//! green tree round-trips byte-for-byte. The tree is always built, even over
//! malformed input, so it serializes; structural problems (a missing `:` or
//! `,`, a stray token where a key or value belongs, an unclosed container,
//! anything but trivia after the top value, an unlexable byte) are recorded as
//! [`SyntaxError`]s alongside it, and `lib::parse` rejects a document that has
//! any. The leniency left is exactly what the family allows: trailing commas,
//! comments, and JSON5 spellings.

use crate::lexer::Tok;
use crate::syntax::{Sk, is_key, is_trivia};
use edikt_syntax::Builder;
use logos::Logos;
use rowan::GreenNode;

/// A structural problem, located at a byte offset into the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntaxError {
    pub offset: usize,
    pub msg: String,
    /// For an unclosed container, the offset of its opening bracket.
    pub opened: Option<usize>,
}

/// Lex and parse `src` into a green tree, ignoring structural errors. For
/// re-parsing text edikt generated itself, which is well-formed by
/// construction.
pub(crate) fn build(src: &str) -> GreenNode {
    build_checked(src).0
}

/// Lex and parse `src` into a green tree plus the structural errors found.
pub(crate) fn build_checked(src: &str) -> (GreenNode, Vec<SyntaxError>) {
    let toks = lex(src);
    let mut p = Parser {
        toks,
        pos: 0,
        offset: 0,
        errors: Vec::new(),
        builder: Builder::new(),
    };
    p.builder.start_node(Sk::Root);
    p.value();
    p.skip_trivia();
    if p.cur().is_some_and(|k| k != Sk::Error) {
        let found = p.describe();
        p.error(format!("unexpected {found} after the top-level value"));
    }
    while p.pos < p.toks.len() {
        p.bump(); // trailing trivia / stray tokens, kept losslessly
    }
    p.builder.finish_node();
    (p.builder.finish(), p.errors)
}

fn lex(src: &str) -> Vec<(Sk, &str)> {
    let mut lexer = Tok::lexer(src);
    let mut out = Vec::new();
    while let Some(res) = lexer.next() {
        let kind = match res {
            Ok(Tok::LBrace) => Sk::LBrace,
            Ok(Tok::RBrace) => Sk::RBrace,
            Ok(Tok::LBracket) => Sk::LBracket,
            Ok(Tok::RBracket) => Sk::RBracket,
            Ok(Tok::Colon) => Sk::Colon,
            Ok(Tok::Comma) => Sk::Comma,
            Ok(Tok::True) => Sk::True,
            Ok(Tok::False) => Sk::False,
            Ok(Tok::Null) => Sk::Null,
            Ok(Tok::Str) => Sk::Str,
            Ok(Tok::SingleStr) => Sk::SingleStr,
            Ok(Tok::Num) => Sk::Num,
            Ok(Tok::Ident) => Sk::Ident,
            Ok(Tok::LineComment) => Sk::LineComment,
            Ok(Tok::BlockComment) => Sk::BlockComment,
            Ok(Tok::Ws) => Sk::Ws,
            Err(()) => Sk::Error,
        };
        out.push((kind, lexer.slice()));
    }
    out
}

struct Parser<'a> {
    toks: Vec<(Sk, &'a str)>,
    pos: usize,
    /// Byte offset of `toks[pos]` in the source.
    offset: usize,
    errors: Vec<SyntaxError>,
    builder: Builder<Sk>,
}

impl Parser<'_> {
    fn cur(&self) -> Option<Sk> {
        self.toks.get(self.pos).map(|(k, _)| *k)
    }

    /// Add the current token to the tree. An error token (a byte the lexer
    /// does not recognize) reports itself here, wherever it sits.
    fn bump(&mut self) {
        let (kind, text) = self.toks[self.pos];
        if kind == Sk::Error {
            let c = text.chars().next().unwrap_or('?');
            self.error(format!("unexpected character `{c}`"));
        }
        self.builder.token(kind, text);
        self.pos += 1;
        self.offset += text.len();
    }

    /// Record an error at the cursor.
    fn error(&mut self, msg: String) {
        self.errors.push(SyntaxError {
            offset: self.offset,
            msg,
            opened: None,
        });
    }

    /// Record that input ended inside the container opened at `open`.
    fn unclosed(&mut self, open: usize, close: char) {
        self.errors.push(SyntaxError {
            offset: self.offset,
            msg: format!("expected `{close}`, found end of input"),
            opened: Some(open),
        });
    }

    /// Record `expected {what}, found <cursor token>`, unless the cursor is an
    /// error token, which reports itself (more precisely) when bumped.
    fn expected(&mut self, what: &str) {
        if self.cur() == Some(Sk::Error) {
            return;
        }
        let found = self.describe();
        self.error(format!("expected {what}, found {found}"));
    }

    /// The token at the cursor, for an error message.
    fn describe(&self) -> String {
        match self.toks.get(self.pos) {
            None => "end of input".to_string(),
            Some((_, text)) => {
                let short: String = text.chars().take(24).collect();
                let more = if short.len() < text.len() { "..." } else { "" };
                format!("`{short}{more}`")
            }
        }
    }

    fn skip_trivia(&mut self) {
        while let Some(k) = self.cur() {
            if is_trivia(k) {
                self.bump();
            } else {
                break;
            }
        }
    }

    /// The next non-trivia kind at or after the cursor, without consuming.
    fn next_significant(&self) -> Option<Sk> {
        self.toks[self.pos..]
            .iter()
            .map(|(k, _)| *k)
            .find(|k| !is_trivia(*k))
    }

    fn value(&mut self) {
        self.skip_trivia();
        self.builder.start_node(Sk::Value);
        match self.cur() {
            Some(Sk::LBrace) => self.object(),
            Some(Sk::LBracket) => self.array(),
            Some(Sk::True | Sk::False | Sk::Null | Sk::Str | Sk::SingleStr | Sk::Num) => {
                self.bump();
            }
            // A closer or separator belongs to the enclosing container: leave
            // it there, so `{"a":}` still closes its object.
            Some(Sk::RBrace | Sk::RBracket | Sk::Comma | Sk::Colon) => self.expected("a value"),
            // A bare word is a key spelling only, never a value. It (or an
            // error token) is still kept in the tree, losslessly.
            Some(_) => {
                self.expected("a value");
                self.bump();
            }
            // An empty document is `lib::parse`'s "no value found"; inside a
            // container, end of input is reported as the unclosed container.
            None => {}
        }
        self.builder.finish_node();
    }

    fn object(&mut self) {
        let open = self.offset;
        self.builder.start_node(Sk::Object);
        self.bump(); // {
        loop {
            self.skip_trivia();
            match self.cur() {
                Some(Sk::RBrace) => {
                    self.bump();
                    break;
                }
                None => {
                    self.unclosed(open, '}');
                    break;
                }
                _ => {
                    let start = self.pos;
                    self.builder.start_node(Sk::Member);
                    // JSON's quoted key, or any of JSON5's key spellings.
                    if self.cur().is_some_and(is_key) {
                        self.bump(); // key
                        self.skip_trivia();
                        if self.cur() == Some(Sk::Colon) {
                            self.bump();
                        } else {
                            self.expected("`:` after an object key");
                        }
                    } else {
                        self.expected("an object key");
                        if self.cur() == Some(Sk::Colon) {
                            self.bump();
                        }
                    }
                    self.value();
                    // Absorb the trailing comma into the member, but only if one
                    // actually follows, so a last member does not swallow the
                    // whitespace before `}` (which would break clean deletion).
                    match self.next_significant() {
                        Some(Sk::Comma) => {
                            self.skip_trivia();
                            self.bump(); // comma
                        }
                        Some(Sk::RBrace) | None => {}
                        Some(_) => {
                            self.skip_trivia();
                            self.expected("`,` or `}`");
                        }
                    }
                    // Guarantee progress over a token nothing above consumes
                    // (a `]` in an object), keeping it in the tree.
                    if self.pos == start {
                        self.bump();
                    }
                    self.builder.finish_node(); // Member
                }
            }
        }
        self.builder.finish_node(); // Object
    }

    fn array(&mut self) {
        let open = self.offset;
        self.builder.start_node(Sk::Array);
        self.bump(); // [
        loop {
            self.skip_trivia();
            match self.cur() {
                Some(Sk::RBracket) => {
                    self.bump();
                    break;
                }
                None => {
                    self.unclosed(open, ']');
                    break;
                }
                _ => {
                    let start = self.pos;
                    self.value();
                    self.skip_trivia();
                    match self.cur() {
                        Some(Sk::Comma) => self.bump(),
                        Some(Sk::RBracket) | None => {}
                        Some(_) => self.expected("`,` or `]`"),
                    }
                    // Guarantee progress over a token nothing above consumes
                    // (a `}` or `:` in an array), keeping it in the tree.
                    if self.pos == start {
                        self.bump();
                    }
                }
            }
        }
        self.builder.finish_node(); // Array
    }
}
