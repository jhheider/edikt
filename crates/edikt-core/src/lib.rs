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
mod builtins;
mod comment;
mod comment_edit;
pub mod convert;
mod document;
mod error;
mod eval;
mod feature;
mod lexer;
#[macro_use]
mod macros;
mod parser;
mod strings;
mod value;
pub mod wrap;

pub use ast::{BinOp, Expr, Step, render_path};
pub use comment::{CommentKind, Commented, CommentedNode, Comments, FlatEntry, flatten_commented};
pub use comment_edit::{apply_comment_mutation, line_index, place_line_comment};
pub use document::Document;
pub use error::EditError;
pub use eval::{EvalError, eval, eval_with_comments, expand_delete_paths, expand_iter_paths};
pub use feature::Feature;
pub use parser::{ParseError, hyphen_hint_for_unknown_function, parse};
pub use value::Value;
