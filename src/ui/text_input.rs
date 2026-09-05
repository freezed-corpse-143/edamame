//! Shared paste policy for single-line modal text fields.
//!
//! Every text-input modal holds a *single line*, while a bracketed paste can
//! carry newlines and arbitrarily large content.  [`sanitize_paste`] is the one
//! place that flattens the two: control characters are dropped and the result is
//! capped at [`PASTE_CHAR_CAP`].  Field-specific filtering (the digits-only
//! insert-table fields, say) layers on top in each state's own `paste`.

/// Maximum characters one paste may contribute to a field: room for long paths
/// and search terms, but not a whole document.
pub const PASTE_CHAR_CAP: usize = 1024;

/// Flatten a bracketed paste for a single-line field: drop control characters,
/// truncate to [`PASTE_CHAR_CAP`].
pub fn sanitize_paste(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(PASTE_CHAR_CAP)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_newlines_and_other_control_chars() {
        assert_eq!(sanitize_paste("a\nb\r\nc\td"), "abcd");
    }

    #[test]
    fn keeps_printable_unicode() {
        assert_eq!(sanitize_paste("naïve — café"), "naïve — café");
    }

    #[test]
    fn caps_at_the_char_limit() {
        let huge = "x".repeat(PASTE_CHAR_CAP + 500);
        assert_eq!(sanitize_paste(&huge).chars().count(), PASTE_CHAR_CAP);
    }

    #[test]
    fn cap_counts_chars_not_bytes() {
        // The cap is a character count, so nothing truncates mid-codepoint.
        let huge = "é".repeat(PASTE_CHAR_CAP + 10);
        assert_eq!(sanitize_paste(&huge).chars().count(), PASTE_CHAR_CAP);
    }
}
