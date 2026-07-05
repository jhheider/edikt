//! edikt core.
//!
//! Home of the [`Value`] model, the [`Feature`] capability enum, and the
//! expression language (lexer, [`parse`], evaluator). The evaluator is built and
//! tested here against a plain in-memory [`Value`] so the language is correct
//! independent of any format-preserving CST; the format modules wire the same
//! AST to their CSTs.
//!
//! The format-agnostic `Document` and `Convert` seams arrive with the JSONC
//! slice, once there is a concrete CST to shape them against.

mod ast;
mod comment;
pub mod convert;
mod document;
mod error;
mod eval;
mod feature;
mod lexer;
mod parser;
mod strings;
mod value;

pub use ast::{BinOp, Expr, Step, render_path};
pub use comment::{CommentKind, Commented, CommentedNode, Comments, FlatEntry, flatten_commented};
pub use document::Document;
pub use error::EditError;
pub use eval::{EvalError, eval, eval_with_comments};
pub use feature::Feature;
pub use parser::{ParseError, parse};
pub use value::Value;
