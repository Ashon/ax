//! POSIX shell quoting. Mirrors the Go `shellQuote` helper used when
//! building user-visible command strings (e.g. for display or for
//! `tmux new-session -c <dir> sh -c '<quoted command>'`).

/// Wrap `value` in POSIX single quotes, escaping any interior quotes via
/// the `'\''` idiom. Empty strings round-trip as `''`.
#[must_use]
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn empty_round_trips_to_double_quotes() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn plain_value_is_wrapped_in_single_quotes() {
        assert_eq!(shell_quote("hello"), "'hello'");
        assert_eq!(shell_quote("with spaces"), "'with spaces'");
    }

    #[test]
    fn embedded_single_quotes_are_escaped() {
        // Go's implementation produces `'it'\''s'` for `it's`.
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
