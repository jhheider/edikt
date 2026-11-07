//! Unified diff helper for `--diff`.

use std::fmt::Write;

/// Produce a unified diff between `old` and `new` with the given file label.
pub fn unified_diff(old: &str, new: &str, label: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Simple LCS-based diff
    let lcs = lcs_table(&old_lines, &new_lines);
    // Backtrack to find diff
    let mut changes: Vec<(char, usize, usize)> = Vec::new(); // (op, old_idx, new_idx)
    let mut ii = old_lines.len();
    let mut jj = new_lines.len();
    while ii > 0 || jj > 0 {
        if ii > 0 && jj > 0 && old_lines[ii - 1] == new_lines[jj - 1] {
            changes.push((' ', ii - 1, jj - 1));
            ii -= 1;
            jj -= 1;
        } else if jj > 0 && (ii == 0 || lcs[ii][jj - 1] >= lcs[ii - 1][jj]) {
            changes.push(('+', ii, jj - 1));
            jj -= 1;
        } else if ii > 0 {
            changes.push(('-', ii - 1, jj));
            ii -= 1;
        } else {
            break;
        }
    }
    changes.reverse();

    // Group into hunks with 3 lines of context
    let context = 3;

    // We need to process with context windows
    // Simpler: just output all changes with context
    let mut output_lines: Vec<(char, &str)> = Vec::new();
    for (op, oi, ni) in &changes {
        match op {
            ' ' => output_lines.push((' ', old_lines[*oi])),
            '-' => output_lines.push(('-', old_lines[*oi])),
            '+' => output_lines.push(('+', new_lines[*ni])),
            _ => {}
        }
    }

    // Now format as unified diff
    // Find ranges of changes
    let mut i = 0;
    let mut hunks_out: Vec<String> = Vec::new();
    while i < output_lines.len() {
        // Find next change
        while i < output_lines.len() && output_lines[i].0 == ' ' {
            i += 1;
        }
        if i >= output_lines.len() {
            break;
        }
        // Start hunk with context
        let start = i.saturating_sub(context);
        let mut end = i;
        // Find end of change region
        while end < output_lines.len() {
            if output_lines[end].0 != ' ' {
                // extend
                end += 1;
            } else {
                // check if there's another change within context
                let mut j = end;
                let mut found_change = false;
                while j < output_lines.len() && j < end + 2 * context + 1 {
                    if output_lines[j].0 != ' ' {
                        found_change = true;
                        break;
                    }
                    j += 1;
                }
                if found_change {
                    end = j;
                } else {
                    break;
                }
            }
        }
        let end = (end + context).min(output_lines.len());

        // Build hunk
        let mut hunk = String::new();
        let old_start = count_old_before(&output_lines, start);
        let new_start = count_new_before(&output_lines, start);
        let mut old_count = 0;
        let mut new_count = 0;
        for item in &output_lines[start..end] {
            match item.0 {
                ' ' => {
                    old_count += 1;
                    new_count += 1;
                }
                '-' => {
                    old_count += 1;
                }
                '+' => {
                    new_count += 1;
                }
                _ => {}
            }
        }
        let _ = writeln!(
            hunk,
            "@@ -{},{} +{},{} @@",
            old_start + 1,
            old_count,
            new_start + 1,
            new_count
        );
        for item in &output_lines[start..end] {
            let _ = writeln!(hunk, "{}{}", item.0, item.1);
        }
        hunks_out.push(hunk);
        i = end;
    }

    if hunks_out.is_empty() {
        return String::new();
    }

    let mut result = format!("--- a/{label}\n+++ b/{label}\n");
    for h in hunks_out {
        result.push_str(&h);
    }
    result
}

fn lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<usize>> {
    let m = a.len();
    let n = b.len();
    let mut table = vec![vec![0usize; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                table[i][j] = table[i - 1][j - 1] + 1;
            } else {
                table[i][j] = table[i - 1][j].max(table[i][j - 1]);
            }
        }
    }
    table
}

fn count_old_before(lines: &[(char, &str)], idx: usize) -> usize {
    lines[..idx].iter().filter(|l| l.0 != '+').count()
}

fn count_new_before(lines: &[(char, &str)], idx: usize) -> usize {
    lines[..idx].iter().filter(|l| l.0 != '-').count()
}
