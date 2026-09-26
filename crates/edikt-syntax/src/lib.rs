//! edikt shared syntax substrate.
//!
//! The `rowan` plumbing every rowan-backed format (`edikt-jsonc`, `edikt-ini`,
//! `edikt-env`) shares. `rowan` is re-exported so each format crate pins the
//! same version through one dependency.
//!
//! - [`syntax_kinds!`] declares a format's kind enum; [`Lang`] is the rowan
//!   `Language` over it, so no format hand-writes a raw-kind table.
//! - [`Builder`] is a `GreenNodeBuilder` that takes the format's kinds
//!   directly, with [`Builder::trivia`] for optional whitespace/newlines.
//! - [`leaf_node`] builds the one-token replacement node a value edit splices
//!   in.
//! - [`tokens`] walks every token under a node (comment and error scans).
//! - [`to_source`] is lossless serialization, which is *free* with rowan: a
//!   green tree stores every token including trivia, so concatenating token
//!   text reproduces the source byte-for-byte.

pub use rowan;

use rowan::{GreenNode, GreenNodeBuilder, Language, SyntaxNode};
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;

/// A format's syntax-kind enum: tokens and nodes, discriminants contiguous
/// from 0. Implemented by [`syntax_kinds!`]; not meant to be written by hand.
pub trait Kinds: Copy + Debug + Eq + Ord + Hash + 'static {
    /// Every kind, in discriminant order.
    const ALL: &'static [Self];

    /// The kind's discriminant.
    fn raw(self) -> u16;
}

/// Declare a syntax-kind enum (`#[repr(u16)]`, contiguous from 0) and its
/// [`Kinds`] table in one place.
///
/// ```
/// edikt_syntax::syntax_kinds! {
///     pub enum Sk { Ws, Word, Root }
/// }
/// type SyntaxNode = edikt_syntax::rowan::SyntaxNode<edikt_syntax::Lang<Sk>>;
/// ```
#[macro_export]
macro_rules! syntax_kinds {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident { $($(#[$vmeta:meta])* $variant:ident),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u16)]
        $vis enum $name {
            $($(#[$vmeta])* $variant),*
        }

        impl $crate::Kinds for $name {
            const ALL: &'static [Self] = &[$($name::$variant),*];
            fn raw(self) -> u16 {
                self as u16
            }
        }
    };
}

/// The rowan [`Language`] over a format's kinds `K`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lang<K>(PhantomData<K>);

impl<K: Kinds> Language for Lang<K> {
    type Kind = K;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> K {
        K::ALL[raw.0 as usize]
    }

    fn kind_to_raw(kind: K) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind.raw())
    }
}

/// A green-tree builder that speaks the format's kinds.
pub struct Builder<K> {
    inner: GreenNodeBuilder<'static>,
    kinds: PhantomData<K>,
}

impl<K: Kinds> Default for Builder<K> {
    fn default() -> Self {
        Builder {
            inner: GreenNodeBuilder::new(),
            kinds: PhantomData,
        }
    }
}

impl<K: Kinds> Builder<K> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_node(&mut self, kind: K) {
        self.inner.start_node(rowan::SyntaxKind(kind.raw()));
    }

    pub fn finish_node(&mut self) {
        self.inner.finish_node();
    }

    pub fn token(&mut self, kind: K, text: &str) {
        self.inner.token(rowan::SyntaxKind(kind.raw()), text);
    }

    /// A token for optional text (indentation, a line terminator): nothing
    /// when `text` is empty, since an empty token is noise in the tree.
    pub fn trivia(&mut self, kind: K, text: &str) {
        if !text.is_empty() {
            self.token(kind, text);
        }
    }

    pub fn finish(self) -> GreenNode {
        self.inner.finish()
    }
}

/// A `node` holding a single `token` of `text` (an empty `node` for empty
/// text): the replacement a scalar edit splices in for a value slot.
pub fn leaf_node<K: Kinds>(node: K, token: K, text: &str) -> GreenNode {
    let mut b = Builder::new();
    b.start_node(node);
    b.trivia(token, text);
    b.finish_node();
    b.finish()
}

/// Every token under `node`, in source order.
pub fn tokens<L: Language>(node: &SyntaxNode<L>) -> impl Iterator<Item = rowan::SyntaxToken<L>> {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
}

/// Serialize a syntax tree back to source, byte-identically for an unedited
/// tree. This is lossless because the green tree retains all trivia.
pub fn to_source<L: Language>(node: &SyntaxNode<L>) -> String {
    node.text().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    syntax_kinds! {
        enum Sk { Ws, Word, Leaf, Root }
    }
    type Node = SyntaxNode<Lang<Sk>>;

    #[test]
    fn kinds_round_trip_through_rowan() {
        let mut b = Builder::new();
        b.start_node(Sk::Root);
        b.trivia(Sk::Ws, "");
        b.trivia(Sk::Ws, "  ");
        b.token(Sk::Word, "hi");
        b.finish_node();
        let root = Node::new_root(b.finish());
        assert_eq!(root.kind(), Sk::Root);
        let kinds: Vec<Sk> = root.children_with_tokens().map(|e| e.kind()).collect();
        assert_eq!(kinds, [Sk::Ws, Sk::Word]);
        assert_eq!(to_source(&root), "  hi");
    }

    #[test]
    fn leaf_node_is_empty_for_empty_text() {
        let leaf = Node::new_root(leaf_node(Sk::Leaf, Sk::Word, "v"));
        assert_eq!((leaf.kind(), to_source(&leaf)), (Sk::Leaf, "v".to_string()));
        let empty = Node::new_root(leaf_node(Sk::Leaf, Sk::Word, ""));
        assert_eq!(empty.children_with_tokens().count(), 0);
    }
}
