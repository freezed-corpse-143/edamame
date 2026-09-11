//! Backslash escapes for search queries. Search is literal substring matching (regex is
//! confined to `:s`), but the query needs a way to express a line break: `/  \n`. A
//! backslash always introduces an escape (a literal one is `\\`), and an unknown escape is
//! an error rather than silently literal, so `\d` tells the user search is not a regex.
//! [`decode`] runs on user-typed text; [`escape`] is its inverse for text edamame supplies
//! (`*` / `#` keyword, a paste).

/// A malformed escape in a search query; `Display` is the hint-line text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EscapeError {
    #[error("Unsupported escape: \\{0} (use \\\\ for a literal backslash)")]
    Unsupported(char),
    #[error("Trailing backslash (use \\\\ for a literal backslash)")]
    Trailing,
}

/// Decode a user-typed query into the literal needle: `\n`, `\t`, `\r`, `\\`; anything else
/// is [`EscapeError::Unsupported`], a lone trailing backslash [`EscapeError::Trailing`].
pub fn decode(input: &str) -> Result<String, EscapeError> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => return Err(EscapeError::Unsupported(other)),
            None => return Err(EscapeError::Trailing),
        }
    }
    Ok(out)
}

/// Encode a literal so [`decode`] round-trips it unchanged.
pub fn escape(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len());
    for c in literal.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_supported_escapes() {
        assert_eq!(decode(r"  \n").unwrap(), "  \n");
        assert_eq!(decode(r"a\tb").unwrap(), "a\tb");
        assert_eq!(decode(r"a\rb").unwrap(), "a\rb");
        assert_eq!(decode(r"C:\\dir").unwrap(), r"C:\dir");
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(decode("foo bar").unwrap(), "foo bar");
        assert_eq!(decode("").unwrap(), "");
    }

    #[test]
    fn an_escaped_backslash_does_not_start_a_new_escape() {
        assert_eq!(decode(r"\\n").unwrap(), r"\n");
        assert_eq!(decode(r"\\\n").unwrap(), "\\\n");
    }

    #[test]
    fn unknown_and_trailing_escapes_error() {
        assert_eq!(decode(r"\d"), Err(EscapeError::Unsupported('d')));
        assert_eq!(decode(r"a\"), Err(EscapeError::Trailing));
        assert_eq!(decode(r"\"), Err(EscapeError::Trailing));
    }

    #[test]
    fn escape_round_trips_through_decode() {
        for literal in [
            "plain",
            "back\\slash",
            "line\nbreak",
            "tab\there",
            "\\n",
            "\\",
            "mixed \\ and \n and \t",
        ] {
            assert_eq!(
                decode(&escape(literal)).as_deref(),
                Ok(literal),
                "round-trip failed for {literal:?}"
            );
        }
    }

    #[test]
    fn escape_leaves_ordinary_text_alone() {
        assert_eq!(escape("foo bar"), "foo bar");
        assert_eq!(escape("café"), "café");
    }
}
