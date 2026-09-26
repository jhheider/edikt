//! Byte-level file conventions every format shares: line endings and the UTF-8
//! byte-order mark.
//!
//! The rule (see the contract's moat section): an untouched line keeps its own
//! line ending, a line an edit inserts takes the file's **dominant** ending,
//! and a leading BOM survives any edit. Formats build new text with `\n` and
//! pass it through [`to_eol`] with the ending [`dominant`] picked.

use std::borrow::Cow;

/// The UTF-8 byte-order mark, as a `char`.
pub const BOM: char = '\u{FEFF}';

/// Split a leading byte-order mark off `src`: whether it had one, and the rest.
///
/// Parsers see the text after the mark (it is not part of any key); the
/// document remembers the flag and [`with_bom`] puts it back on serialize.
pub fn split_bom(src: &str) -> (bool, &str) {
    match src.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, src),
    }
}

/// `body`, with the byte-order mark restored if the source had one.
pub fn with_bom(bom: bool, body: String) -> String {
    if bom {
        let mut out = String::with_capacity(body.len() + BOM.len_utf8());
        out.push(BOM);
        out.push_str(&body);
        out
    } else {
        body
    }
}

/// The line ending most of `src`'s lines use: `"\r\n"` when CRLF lines
/// outnumber bare-LF ones, else `"\n"` (a file with no line breaks is LF).
pub fn dominant(src: &str) -> &'static str {
    let (crlf, lf) = count_endings(src);
    if crlf > lf { "\r\n" } else { "\n" }
}

/// (CRLF count, bare-LF count).
fn count_endings(src: &str) -> (usize, usize) {
    let bytes = src.as_bytes();
    let mut crlf = 0;
    let mut lf = 0;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            if i > 0 && bytes[i - 1] == b'\r' {
                crlf += 1;
            } else {
                lf += 1;
            }
        }
    }
    (crlf, lf)
}

/// The line ending that terminates `line` (`"\r\n"`, `"\n"`, or `""` at EOF).
pub fn ending_of(line: &str) -> &'static str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// Split `line` into its content and its terminator (see [`ending_of`]).
#[doc(hidden)]
pub fn split_ending(line: &str) -> (&str, &str) {
    line.split_at(line.len() - ending_of(line).len())
}

/// The byte offset of the start of the line containing `pos`.
#[doc(hidden)]
pub fn line_start(src: &str, pos: usize) -> usize {
    src[..pos].rfind('\n').map_or(0, |i| i + 1)
}

/// The run of spaces and tabs `s` starts with: a line's indentation, when `s`
/// starts at the line's first byte.
#[doc(hidden)]
pub fn leading_indent(s: &str) -> &str {
    &s[..s.len() - s.trim_start_matches([' ', '\t']).len()]
}

/// Respell every bare `\n` in generated text as `eol`. A `\n` already preceded
/// by `\r` is left alone, so the call is idempotent. Only for text an edit
/// generates; never run it over a file's own bytes.
pub fn to_eol<'a>(fragment: &'a str, eol: &str) -> Cow<'a, str> {
    if eol == "\n" || !fragment.contains('\n') {
        return Cow::Borrowed(fragment);
    }
    let mut out = String::with_capacity(fragment.len() + fragment.len() / 16);
    let mut prev = '\0';
    for c in fragment.chars() {
        if c == '\n' && prev != '\r' {
            out.push_str(eol);
        } else {
            out.push(c);
        }
        prev = c;
    }
    Cow::Owned(out)
}

