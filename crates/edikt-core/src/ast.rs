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
    /// `left // right` — jq's alternative: `left`'s truthy outputs, or —
    /// when there are none (a miss, `null`, `false`) — `right`'s.
    Alternative(Box<Expr>, Box<Expr>),
    /// `a, b, c` — concatenate output streams.
    Comma(Vec<Expr>),
    /// A function call, e.g. `length`, `select(.x == 1)`, `ltrimstr("pre")`.
    Call(String, Vec<Expr>),
    /// `[ expr ]` — collect the inner stream into an array (`None` = `[]`).
    Collect(Option<Box<Expr>>),
    /// `{ key: expr, ... }` — construct an object.
    ObjectConstruct(Vec<(String, Expr)>),
    /// `path = rhs` — assign; `rhs` is evaluated against the whole input.
    Assign(Box<Expr>, Box<Expr>),
    /// `path |= rhs` — update-assign; `rhs` sees the current value at `path`.
    UpdateAssign(Box<Expr>, Box<Expr>),
    /// `path += rhs` — add-assign; `path = path + rhs` (numeric add, string/array
    /// concat). `rhs` is evaluated against the whole input.
    AddAssign(Box<Expr>, Box<Expr>),
}

impl Expr {
    /// Does this expression mutate the document (contains an assignment or a
    /// `del(...)`)? The CLI uses this to pick mutation mode vs query mode.
    pub fn is_mutation(&self) -> bool {
        match self {
            Expr::Assign(..) | Expr::UpdateAssign(..) | Expr::AddAssign(..) => true,
            Expr::Call(name, args) => name == "del" || args.iter().any(Expr::is_mutation),
            Expr::Pipe(a, b) => a.is_mutation() || b.is_mutation(),
            Expr::Alternative(a, b) => a.is_mutation() || b.is_mutation(),
            Expr::Comma(items) => items.iter().any(Expr::is_mutation),
            Expr::Neg(inner) => inner.is_mutation(),
            Expr::Binary(_, a, b) => a.is_mutation() || b.is_mutation(),
            Expr::Collect(inner) => inner.as_ref().is_some_and(|e| e.is_mutation()),
            Expr::ObjectConstruct(pairs) => pairs.iter().any(|(_, e)| e.is_mutation()),
            Expr::Path(_) | Expr::Literal(_) => false,
        }
    }

    /// The path steps if this expression is a plain path (the only valid left
    /// side of an assignment), else `None`.
    pub fn as_path(&self) -> Option<&[Step]> {
        match self {
            Expr::Path(steps) => Some(steps),
            _ => None,
        }
    }
}

/// Render a path of steps back to jq-ish source text (`.a.b[0]`, `.["a.b"]`) for
/// error messages. An empty path is `.` (identity).
pub fn render_path(steps: &[Step]) -> String {
    if steps.is_empty() {
        return ".".to_string();
    }
    let mut out = String::new();
    for step in steps {
        match step {
            Step::Field(k) if is_bare_ident(k) => {
                out.push('.');
                out.push_str(k);
            }
            // Non-identifier keys use the bracket-string form so the rendered path
            // is itself a valid expression.
            Step::Field(k) => out.push_str(&format!(".[{k:?}]")),
            Step::Index(i) => out.push_str(&format!("[{i}]")),
            Step::Iterate => out.push_str("[]"),
        }
    }
    out
}

/// Whether `k` is a bare identifier that needs no quoting in a path (`.foo`).
fn is_bare_ident(k: &str) -> bool {
    let mut chars = k.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_paths() {
        assert_eq!(render_path(&[]), ".");
        assert_eq!(
            render_path(&[Step::Field("a".into()), Step::Field("b".into())]),
            ".a.b"
        );
        assert_eq!(
            render_path(&[Step::Field("arr".into()), Step::Index(0)]),
            ".arr[0]"
        );
        assert_eq!(
            render_path(&[Step::Field("xs".into()), Step::Iterate]),
            ".xs[]"
        );
        // A dotted key can't be a bare identifier — bracket-quote it.
        assert_eq!(render_path(&[Step::Field("a.b".into())]), ".[\"a.b\"]");
    }
}
