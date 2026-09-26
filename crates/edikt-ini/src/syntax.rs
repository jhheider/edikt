//! Syntax kinds and the rowan `Language` for INI.

edikt_syntax::syntax_kinds! {
    /// Token and node kinds: tokens first, then nodes.
    pub enum Sk {
        // tokens
        Ws,
        Newline,
        Comment,
        Open,   // `[`
        Close,  // `]`
        Name,   // section name
        Key,    // entry key
        Sep,    // `=` or `:`
        ValStr, // entry value text (trimmed core)
        Error,
        // nodes
        Value,   // wraps the value text of an entry (possibly empty)
        Entry,   // one `key = value` line, including its terminator
        Header,  // one `[section]` line, including its terminator
        Section, // a header (optional, absent for the preamble) plus its entries/trivia
        Root,
    }
}

pub type IniLang = edikt_syntax::Lang<Sk>;

pub type SyntaxNode = rowan::SyntaxNode<IniLang>;
