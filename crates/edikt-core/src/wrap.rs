//! Comment wrapping: the shared "use the space as it exists" rule.
//!
//! Head/foot comment text wraps to the document's own width envelope so a
//! comment never makes the file wider than it already is; the caller supplies
//! the node's indent and the delimiter width (`"# "` = 2, `"// "` = 3). Inline
//! comments never wrap; the caller skips this. See the wrapping section of
//! `docs/design/comments-as-first-class.md`.

/// The absolute wrap column for `source`: `clamp(longest line, 80, 100)`. The
/// ceiling stops a wide file yielding 200-char comment lines; the floor keeps a
/// flat `.env` from wrapping prose to its 9-char keys.
pub fn wrap_width(source: &str) -> usize {
    let longest = source.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    longest.clamp(80, 100)
}

/// Wrap `text` into lines whose rendered form (`<indent><delim><line>`) fits in
/// `width` columns. Greedy fill; a single word longer than the budget overflows
/// rather than hard-breaking. Returns the text of each line (no indent/delim);
/// always at least one line (empty for empty text).
pub fn wrap_comment(text: &str, width: usize, indent: usize, delim_cols: usize) -> Vec<String> {
    let budget = width.saturating_sub(indent + delim_cols).max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in text.split_whitespace() {
        let w = word.chars().count();
        if cur.is_empty() {
            cur.push_str(word);
            cur_len = w;
        } else if cur_len + 1 + w <= budget {
            cur.push(' ');
            cur.push_str(word);
            cur_len += 1 + w;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_len = w;
        }
    }
    lines.push(cur);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_clamps_to_the_band() {
        assert_eq!(wrap_width("PORT=8080\n"), 80); // narrow -> floor
        assert_eq!(wrap_width(&format!("{}\n", "x".repeat(90))), 90); // tracks
        assert_eq!(wrap_width(&format!("{}\n", "x".repeat(200))), 100); // ceiling
        assert_eq!(wrap_width(""), 80); // empty -> floor
    }

    #[test]
    fn wraps_greedily_to_the_budget() {
        // width 20, indent 0, delim "# " (2) -> budget 18.
        let out = wrap_comment("the quick brown fox jumps", 20, 0, 2);
        assert_eq!(out, vec!["the quick brown", "fox jumps"]);
    }

    #[test]
    fn long_word_overflows_not_breaks() {
        let out = wrap_comment("see https://example.com/a/very/long/path now", 30, 0, 2);
        // The URL is longer than the 28-col budget but stays on its own line.
        assert_eq!(
            out,
            vec!["see", "https://example.com/a/very/long/path", "now"]
        );
    }

    #[test]
    fn empty_text_is_one_empty_line() {
        assert_eq!(wrap_comment("", 80, 0, 2), vec![String::new()]);
    }
}
