//! Spelling a string value as KDL text the way the value it replaces was
//! spelled (jhheider/edikt#81): quoted, raw, or a bare identifier.

use kdl::KdlValue;

/// `value`'s text, spelled like the value it replaces (`old`, empty for a new
/// entry) when both are strings (jhheider/edikt#81). A raw string stays raw
/// (`#"..."#`, gaining a `#` if the value contains its closing delimiter), a
/// quoted string stays quoted, and a bare identifier string stays bare while
/// the value is still a valid identifier. Anything a spelling can't hold
/// falls back to quoted, which spells any string; a multi-line string (`"""`)
/// has no one-line form to keep, so it becomes quoted too.
pub(crate) fn spell_like(old: &str, value: &KdlValue) -> String {
    let KdlValue::String(s) = value else {
        return value.to_string();
    };
    let hashes = old.bytes().take_while(|&b| b == b'#').count();
    let after = &old[hashes..];
    if hashes > 0 && after.starts_with('"') {
        if !after.starts_with("\"\"\"")
            && let Some(raw) = raw_string(s, hashes)
        {
            return raw;
        }
    } else if !old.starts_with('"') {
        // kdl-rs spells a string bare exactly when it is a valid identifier.
        let bare = value.to_string();
        if !bare.starts_with('"') {
            return bare;
        }
    }
    quoted_string(s)
}

/// `s` as a one-line raw string with at least `min_hashes` hashes, or `None`
/// if it holds a character a raw string can't carry. A leading `"` would read
/// as a multi-line opener, so it can't either.
fn raw_string(s: &str, min_hashes: usize) -> Option<String> {
    if s.starts_with('"') || s.chars().any(|c| kdl_newline(c) || kdl_disallowed(c)) {
        return None;
    }
    let mut hashes = "#".repeat(min_hashes);
    while s.contains(&format!("\"{hashes}")) {
        hashes.push('#');
    }
    Some(format!("{hashes}\"{s}\"{hashes}"))
}

/// `s` as a quoted string, escaping what KDL can't hold literally on one line.
fn quoted_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if kdl_newline(c) || kdl_disallowed(c) => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A KDL newline character (other than the `\n`/`\r` handled by name).
fn kdl_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// A code point KDL forbids anywhere in a document, which a quoted string
/// must escape.
fn kdl_disallowed(c: char) -> bool {
    matches!(c,
        '\u{0}'..='\u{8}'
        | '\u{e}'..='\u{1f}'
        | '\u{7f}'
        | '\u{200e}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}'
        | '\u{feff}')
}
