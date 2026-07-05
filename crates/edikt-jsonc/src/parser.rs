//! Recursive-descent parser building a lossless rowan green tree.
//!
//! Every token (including trivia) is added to the tree in lexical order, so the
//! green tree round-trips byte-for-byte. The grammar is deliberately lenient —
//! trailing commas are accepted, and the tree is always well-formed enough to
//! serialize; structural validity is checked separately in `lib::parse`.

use crate::lexer::Tok;
use crate::syntax::{Sk, is_trivia, sk};
use logos::Logos;
use rowan::{GreenNode, GreenNodeBuilder};

/// Lex and parse `src` into a green tree.
pub(crate) fn build(src: &str) -> GreenNode {
    let toks = lex(src);
    let mut p = Parser {
        toks,
        pos: 0,
        builder: GreenNodeBuilder::new(),
    };
    p.builder.start_node(sk(Sk::Root));
    p.value();
    while p.pos < p.toks.len() {
        p.bump(); // trailing trivia / stray tokens
    }
    p.builder.finish_node();
    p.builder.finish()
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
            Ok(Tok::Num) => Sk::Num,
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
    builder: GreenNodeBuilder<'static>,
}

impl Parser<'_> {
    fn cur(&self) -> Option<Sk> {
        self.toks.get(self.pos).map(|(k, _)| *k)
    }

    fn bump(&mut self) {
        let (kind, text) = self.toks[self.pos];
        self.builder.token(sk(kind), text);
        self.pos += 1;
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
        self.builder.start_node(sk(Sk::Value));
        match self.cur() {
            Some(Sk::LBrace) => self.object(),
            Some(Sk::LBracket) => self.array(),
            Some(_) => self.bump(), // scalar, or a stray token (kept, lossless)
            None => {}
        }
        self.builder.finish_node();
    }

    fn object(&mut self) {
        self.builder.start_node(sk(Sk::Object));
        self.bump(); // {
        loop {
            self.skip_trivia();
            match self.cur() {
                Some(Sk::RBrace) => {
                    self.bump();
                    break;
                }
                None => break,
                _ => {
                    self.builder.start_node(sk(Sk::Member));
                    if self.cur() == Some(Sk::Str) {
                        self.bump(); // key
                    }
                    self.skip_trivia();
                    if self.cur() == Some(Sk::Colon) {
                        self.bump();
                    }
                    self.value();
                    // Absorb the trailing comma into the member — but only if one
                    // actually follows, so a last member does not swallow the
                    // whitespace before `}` (which would break clean deletion).
                    if self.next_significant() == Some(Sk::Comma) {
                        self.skip_trivia();
                        self.bump(); // comma
                    }
                    self.builder.finish_node(); // Member
                }
            }
        }
        self.builder.finish_node(); // Object
    }

    fn array(&mut self) {
        self.builder.start_node(sk(Sk::Array));
        self.bump(); // [
        loop {
            self.skip_trivia();
            match self.cur() {
                Some(Sk::RBracket) => {
                    self.bump();
                    break;
                }
                None => break,
                _ => {
                    self.value();
                    self.skip_trivia();
                    if self.cur() == Some(Sk::Comma) {
                        self.bump();
                    }
                }
            }
        }
        self.builder.finish_node();
    }
}
