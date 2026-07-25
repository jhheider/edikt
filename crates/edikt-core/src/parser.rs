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
    let e = match p.parse_program() {
        Ok(e) => e,
        Err(e) => return Err(with_hyphen_hint(e, src)),
    };
    if p.pos != p.toks.len() {
        let t = &p.toks[p.pos];
        return Err(with_hyphen_hint(
            ParseError {
                msg: format!("unexpected trailing token `{}`", t.text),
                pos: t.start,
            },
            src,
        ));
    }
    Ok(e)
}

/// Replace a parse failure with the hyphenated-key explanation when the source
/// contains one.
///
/// Assignment targets no longer need this: [`Parser::try_assign_target`] parses
/// hyphenated keys outright, because subtraction cannot appear on the left of
/// `=`. Everywhere else the ambiguity is real, since `.total-length` is a
/// legitimate subtraction of the `length` builtin, so a **query** like
/// `.dev-dependencies.serde_json` still cannot be guessed at. It dies at the `.`
/// after `dependencies` with "unexpected trailing token `.`", which names
/// neither the hyphen nor the fix.
///
/// Only ever applied to an already failing parse, so it cannot change how a
/// working expression behaves.
fn with_hyphen_hint(err: ParseError, src: &str) -> ParseError {
    // The LHS check got there first and knows more than this does.
    if err.msg.contains("contains `-`") {
        return err;
    }
    match hyphenated_key(src) {
        // Same wording as the LHS check, so one mistake has one message.
        Some((key, pos)) => ParseError {
            msg: format!(
                "key `{key}` contains `-` (parsed as subtraction); quote it: {}\"{key}\"",
                &src[..pos]
            ),
            pos,
        },
        None => err,
    }
}

