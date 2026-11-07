//! Minimal ANSI colorization for JSON output.

/// Colorize a JSON string with ANSI escape codes.
pub fn colorize_json(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + s.len() / 4);
    let mut chars = s.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(c) = chars.next() {
        if escape {
            result.push(c);
            escape = false;
            continue;
        }
        if in_string {
            if c == '\\' {
                escape = true;
                result.push(c);
            } else if c == '"' {
                in_string = false;
                result.push(c);
                result.push_str("\x1b[0m");
            } else {
                result.push(c);
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                result.push_str("\x1b[32m"); // green for strings
                result.push(c);
            }
            '{' | '[' | '}' | ']' | ',' => {
                result.push_str("\x1b[90m"); // dim for structural
                result.push(c);
                result.push_str("\x1b[0m");
            }
            ':' => {
                result.push_str("\x1b[90m");
                result.push(c);
                result.push_str("\x1b[0m");
            }
            't' | 'f' => {
                result.push_str("\x1b[35m"); // magenta for booleans
                result.push(c);
            }
            'n' => {
                result.push_str("\x1b[35m"); // magenta for null
                result.push(c);
            }
            c if c.is_ascii_digit() || c == '-' || c == '.' => {
                result.push_str("\x1b[33m"); // yellow for numbers
                result.push(c);
            }
            ' ' | '\n' | '\t' | '\r' => {
                result.push(c);
            }
            _ => {
                result.push(c);
            }
        }
    }
    result.push_str("\x1b[0m");
    result
}
