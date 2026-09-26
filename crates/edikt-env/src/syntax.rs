//! Syntax kinds and the rowan `Language` for `.env` / `.properties`.

edikt_syntax::syntax_kinds! {
    /// Token and node kinds: tokens first, then nodes.
    pub enum Sk {
        // tokens
        Ws,
        Newline,
        Comment,
        Key,
        Sep,    // `=` or `:`
        ValStr, // value text (trimmed core)
        Error,
        // nodes
        Value, // wraps an entry's value text (possibly empty)
        Entry, // one `key=value` line, including its terminator
        Root,
    }
}

pub type EnvLang = edikt_syntax::Lang<Sk>;

pub type SyntaxNode = rowan::SyntaxNode<EnvLang>;