/// `edited` with [`to_eol`] applied only to the text an insertion added: the
/// span between the longest common prefix and suffix it shares with
/// `original`. For a splice that keeps the original bytes on both sides of new
/// text (an element appended before a closing bracket), so the new lines match
/// the file while every original byte, bare `\n`s included, stays as it was.
pub fn eol_inserted(original: &str, edited: &str, eol: &str) -> String {
    if eol == "\n" {
        return edited.to_string();
    }
    let (o, e) = (original.as_bytes(), edited.as_bytes());
    let mut pre = o.iter().zip(e).take_while(|(a, b)| a == b).count();
    // Back off to a char boundary; the suffix is capped so the two never
    // overlap in either string.
    while !edited.is_char_boundary(pre) {
        pre -= 1;
    }
    let max_suf = o.len().min(e.len()) - pre;
    let mut suf = o
        .iter()
        .rev()
        .zip(e.iter().rev())
        .take(max_suf)
        .take_while(|(a, b)| a == b)
        .count();
    while !edited.is_char_boundary(e.len() - suf) {
        suf -= 1;
    }
    let mid = &edited[pre..e.len() - suf];
    // A `\n` right after the kept prefix's `\r` is already a CRLF.
    let fixed = if mid.starts_with('\n') && edited[..pre].ends_with('\r') {
        format!("\n{}", to_eol(&mid[1..], eol))
    } else {
        to_eol(mid, eol).into_owned()
    };
    format!("{}{fixed}{}", &edited[..pre], &edited[e.len() - suf..])
}

/// Re-apply `original`'s line endings to `output`, a re-serialization of it
/// that came back with LF endings only (a library that normalizes, such as
/// `toml_edit`, which drops every `\r` it writes).
///
/// A line whose text survived from the original gets that line's own ending
/// back; a line the edit introduced gets the original's [`dominant`] one. A
/// uniform file (every line CRLF, or none) needs no matching: each bare `\n`
/// becomes the one ending. A mixed file aligns lines by a longest-common-
/// subsequence match over the region between the common prefix and suffix.
/// `output`'s own `\r\n`s are never doubled.
///
/// Works line by line, never inside a line, so a multi-line string's lines are
/// matched like any other: an untouched one keeps its exact ending.
pub fn restore_endings(original: &str, output: &str) -> String {
    let (crlf, lf) = count_endings(original);
    if crlf == 0 {
        return output.to_string();
    }
    let eol = dominant(original);
    if lf == 0 {
        return to_eol(output, eol).into_owned();
    }
    let orig: Vec<&str> = original.split_inclusive('\n').collect();
    let out: Vec<&str> = output.split_inclusive('\n').collect();
    let matched = align(&orig, &out, body);
    let mut s = String::with_capacity(output.len() + output.len() / 16);
    for (i, line) in out.iter().enumerate() {
        if !line.ends_with('\n') || line.ends_with("\r\n") {
            s.push_str(line);
            continue;
        }
        s.push_str(body(line));
        let ending = matched[i]
            .map(|j| ending_of(orig[j]))
            .filter(|e| !e.is_empty())
            .unwrap_or(eol);
        s.push_str(ending);
    }
    s
}

/// A line without its ending.
fn body(line: &str) -> &str {
    let l = line.strip_suffix('\n').unwrap_or(line);
    l.strip_suffix('\r').unwrap_or(l)
}

