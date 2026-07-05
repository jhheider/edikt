//! Recursive-descent / precedence parser for the expression language.
//!
//! Precedence, lowest to highest: `|` (pipe), `,` (comma), comparison,
//! additive, multiplicative, unary `-`, primary. Dotted/indexed paths desugar
//! into a `Path` of steps.

use crate::ast::{BinOp, Expr, Step};
use crate::comment::CommentKind;
use crate::lexer::Lx;
use crate::value::Value;
use logos::Logos;

/// A parse failure with a byte offset into the source expression.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg} (at offset {pos})")]
pub struct ParseError {
    pub msg: String,
    pub pos: usize,
}

struct Tok {
    kind: Lx,
    text: String,
    start: usize,
}

/// Parse a complete expression, consuming all of `src`.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.parse_pipe()?;
    if p.pos != p.toks.len() {
        let t = &p.toks[p.pos];
        return Err(ParseError {
            msg: format!("unexpected trailing token `{}`", t.text),
            pos: t.start,
        });
    }
    Ok(e)
}

fn lex(src: &str) -> Result<Vec<Tok>, ParseError> {
    let mut lx = Lx::lexer(src);
    let mut out = Vec::new();
    while let Some(res) = lx.next() {
        let span = lx.span();
        match res {
            Ok(kind) => out.push(Tok {
                kind,
                text: lx.slice().to_string(),
                start: span.start,
            }),
            Err(_) => {
                return Err(ParseError {
                    msg: format!("unexpected character `{}`", lx.slice()),
                    pos: span.start,
                });
            }
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<Lx> {
        self.toks.get(self.pos).map(|t| t.kind)
    }
    fn text(&self) -> &str {
        self.toks
            .get(self.pos)
            .map(|t| t.text.as_str())
            .unwrap_or("")
    }
    fn at_end(&self) -> usize {
        self.toks
            .last()
            .map(|t| t.start + t.text.len())
            .unwrap_or(0)
    }
    fn err_here(&self, msg: impl Into<String>) -> ParseError {
        let pos = self
            .toks
            .get(self.pos)
            .map(|t| t.start)
            .unwrap_or_else(|| self.at_end());
        ParseError {
            msg: msg.into(),
            pos,
        }
    }
    fn expect(&mut self, kind: Lx, what: &str) -> Result<(), ParseError> {
        if self.peek() == Some(kind) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err_here(format!("expected {what}")))
        }
    }

    fn parse_pipe(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_comma()?;
        while self.peek() == Some(Lx::Pipe) {
            self.pos += 1;
            let right = self.parse_comma()?;
            left = Expr::Pipe(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_comma(&mut self) -> Result<Expr, ParseError> {
        let first = self.parse_assign()?;
        if self.peek() != Some(Lx::Comma) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.peek() == Some(Lx::Comma) {
            self.pos += 1;
            items.push(self.parse_assign()?);
        }
        Ok(Expr::Comma(items))
    }

    fn parse_assign(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_alt()?;
        match self.peek() {
            Some(Lx::Assign) => {
                self.pos += 1;
                let rhs = self.parse_assign()?; // right-associative
                Ok(Expr::Assign(Box::new(lhs), Box::new(rhs)))
            }
            Some(Lx::PipeAssign) => {
                self.pos += 1;
                let rhs = self.parse_assign()?;
                Ok(Expr::UpdateAssign(Box::new(lhs), Box::new(rhs)))
            }
            Some(Lx::PlusAssign) => {
                self.pos += 1;
                let rhs = self.parse_assign()?;
                Ok(Expr::AddAssign(Box::new(lhs), Box::new(rhs)))
            }
            _ => Ok(lhs),
        }
    }

    /// `a // b` — right-associative, binding tighter than `=` (so
    /// `.k = .a // "d"` defaults the RHS) and looser than comparison.
    fn parse_alt(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_cmp()?;
        if self.peek() == Some(Lx::Alt) {
            self.pos += 1;
            let right = self.parse_alt()?;
            return Ok(Expr::Alternative(Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_add()?;
        let op = match self.peek() {
            Some(Lx::EqEq) => BinOp::Eq,
            Some(Lx::Ne) => BinOp::Ne,
            Some(Lx::Lt) => BinOp::Lt,
            Some(Lx::Gt) => BinOp::Gt,
            Some(Lx::Le) => BinOp::Le,
            Some(Lx::Ge) => BinOp::Ge,
            _ => return Ok(left),
        };
        self.pos += 1;
        let right = self.parse_add()?;
        Ok(Expr::Binary(op, Box::new(left), Box::new(right)))
    }

    fn parse_add(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Some(Lx::Plus) => BinOp::Add,
                Some(Lx::Minus) => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_mul()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Lx::Star) => BinOp::Mul,
                Some(Lx::Slash) => BinOp::Div,
                Some(Lx::Percent) => BinOp::Mod,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_unary()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        if self.peek() == Some(Lx::Minus) {
            self.pos += 1;
            return Ok(Expr::Neg(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            Some(Lx::Dot) => self.parse_path(),
            Some(Lx::LParen) => {
                self.pos += 1;
                let e = self.parse_pipe()?;
                self.expect(Lx::RParen, "`)`")?;
                Ok(e)
            }
            Some(Lx::LBrack) => {
                self.pos += 1;
                if self.peek() == Some(Lx::RBrack) {
                    self.pos += 1;
                    return Ok(Expr::Collect(None));
                }
                let inner = self.parse_pipe()?;
                self.expect(Lx::RBrack, "`]`")?;
                Ok(Expr::Collect(Some(Box::new(inner))))
            }
            Some(Lx::LBrace) => self.parse_object_construct(),
            Some(Lx::Num) => {
                let v = number_value(self.text());
                self.pos += 1;
                Ok(Expr::Literal(v))
            }
            Some(Lx::Str) => {
                let v = Value::Str(unescape(self.text()));
                self.pos += 1;
                Ok(Expr::Literal(v))
            }
            Some(Lx::Ident) => self.parse_ident(),
            _ => Err(self.err_here("expected an expression")),
        }
    }

    fn parse_object_construct(&mut self) -> Result<Expr, ParseError> {
        self.pos += 1; // `{`
        let mut pairs = Vec::new();
        if self.peek() == Some(Lx::RBrace) {
            self.pos += 1;
            return Ok(Expr::ObjectConstruct(pairs));
        }
        loop {
            let key = match self.peek() {
                Some(Lx::Ident) => self.text().to_string(),
                Some(Lx::Str) => unescape(self.text()),
                _ => return Err(self.err_here("expected an object key")),
            };
            self.pos += 1;
            self.expect(Lx::Colon, "`:`")?;
            // A comparison-level value keeps `,` free to separate entries.
            let value = self.parse_cmp()?;
            pairs.push((key, value));
            match self.peek() {
                Some(Lx::Comma) => self.pos += 1,
                Some(Lx::RBrace) => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.err_here("expected `,` or `}`")),
            }
        }
        Ok(Expr::ObjectConstruct(pairs))
    }

    fn parse_path(&mut self) -> Result<Expr, ParseError> {
        self.pos += 1; // leading `.`
        let mut steps = Vec::new();
        loop {
            match self.peek() {
                Some(Lx::Ident) => {
                    steps.push(Step::Field(self.text().to_string()));
                    self.pos += 1;
                }
                Some(Lx::Str) => {
                    steps.push(Step::Field(unescape(self.text())));
                    self.pos += 1;
                }
                Some(Lx::LBrack) => {
                    self.pos += 1;
                    if self.peek() == Some(Lx::RBrack) {
                        self.pos += 1;
                        steps.push(Step::Iterate);
                    } else if self.peek() == Some(Lx::Str) {
                        // `.["key"]` — a field by name (dotted/special keys).
                        let key = unescape(self.text());
                        self.pos += 1;
                        self.expect(Lx::RBrack, "`]`")?;
                        steps.push(Step::Field(key));
                    } else {
                        let neg = self.peek() == Some(Lx::Minus);
                        if neg {
                            self.pos += 1;
                        }
                        if self.peek() != Some(Lx::Num) {
                            return Err(self.err_here("expected an array index or a string key"));
                        }
                        let n = parse_i64(self.text())
                            .map_err(|_| self.err_here("array index out of range"))?;
                        self.pos += 1;
                        self.expect(Lx::RBrack, "`]`")?;
                        steps.push(Step::Index(if neg { -n } else { n }));
                    }
                }
                Some(Lx::Hash) => {
                    // `#` addresses the current node's comment. `#` alone is the
                    // head comment; `#.head`/`#.inline`/`#.foot` pick a kind.
                    // Terminal — no navigation follows a comment.
                    self.pos += 1;
                    let mut kind = CommentKind::Head;
                    if self.peek() == Some(Lx::Dot)
                        && let Some(word) =
                            self.toks.get(self.pos + 1).filter(|t| t.kind == Lx::Ident)
                        && let Some(k) = comment_kind(&word.text)
                    {
                        self.pos += 2; // consume `.` and the kind word
                        kind = k;
                    }
                    steps.push(Step::Comment(kind));
                    break;
                }
                _ => break,
            }
            match self.peek() {
                Some(Lx::Dot) => {
                    self.pos += 1;
                    continue;
                }
                Some(Lx::LBrack) => continue,
                _ => break,
            }
        }
        Ok(Expr::Path(steps))
    }

    fn parse_ident(&mut self) -> Result<Expr, ParseError> {
        let name = self.text().to_string();
        self.pos += 1;
        match name.as_str() {
            "true" => return Ok(Expr::Literal(Value::Bool(true))),
            "false" => return Ok(Expr::Literal(Value::Bool(false))),
            "null" => return Ok(Expr::Literal(Value::Null)),
            _ => {}
        }
        if self.peek() == Some(Lx::LParen) {
            self.pos += 1;
            let mut args = vec![self.parse_pipe()?];
            while self.peek() == Some(Lx::Semi) {
                self.pos += 1;
                args.push(self.parse_pipe()?);
            }
            self.expect(Lx::RParen, "`)`")?;
            Ok(Expr::Call(name, args))
        } else {
            Ok(Expr::Call(name, Vec::new()))
        }
    }
}

/// Map a `#.<word>` refinement to its comment kind.
fn comment_kind(word: &str) -> Option<CommentKind> {
    match word {
        "head" => Some(CommentKind::Head),
        "inline" => Some(CommentKind::Inline),
        "foot" => Some(CommentKind::Foot),
        _ => None,
    }
}

fn number_value(t: &str) -> Value {
    if t.contains(['.', 'e', 'E']) {
        Value::Float(t.parse().unwrap_or(0.0))
    } else {
        match t.parse::<i64>() {
            Ok(i) => Value::Int(i),
            Err(_) => Value::Float(t.parse().unwrap_or(0.0)),
        }
    }
}

fn parse_i64(t: &str) -> Result<i64, std::num::ParseIntError> {
    t.parse::<i64>()
}

/// Unescape a JSON-style double-quoted string token (including its quotes).
fn unescape(tok: &str) -> String {
    let inner = tok
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(tok);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{0008}'),
            Some('f') => out.push('\u{000c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Expr {
        parse(s).unwrap_or_else(|e| panic!("parse `{s}`: {e}"))
    }

    #[test]
    fn identity() {
        assert_eq!(p("."), Expr::Path(vec![]));
    }

    #[test]
    fn dotted_path() {
        assert_eq!(
            p(".a.b"),
            Expr::Path(vec![Step::Field("a".into()), Step::Field("b".into())])
        );
    }

    #[test]
    fn index_and_iterate() {
        assert_eq!(
            p(".arr[0][]"),
            Expr::Path(vec![
                Step::Field("arr".into()),
                Step::Index(0),
                Step::Iterate
            ])
        );
        assert_eq!(
            p(".x[-1]"),
            Expr::Path(vec![Step::Field("x".into()), Step::Index(-1)])
        );
    }

    #[test]
    fn quoted_field() {
        assert_eq!(
            p(r#"."weird key""#),
            Expr::Path(vec![Step::Field("weird key".into())])
        );
    }

    #[test]
    fn literals() {
        assert_eq!(p("true"), Expr::Literal(Value::Bool(true)));
        assert_eq!(p("null"), Expr::Literal(Value::Null));
        assert_eq!(p("42"), Expr::Literal(Value::Int(42)));
        assert_eq!(p("1.5"), Expr::Literal(Value::Float(1.5)));
        assert_eq!(p(r#""hi""#), Expr::Literal(Value::Str("hi".into())));
    }

    #[test]
    fn arithmetic_precedence() {
        // 1 + 2 * 3  ==  1 + (2 * 3)
        assert_eq!(
            p("1 + 2 * 3"),
            Expr::Binary(
                BinOp::Add,
                Box::new(Expr::Literal(Value::Int(1))),
                Box::new(Expr::Binary(
                    BinOp::Mul,
                    Box::new(Expr::Literal(Value::Int(2))),
                    Box::new(Expr::Literal(Value::Int(3))),
                )),
            )
        );
    }

    #[test]
    fn pipe_and_select() {
        let e = p(r#".items[] | select(.name == "x")"#);
        match e {
            Expr::Pipe(l, r) => {
                assert_eq!(
                    *l,
                    Expr::Path(vec![Step::Field("items".into()), Step::Iterate])
                );
                match *r {
                    Expr::Call(ref name, ref args) => {
                        assert_eq!(name, "select");
                        assert_eq!(args.len(), 1);
                    }
                    _ => panic!("expected select call"),
                }
            }
            _ => panic!("expected pipe"),
        }
    }

    #[test]
    fn errors() {
        assert!(parse(".a.").is_ok()); // lenient trailing dot
        assert!(parse("(").is_err());
        assert!(parse(".a b").is_err()); // trailing token
        assert!(parse("@").is_err()); // bad char
    }

    #[test]
    fn comment_accessor() {
        use crate::comment::CommentKind;
        // `#` alone is the head comment.
        assert_eq!(
            p(".foo.#"),
            Expr::Path(vec![
                Step::Field("foo".into()),
                Step::Comment(CommentKind::Head)
            ])
        );
        // `#.head` / `#.inline` / `#.foot` pick a kind.
        assert_eq!(
            p(".foo.#.inline"),
            Expr::Path(vec![
                Step::Field("foo".into()),
                Step::Comment(CommentKind::Inline)
            ])
        );
        assert_eq!(
            p(".a.#.foot"),
            Expr::Path(vec![
                Step::Field("a".into()),
                Step::Comment(CommentKind::Foot)
            ])
        );
        // `.#` at the top is the document comment.
        assert_eq!(p(".#"), Expr::Path(vec![Step::Comment(CommentKind::Head)]));
        // After iteration: `.items[].#`.
        assert_eq!(
            p(".items[].#"),
            Expr::Path(vec![
                Step::Field("items".into()),
                Step::Iterate,
                Step::Comment(CommentKind::Head)
            ])
        );
        // Comment is terminal — nothing may navigate past it.
        assert!(parse(".foo.#.bar").is_err());
    }
}
