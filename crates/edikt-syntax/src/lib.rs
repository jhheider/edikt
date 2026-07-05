//! edikt shared syntax substrate.
//!
//! The `rowan` plumbing every format shares. `rowan` is re-exported so each
//! format crate pins the same version through one dependency.
//!
//! Lossless serialization is *free* with rowan: a green tree stores every token
//! including trivia, so walking it and concatenating token text reproduces the
//! source byte-for-byte. [`to_source`] is that walk. Structural-sharing edit
//! helpers (for M2) will join it here once there is a second rowan-backed format
//! to share them with.

pub use rowan;

use rowan::{Language, SyntaxNode};

/// Serialize a syntax tree back to source, byte-identically for an unedited
/// tree. This is lossless because the green tree retains all trivia.
pub fn to_source<L: Language>(node: &SyntaxNode<L>) -> String {
    node.text().to_string()
}
