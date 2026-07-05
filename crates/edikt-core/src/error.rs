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
