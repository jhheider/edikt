//! Edit errors, shared across format modules.

/// A format-preserving edit failure (bad path, wrong type for the operation,
/// unsupported construct).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditError {
    pub msg: String,
}

impl EditError {
    pub fn new(msg: impl Into<String>) -> EditError {
        EditError { msg: msg.into() }
    }
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}
impl std::error::Error for EditError {}
