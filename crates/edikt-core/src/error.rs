//! Edit errors, shared across format modules.

/// A format-preserving edit failure (bad path, wrong type for the operation,
/// unsupported construct).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{msg}")]
pub struct EditError {
    pub msg: String,
}

impl EditError {
    pub fn new(msg: impl Into<String>) -> EditError {
        EditError { msg: msg.into() }
    }
}

/// An evaluation failure inside an edit (a right side that errors, a path
/// that steps into the wrong type) is an edit failure with the same message.
impl From<crate::eval::EvalError> for EditError {
    fn from(e: crate::eval::EvalError) -> EditError {
        EditError::new(e.to_string())
    }
}