/// For each line of `b`, the index of the line of `a` it matches (by `key`),
/// if any: common prefix and suffix first, then an LCS over what's left. A
/// middle too large for the quadratic table falls back to unmatched (dominant
/// endings), which only a huge edit to a mixed-ending file can reach.
fn align<'a>(a: &[&'a str], b: &[&'a str], key: impl Fn(&'a str) -> &'a str) -> Vec<Option<usize>> {
    let mut m = vec![None; b.len()];
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && key(a[pre]) == key(b[pre]) {
        m[pre] = Some(pre);
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre
        && suf < b.len() - pre
        && key(a[a.len() - 1 - suf]) == key(b[b.len() - 1 - suf])
    {
        m[b.len() - 1 - suf] = Some(a.len() - 1 - suf);
        suf += 1;
    }
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    const MAX_CELLS: usize = 4_000_000;
    if am.is_empty() || bm.is_empty() || am.len().saturating_mul(bm.len()) > MAX_CELLS {
        return m;
    }
    // lcs[i][j] = LCS length of am[i..] and bm[j..].
    let w = bm.len() + 1;
    let mut lcs = vec![0u32; (am.len() + 1) * w];
    for i in (0..am.len()).rev() {
        for j in (0..bm.len()).rev() {
            lcs[i * w + j] = if key(am[i]) == key(bm[j]) {
                lcs[(i + 1) * w + j + 1] + 1
            } else {
                lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < am.len() && j < bm.len() {
        if key(am[i]) == key(bm[j]) {
            m[pre + j] = Some(pre + i);
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * w + j] >= lcs[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dominant_counts_lines() {
        assert_eq!(dominant("a\r\nb\r\nc\n"), "\r\n");
        assert_eq!(dominant("a\r\nb\nc\n"), "\n");
        assert_eq!(dominant("a\r\nb\n"), "\n", "a tie is LF");
        assert_eq!(dominant("no breaks"), "\n");
    }

    #[test]
    fn to_eol_is_idempotent() {
        assert_eq!(to_eol("a\nb\n", "\r\n"), "a\r\nb\r\n");
        assert_eq!(to_eol("a\r\nb\n", "\r\n"), "a\r\nb\r\n");
        assert_eq!(to_eol("a\nb\n", "\n"), "a\nb\n");
    }

    #[test]
    fn eol_inserted_touches_only_new_text() {
        // Appending before the closing bracket of a CRLF array.
        assert_eq!(
            eol_inserted("[\r\n  1\r\n]", "[\r\n  1,\n  2\r\n]", "\r\n"),
            "[\r\n  1,\r\n  2\r\n]"
        );
        // An original bare LF next to the insertion is left alone.
        assert_eq!(
            eol_inserted("[\n  1\r\n]", "[\n  1,\n  2\r\n]", "\r\n"),
            "[\n  1,\r\n  2\r\n]"
        );
        assert_eq!(eol_inserted("[1]", "[1, 2]", "\r\n"), "[1, 2]");
        assert_eq!(eol_inserted("a\n", "a\nb\n", "\n"), "a\nb\n");
    }

    #[test]
    fn bom_round_trips() {
        let (bom, rest) = split_bom("\u{FEFF}a=1\n");
        assert!(bom);
        assert_eq!(rest, "a=1\n");
        assert_eq!(with_bom(bom, rest.to_string()), "\u{FEFF}a=1\n");
        assert_eq!(split_bom("a"), (false, "a"));
    }

    #[test]
    fn restore_uniform() {
        assert_eq!(restore_endings("a\nb\n", "a\nc\n"), "a\nc\n");
        assert_eq!(
            restore_endings("a\r\nb\r\n", "a\nb\nc\n"),
            "a\r\nb\r\nc\r\n"
        );
        // Never doubles a CR the output already has.
        assert_eq!(restore_endings("a\r\n", "a\r\nb\n"), "a\r\nb\r\n");
        // An unterminated last line stays unterminated.
        assert_eq!(restore_endings("a\r\nb", "a\nb"), "a\r\nb");
    }

    #[test]
    fn restore_mixed_keeps_each_lines_ending() {
        let orig = "a\r\nb\nc\r\nd\r\n";
        // Identity.
        assert_eq!(restore_endings(orig, "a\nb\nc\nd\n"), orig);
        // A changed or inserted line takes the dominant ending.
        assert_eq!(
            restore_endings(orig, "a\nB\nnew\nc\nd\n"),
            "a\r\nB\r\nnew\r\nc\r\nd\r\n"
        );
        assert_eq!(restore_endings(orig, "x\nb\nc\nd\n"), "x\r\nb\nc\r\nd\r\n");
        // A deleted line takes nothing else's ending with it.
        assert_eq!(restore_endings(orig, "a\nb\nd\n"), "a\r\nb\nd\r\n");
    }
}
