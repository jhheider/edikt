//! The expression AST.
//!
//! The v1 query language desugars dotted/indexed paths into a `Path` of steps,
//! so the evaluator only deals with a handful of node kinds. Mutation forms
//! (`=`, `|=`, `+=`, `del`) are not parsed yet — they arrive with M2.

use crate::value::Value;

/// One navigation step within a path, applied to the current input.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// `.field` or `."quoted"` — object member access.
    Field(String),
    /// `[n]` — array index (negative counts from the end).
    Index(i64),
    /// `[]` — iterate array elements / object values.
    Iterate,
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

/// An expression node.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A path from the current input; an empty step list is identity (`.`).
    Path(Vec<Step>),
    /// A literal scalar value.
    Literal(Value),
    /// Arithmetic negation.
    Neg(Box<Expr>),
    /// A binary operation.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `left | right` — pipe each output of `left` into `right`.
    Pipe(Box<Expr>, Box<Expr>),
    /// `a, b, c` — concatenate output streams.
    Comma(Vec<Expr>),
    /// A function call, e.g. `length`, `select(.x == 1)`, `ltrimstr("pre")`.
    Call(String, Vec<Expr>),
}