/// The first `.some-key` in the source, with the offset of its first character.
///
/// A hyphen only counts when it sits between two identifier characters, so
/// `.a-1` and `.a - b` are left alone: the first is arithmetic on a literal and
/// the second is spaced, and neither is the mistake being diagnosed.
fn hyphenated_key(src: &str) -> Option<(String, usize)> {
    let b = src.as_bytes();
    let ident_start = |c: u8| c.is_ascii_alphabetic() || c == b'_';
    let ident_char = |c: u8| c.is_ascii_alphanumeric() || c == b'_';

    let mut i = 0;
    while i < b.len() {
        if b[i] != b'.' || i + 1 >= b.len() || !ident_start(b[i + 1]) {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        let mut hyphenated = false;
        while j < b.len() {
            if ident_char(b[j]) {
                j += 1;
            } else if b[j] == b'-' && j + 1 < b.len() && ident_start(b[j + 1]) {
                hyphenated = true;
                j += 1;
            } else {
                break;
            }
        }
        if hyphenated {
            return Some((src[start..j].to_string(), start));
        }
        i = j;
    }
    None
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

    /// A whole program: an optional leading `^dN` document selector (for
    /// multi-document YAML streams), then the expression it scopes.
    fn parse_program(&mut self) -> Result<Expr, ParseError> {
        if self.peek() != Some(Lx::Caret) {
            return self.parse_pipe();
        }
        self.pos += 1; // `^`
        let n = match self.peek() {
            Some(Lx::Ident) => {
                let digits = self.text().strip_prefix('d').ok_or_else(|| {
                    self.err_here("document selector is `^dN`, e.g. `^d0` (document 0)")
                })?;
                let n = digits.parse::<usize>().map_err(|_| {
                    self.err_here("`^dN` needs a document index, e.g. `^d0` (document 0)")
                })?;
                self.pos += 1;
                n
            }
            _ => {
                return Err(self.err_here("expected a document index after `^`, e.g. `^d0`"));
            }
        };
        // `^dN | body` and `^dN.body` both scope `body` to the document; a bare
        // `^dN` selects the whole document (identity body).
        if self.peek() == Some(Lx::Pipe) {
            self.pos += 1;
        }
        let body = if self.peek().is_none() {
            Expr::Path(Vec::new())
        } else {
            self.parse_pipe()?
        };
        Ok(Expr::DocSelect(n, Box::new(body)))
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
        let lhs = match self.try_assign_target() {
            Some(path) => path,
            None => self.parse_alt()?,
        };
        match self.peek() {
            Some(Lx::Assign) => {
                self.reject_hyphen_key_lhs(&lhs)?;
                self.pos += 1;
                let rhs = self.parse_assign()?; // right-associative
                Ok(Expr::Assign(Box::new(lhs), Box::new(rhs)))
            }
            Some(Lx::PipeAssign) => {
                self.reject_hyphen_key_lhs(&lhs)?;
                self.pos += 1;
                let rhs = self.parse_assign()?;
                Ok(Expr::UpdateAssign(Box::new(lhs), Box::new(rhs)))
            }
            Some(Lx::PlusAssign) => {
                self.reject_hyphen_key_lhs(&lhs)?;
                self.pos += 1;
                let rhs = self.parse_assign()?;
                Ok(Expr::AddAssign(Box::new(lhs), Box::new(rhs)))
            }
            _ => Ok(lhs),
        }
    }

    /// Parse an assignment target: a path whose bare keys may contain `-`.
    ///
    /// **The left side of an assignment must be a path, so subtraction cannot
    /// occur there.** That removes the ambiguity that forces `."dev-dependencies"`
    /// everywhere else: in this one position a hyphen between two identifier
    /// characters is unambiguously part of the key, and edikt can simply parse
    /// it instead of making people quote the most common key in a Cargo.toml.
    ///
    /// Speculative, and it rewinds on any miss, so nothing here can change how
    /// a non-assignment expression parses. Returns `Some` only when a complete
    /// path is followed by an assignment operator; otherwise the position is
    /// restored and the ordinary precedence chain runs unchanged.
    ///
    /// Purely additive: every expression this accepts previously failed, since
    /// a subtraction on the left of `=` was never legal.
    fn try_assign_target(&mut self) -> Option<Expr> {
        let save = self.pos;
        let parsed = self.parse_hyphenated_path();
        let is_target = parsed.is_some()
            && matches!(
                self.peek(),
                Some(Lx::Assign | Lx::PipeAssign | Lx::PlusAssign)
            );
        if is_target {
            return parsed;
        }
        self.pos = save;
        None
    }

    /// A `.a.b-c[0]` path, joining `Ident - Ident` runs that are adjacent in the
    /// source into one key. Only called speculatively from
    /// [`Parser::try_assign_target`].
    ///
    /// Adjacency is what keeps this honest: `- ` with a space around it is never
    /// joined, so a spaced expression cannot be silently reinterpreted.
    fn parse_hyphenated_path(&mut self) -> Option<Expr> {
        if self.peek() != Some(Lx::Dot) {
            return None;
        }
        self.pos += 1;
        let mut steps = Vec::new();
        loop {
            match self.peek() {
                Some(Lx::Ident) => steps.push(Step::Field(self.take_hyphenated_ident())),
                Some(Lx::Str) => {
                    steps.push(Step::Field(unescape(self.text())));
                    self.pos += 1;
                }
                Some(Lx::LBrack) => {
                    self.pos += 1;
                    if self.peek() == Some(Lx::Str) {
                        let key = unescape(self.text());
                        self.pos += 1;
                        if self.peek() != Some(Lx::RBrack) {
                            return None;
                        }
                        self.pos += 1;
                        steps.push(Step::Field(key));
                    } else {
                        let neg = self.peek() == Some(Lx::Minus);
                        if neg {
                            self.pos += 1;
                        }
                        if self.peek() != Some(Lx::Num) {
                            return None;
                        }
                        let n = parse_i64(self.text()).ok()?;
                        self.pos += 1;
                        if self.peek() != Some(Lx::RBrack) {
                            return None;
                        }
                        self.pos += 1;
                        steps.push(Step::Index(if neg { -n } else { n }));
                    }
                }
                _ => return None,
            }
            match self.peek() {
                Some(Lx::Dot) => self.pos += 1,
                Some(Lx::LBrack) => {}
                _ => break,
            }
        }
        Some(Expr::Path(steps))
    }

    /// Consume `Ident (- Ident)*` where every token abuts the last, and return
    /// the joined key.
    fn take_hyphenated_ident(&mut self) -> String {
        let mut name = self.text().to_string();
        self.pos += 1;
        while self.peek() == Some(Lx::Minus)
            && self.abuts_previous(self.pos)
            && self
                .toks
                .get(self.pos + 1)
                .is_some_and(|t| t.kind == Lx::Ident)
            && self.abuts_previous(self.pos + 1)
        {
            name.push('-');
            name.push_str(&self.toks[self.pos + 1].text);
            self.pos += 2;
        }
        name
    }

    /// Does token `i` start exactly where token `i - 1` ended? Whitespace
    /// between them means the user wrote an operator, not a key.
    fn abuts_previous(&self, i: usize) -> bool {
        match (self.toks.get(i.wrapping_sub(1)), self.toks.get(i)) {
            (Some(prev), Some(cur)) => prev.start + prev.text.len() == cur.start,
            _ => false,
        }
    }

    /// A hyphenated bare key that reached an assignment despite
    /// [`Parser::try_assign_target`], which means it was not a plain path (a
    /// pipe, a parenthesized expression). Name the fix rather than failing later
    /// with "left side of an assignment must be a path".
    fn reject_hyphen_key_lhs(&self, lhs: &Expr) -> Result<(), ParseError> {
        let Some((path, key)) = hyphen_key(lhs) else {
            return Ok(());
        };
        Err(self.err_here(format!(
            "key `{key}` contains `-` (parsed as subtraction); quote it: {path}"
        )))
    }

    /// `a // b`: right-associative, binding tighter than `=` (so
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
            // jq spells object entries `key: value`; TOML/KDL hands reach for
            // `key = value` when the target file is TOML. Accept both; the
            // expression parses before any file is read, so the grammar cannot
            // be conditioned on the target format.
            match self.peek() {
                Some(Lx::Colon) | Some(Lx::Assign) => self.pos += 1,
                _ => return Err(self.err_here("expected `:` or `=`")),
            }
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
                        // `.["key"]`: a field by name (dotted/special keys).
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
                    // Terminal: no navigation follows a comment.
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
/// Recognize the wreckage of a hyphenated bare key used as a path: a chain of
/// subtractions whose leftmost operand is a path ending in a field and whose
/// right operands are bare zero-argument calls (`.a.crate-type` lexes as
/// `.a.crate`, `-`, `type`). Returns the corrected, quoted path and the
/// offending key (`.a."crate-type"`, `crate-type`); `None` if the shape is
/// anything else.
fn hyphen_key(expr: &Expr) -> Option<(String, String)> {
    let mut tail: Vec<&str> = Vec::new();
    let mut cur = expr;
    while let Expr::Binary(BinOp::Sub, l, r) = cur {
        match r.as_ref() {
            Expr::Call(name, args) if args.is_empty() => tail.push(name.as_str()),
            _ => return None,
        }
        cur = l;
    }
    if tail.is_empty() {
        return None;
    }
    let Expr::Path(steps) = cur else {
        return None;
    };
    let Some((Step::Field(first), prefix)) = steps.split_last() else {
        return None;
    };
    tail.push(first.as_str());
    tail.reverse();
    let key = tail.join("-");
    let mut path = String::new();
    for step in prefix {
        match step {
            Step::Field(f) if is_bare_key(f) => {
                path.push('.');
                path.push_str(f);
            }
            Step::Field(f) => {
                path.push_str(&format!(".\"{f}\""));
            }
            Step::Index(i) => path.push_str(&format!("[{i}]")),
            _ => return None,
        }
    }
    path.push_str(&format!(".\"{key}\""));
    Some((path, key))
}

/// Would this key lex as a single bare `Ident` in a path?
fn is_bare_key(k: &str) -> bool {
    let mut chars = k.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

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
    fn doc_select_parses() {
        // `^dN | body`, `^dN.body`, and a bare `^dN` (identity body).
        assert_eq!(
            p("^d0 | .kind"),
            Expr::DocSelect(0, Box::new(Expr::Path(vec![Step::Field("kind".into())])))
        );
        assert_eq!(
            p("^d2.spec"),
            Expr::DocSelect(2, Box::new(Expr::Path(vec![Step::Field("spec".into())])))
        );
        assert_eq!(p("^d1"), Expr::DocSelect(1, Box::new(Expr::Path(vec![]))));
        // It scopes an assignment, and reports as a mutation.
        assert!(p("^d0 | .replicas = 3").is_mutation());
    }

    #[test]
    fn doc_select_bad_index_errors() {
        assert!(parse("^dfoo | .x").is_err());
        assert!(parse("^x").is_err());
        assert!(parse("^ | .x").is_err());
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
        // Comment is terminal: nothing may navigate past it.
        assert!(parse(".foo.#.bar").is_err());
    }

    #[test]
    fn object_construct_accepts_toml_equals() {
        // `key = value` (TOML inline-table spelling) and jq's `key: value`
        // both work, even mixed in one literal.
        assert_eq!(
            p(r#"{version = "1", optional: true}"#),
            Expr::ObjectConstruct(vec![
                ("version".into(), Expr::Literal(Value::Str("1".into()))),
                ("optional".into(), Expr::Literal(Value::Bool(true))),
            ])
        );
    }

    #[test]
    fn object_construct_names_both_separators() {
        let e = parse("{a 1}").unwrap_err();
        assert!(e.to_string().contains("expected `:` or `=`"), "got: {e}");
    }

    #[test]
    fn hyphen_hint_leaves_real_subtraction_alone() {
        // Query-position subtraction still parses as arithmetic.
        assert_eq!(
            p(".a-b"),
            Expr::Binary(
                BinOp::Sub,
                Box::new(Expr::Path(vec![Step::Field("a".into())])),
                Box::new(Expr::Call("b".into(), vec![]))
            )
        );
        // An LHS that is subtraction-but-not-a-bare-key shape (numeric
        // operand) is not intercepted; it still parses into an Assign for
        // the eval-time "must be a path" check.
        assert!(parse(".a - 1 = 2").is_ok());
        // RHS containing subtraction is untouched.
        p(".x = .a - .b");
    }

    /// The left of an assignment must be a path, so subtraction cannot occur
    /// there and a hyphen is unambiguously part of the key. No quoting needed.
    #[test]
    fn a_hyphenated_key_assigns_without_quoting() {
        let e = p(r#".dev-dependencies.serde_json = "1.0""#);
        let Expr::Assign(lhs, _) = e else {
            panic!("expected an assignment")
        };
        assert_eq!(
            *lhs,
            Expr::Path(vec![
                Step::Field("dev-dependencies".into()),
                Step::Field("serde_json".into()),
            ])
        );
    }

    #[test]
    fn hyphenated_keys_work_for_every_assignment_operator() {
        for src in [
            r#".package.rust-version = "1.85""#,
            ".lib.crate-type |= 1",
            ".a.b-c += 1",
            ".rust-version = 1",
        ] {
            p(src);
        }
    }

    #[test]
    fn a_multi_hyphen_key_is_one_field() {
        let Expr::Assign(lhs, _) = p(".lib.crate-type-x = 1") else {
            panic!("expected an assignment")
        };
        assert_eq!(
            *lhs,
            Expr::Path(vec![
                Step::Field("lib".into()),
                Step::Field("crate-type-x".into()),
            ])
        );
    }

    /// Indices and quoted keys still work alongside hyphenated ones.
    #[test]
    fn a_hyphenated_path_still_takes_indices_and_quoted_keys() {
        p(r#".bin[0].required-features = ["x"]"#);
        p(r#".target."cfg(unix)".dev-dependencies.libc = "0.2""#);
        p(r#".a.b-c[-1] = 1"#);
    }

    /// Adjacency is the guard. A spaced `-` is an operator, so this is still a
    /// subtraction and still an invalid assignment target.
    #[test]
    fn a_spaced_hyphen_is_not_absorbed_into_a_key() {
        assert!(parse(".a - b = 1").is_err());
    }

    /// Nothing outside assignment position changed: subtraction still parses.
    #[test]
    fn queries_are_untouched_by_the_assignment_rule() {
        p(".total - length");
        p(".a - .b");
        p(".count-1");
        p(".a.b.c");
    }

    /// A query with a hyphenated key remains ambiguous (subtraction is legal
    /// there), so it still gets the explanatory error rather than a guess.
    #[test]
    fn a_hyphenated_key_in_a_query_still_explains_itself() {
        let e = parse(".dev-dependencies.serde_json").unwrap_err();
        assert!(e.to_string().contains("contains `-`"), "{e}");
        assert!(e.to_string().contains(r#"."dev-dependencies""#), "{e}");
    }

    #[test]
    fn the_scanner_ignores_what_is_not_a_hyphenated_key() {
        assert!(hyphenated_key(".total - length").is_none());
        assert!(hyphenated_key(".count-1").is_none());
        assert!(hyphenated_key(".a.b.c").is_none());
        assert!(hyphenated_key("-").is_none());
        assert!(hyphenated_key("").is_none());
    }

    #[test]
    fn the_scanner_finds_the_key_and_its_offset() {
        assert_eq!(
            hyphenated_key(".dev-dependencies.x"),
            Some(("dev-dependencies".to_string(), 1))
        );
        assert_eq!(
            hyphenated_key(".a-b-c"),
            Some(("a-b-c".to_string(), 1)),
            "multiple hyphens are one key"
        );
    }

    /// An unrelated syntax error must keep its own message rather than being
    /// blamed on a hyphen elsewhere in the expression.
    #[test]
    fn an_unrelated_error_keeps_its_own_message() {
        let e = parse(".a | ").unwrap_err();
        assert!(!e.msg.contains("contains `-`"), "{}", e.msg);
    }
}
