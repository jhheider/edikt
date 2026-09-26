//! jq-style array indices: a negative index counts from the end.
//!
//! Every place that resolves `.a[i]` (the evaluator, path resolution, comment
//! lookup, each format's CST walk) goes through these two, so "what does
//! `[-1]` name" has one answer.

/// Normalize `i` against a sequence of length `len`: a negative index counts
/// from the end. `None` when it lands before the start; a result `>= len` is
/// past the end (an append position for `len`, out of range beyond it).
pub fn normalize_index(i: i64, len: usize) -> Option<usize> {
    let idx = if i < 0 { len as i64 + i } else { i };
    usize::try_from(idx).ok()
}

/// Resolve `i` to an existing element's position in a sequence of length
/// `len` (negative counts from the end), or `None` when out of range.
pub fn resolve_index(i: i64, len: usize) -> Option<usize> {
    normalize_index(i, len).filter(|&n| n < len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_counts_from_the_end() {
        assert_eq!(normalize_index(-1, 3), Some(2));
        assert_eq!(normalize_index(-3, 3), Some(0));
        assert_eq!(normalize_index(-4, 3), None);
        assert_eq!(normalize_index(3, 3), Some(3));
        assert_eq!(resolve_index(-1, 3), Some(2));
        assert_eq!(resolve_index(3, 3), None);
        assert_eq!(resolve_index(-4, 3), None);
        assert_eq!(resolve_index(0, 0), None);
    }
}
