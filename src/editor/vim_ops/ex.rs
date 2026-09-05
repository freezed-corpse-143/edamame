//! Ex-command parsing and substitution.
//!
//! [`parse_ex`] is a *pure* parser from the text after the leading `:` to an [`ExCommand`].
//! `:w` / `:q` / `:wq` become App effects the reducer bubbles up as outcomes; `:s` / `:%s` are
//! executed here by [`execute_substitute`], as one [`EditDelta`] so a whole `:%s/…/…/g` undoes
//! in a single step.  This is the only place a regex engine is used — the `/` search path stays
//! literal substring + smartcase.
//!
//! **The pattern sees the whole range at once**, not one line at a time
//! ([`region_haystack`] + [`for_each_region_match`]), so it may match across a line break.
//! Three properties hold that together: `multi_line(true)` at both compile sites keeps `^`/`$`
//! anchoring per line, the region excludes the last line's own break so a match can never
//! escape the range, and the non-`g` walk replaces the first match *starting on* each line.
//!
//! **Vim syntax in, vim syntax out**: the pattern is translated by
//! [`vim_regex::translate_pattern`](super::vim_regex::translate_pattern) and the replacement
//! expanded per match by
//! [`vim_regex::expand_replacement`](super::vim_regex::expand_replacement).  The engine is
//! `fancy-regex`, not `regex`, so backreferences and the lookaround `\<`/`\>` translate to are
//! available.  An escaped delimiter (`\/`) is reduced to a literal during parsing.

use fancy_regex::{Regex, RegexBuilder};

use crate::document::EditDelta;
use crate::editor::vim_ops::vim_regex::{expand_replacement, translate_pattern};
use crate::editor::EditorState;

/// A parsed ex command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExCommand {
    /// `:w` — write the buffer.
    Write,
    /// `:w <path>` — write a snapshot *without* changing the buffer's own path (real-vim
    /// `:w {file}` semantics).  `force` is a trailing `!`, skipping the overwrite prompt.
    WriteCopy { path: String, force: bool },
    /// `:saveas <path>` — write and *adopt* the path, so later `:w` target it.
    WriteAs { path: String, force: bool },
    /// `:saveas` with no argument — prompt for a path, even on an already-named buffer.
    SaveAsPrompt,
    /// `:q` — quit (dirty-guarded).
    Quit,
    /// `:wq` — write then quit.
    WriteQuit,
    /// `:wq <path>` — copy semantics like `:w <path>`, then quit.
    WriteQuitCopy { path: String, force: bool },
    /// `:x` — write only when modified, then quit (`:wq` always writes).
    WriteQuitIfModified,
    /// `:s/…` (current line), `:%s/…` (whole file), or `:'<,'>s/…` (the
    /// last visual selection's line span).
    Substitute(Substitution),
    /// `:42` — jump to a 1-based line number.  `:$` parses to `GoToLine(u32::MAX)`, which the
    /// motion layer clamps like any other overshoot.
    GoToLine(u32),
}

/// Which lines a substitution runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstituteRange {
    /// `:s` — the current line only.
    CurrentLine,
    /// `:%s` — every line in the buffer.
    AllLines,
    /// `:'<,'>s` — the last visual selection's line span, resolved against the bounds threaded
    /// into [`execute_substitute`].
    VisualRange,
}

/// A parsed `:s` / `:%s` / `:'<,'>s` substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitution {
    /// The line span the substitution runs over.
    pub range: SubstituteRange,
    /// The vim regex pattern, escaped delimiters already reduced.
    pub pattern: String,
    /// The vim replacement text (`\1` / `&` / `\U…\E`), expanded per match.
    pub replacement: String,
    /// Whether the second delimiter was typed (`:s/foo/` vs `:s/foo`).  Both parse to an empty
    /// `replacement`, but the live preview must tell "still typing the pattern" from "replace
    /// with nothing".  The execute path ignores it — both delete.
    pub replacement_present: bool,
    /// `g` flag — replace every match on a line, not just the first.
    pub global: bool,
    /// `i` flag — case-insensitive matching.
    pub ignore_case: bool,
}

/// A parse- or execution-time ex error; its `Display` is what the reducer flashes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExError {
    #[error("Not an editor command: {0}")]
    UnknownCommand(String),
    #[error("Unknown flag: {0}")]
    UnknownFlag(char),
    #[error("Empty search pattern")]
    EmptyPattern,
    #[error("Unsupported vim pattern: {0}")]
    UnsupportedPattern(String),
    #[error("Invalid pattern: {0}")]
    InvalidRegex(String),
}

/// Parse the text after the leading `:` into an [`ExCommand`].
pub fn parse_ex(input: &str) -> Result<ExCommand, ExError> {
    let s = input.trim();

    // The `'<,'>` marks vim inserts when `:` is pressed in Visual.  Only qualifies a `:s`.
    let (visual_range, s) = match s.strip_prefix("'<,'>") {
        Some(rest) => (true, rest.trim_start()),
        None => (false, s),
    };

    // A bare `:s` ("repeat last substitution") is out of scope, so a delimiter must follow.
    // A `%` overrides any `'<,'>` prefix, matching vim's last-range-wins rule.
    if let Some(rest) = s.strip_prefix("%s") {
        return parse_substitute(SubstituteRange::AllLines, rest);
    }
    if let Some(rest) = s.strip_prefix('s') {
        if rest.starts_with('/') {
            let range = if visual_range {
                SubstituteRange::VisualRange
            } else {
                SubstituteRange::CurrentLine
            };
            return parse_substitute(range, rest);
        }
    }

    // The write / quit family ignores a `'<,'>` prefix and acts on the whole buffer — `:` in
    // Visual auto-inserts marks the user never typed.  It also can't go through the exact-match
    // table below, because its members may carry a path argument.
    if let Some(cmd) = parse_write_forms(s) {
        return Ok(cmd);
    }

    // A bare line address, only without a `'<,'>` prefix.  Out-of-range numbers are clamped by
    // the motion layer, not rejected here; one too large for `u32` saturates to the same place.
    if !visual_range {
        if s == "$" {
            return Ok(ExCommand::GoToLine(u32::MAX));
        }
        if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
            return Ok(ExCommand::GoToLine(s.parse().unwrap_or(u32::MAX)));
        }
    }

    match s {
        "q" | "quit" => Ok(ExCommand::Quit),
        "x" | "xit" => Ok(ExCommand::WriteQuitIfModified),
        // Keep any `'<,'>` prefix in the message so the user sees what failed to parse.
        _ if visual_range => Err(ExError::UnknownCommand(input.trim().to_owned())),
        other => Err(ExError::UnknownCommand(other.to_owned())),
    }
}

/// Parse the write / save-as family — `:w[!]`, `:write[!]`, `:saveas[!]`, `:wq[!]`, each
/// optionally with a path.  `None` for anything else, so the caller's exact-match table can
/// handle `:q`, `:x`, …
fn parse_write_forms(s: &str) -> Option<ExCommand> {
    // `s` is already outer-trimmed, so the remainder is the path verbatim — internal spaces
    // preserved.
    let (head, rest) = match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    };
    let force = head.ends_with('!');
    let word = head.strip_suffix('!').unwrap_or(head);
    let path = (!rest.is_empty()).then(|| rest.to_owned());
    match (word, path) {
        // `:saveas` re-points the buffer; bare, it prompts for a name.
        ("saveas", Some(path)) => Some(ExCommand::WriteAs { path, force }),
        ("saveas", None) => Some(ExCommand::SaveAsPrompt),
        // `:w <path>` writes a copy and keeps the current file (real vim).
        ("w" | "write", Some(path)) => Some(ExCommand::WriteCopy { path, force }),
        ("w" | "write", None) => Some(ExCommand::Write),
        ("wq", Some(path)) => Some(ExCommand::WriteQuitCopy { path, force }),
        ("wq", None) => Some(ExCommand::WriteQuit),
        _ => None,
    }
}

/// Parse the `/pat/rep/flags` tail of a substitution.  `rest` is everything
/// after the `s` / `%s` prefix and must begin with the `/` delimiter.
fn parse_substitute(range: SubstituteRange, rest: &str) -> Result<ExCommand, ExError> {
    const DELIM: char = '/';
    let Some(body) = rest.strip_prefix(DELIM) else {
        let prefix = match range {
            SubstituteRange::AllLines => "%s",
            SubstituteRange::VisualRange => "'<,'>s",
            SubstituteRange::CurrentLine => "s",
        };
        return Err(ExError::UnknownCommand(format!("{prefix}{rest}")));
    };

    let (pattern, after_pattern) = take_field(body, DELIM);
    let replacement_present = after_pattern.is_some();
    // No second delimiter (`:s/foo`) → empty replacement, no flags.
    let (replacement, flags) = match after_pattern {
        None => (String::new(), ""),
        Some(after) => {
            let (rep, after_rep) = take_field(after, DELIM);
            (rep, after_rep.unwrap_or(""))
        }
    };

    let mut global = false;
    let mut ignore_case = false;
    for c in flags.chars() {
        match c {
            'g' => global = true,
            'i' => ignore_case = true,
            c if c.is_whitespace() => {}
            c => return Err(ExError::UnknownFlag(c)),
        }
    }

    Ok(ExCommand::Substitute(Substitution {
        range,
        pattern,
        replacement,
        replacement_present,
        global,
        ignore_case,
    }))
}

/// Take one delimiter-terminated field from `s`, returning the field text — with an escaped
/// delimiter reduced to a literal, every other escape kept for the regex engine — and the slice
/// after the terminator, or `None` when the field runs to the end.
fn take_field(s: &str, delim: char) -> (String, Option<&str>) {
    let mut out = String::new();
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            if c == delim {
                out.push(delim);
            } else {
                out.push('\\');
                out.push(c);
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == delim {
            return (out, Some(&s[i + c.len_utf8()..]));
        } else {
            out.push(c);
        }
    }
    // A trailing backslash with nothing after it is kept literally.
    if escaped {
        out.push('\\');
    }
    (out, None)
}

/// Execute a substitution and return the number of matches replaced; `Ok(0)` when the pattern
/// never matched, recording no edit.  See the module docs for the range and dialect rules.
///
/// `visual_range` is the inclusive `(first, last)` line span for a
/// [`SubstituteRange::VisualRange`]; ignored for the other ranges, and a `VisualRange` with no
/// bounds falls back to the current line.
pub fn execute_substitute(
    editor: &mut EditorState,
    sub: &Substitution,
    visual_range: Option<(usize, usize)>,
) -> Result<usize, ExError> {
    if sub.pattern.is_empty() {
        return Err(ExError::EmptyPattern);
    }
    let translated = translate_pattern(&sub.pattern)?;
    // `multi_line` keeps `^`/`$` anchoring per line even though the pattern sees the whole
    // range; `dot_matches_new_line` stays off so `.` still refuses to cross a break, as in vim.
    let re = RegexBuilder::new(&translated)
        .case_insensitive(sub.ignore_case)
        .multi_line(true)
        .build()
        .map_err(|e| ExError::InvalidRegex(e.to_string()))?;

    let cursor_line = editor.buffer.char_to_line(editor.cursor.offset);
    let Some(edit) = build_substitution(&editor.buffer, cursor_line, &re, sub, visual_range, None)?
    else {
        return Ok(0);
    };
    let count = edit.count;
    let range_first = edit.range_first;
    editor.apply_delta(edit.delta);
    // Park the cursor at the start of the first affected line, not at the end of the inserted
    // region (`apply_delta`'s default), which for `:%s` would jump to end-of-document.
    // `range_first` still names the same text after a shrinking multi-line match: every line
    // before the first match is byte-identical pre/post.  The `min` covers a range whose own
    // first line was consumed.
    let target = editor
        .buffer
        .line_to_char(range_first.min(editor.buffer.line_count().saturating_sub(1)));
    editor.place_cursor(target);
    Ok(count)
}

/// Resolve a substitution's inclusive `(first, last)` line span; `None` only for an empty
/// buffer.
pub(crate) fn resolve_substitute_lines(
    buffer: &crate::document::Buffer,
    cursor_line: usize,
    range: SubstituteRange,
    visual_range: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let line_count = buffer.line_count();
    if line_count == 0 {
        return None;
    }
    Some(match range {
        SubstituteRange::AllLines => (0, line_count - 1),
        SubstituteRange::CurrentLine => (cursor_line, cursor_line),
        SubstituteRange::VisualRange => match visual_range {
            Some((f, l)) => (f.min(line_count - 1), l.min(line_count - 1)),
            None => (cursor_line, cursor_line),
        },
    })
}

/// The fully-computed edit for one substitution, produced by [`build_substitution`] against an
/// unmodified buffer.  The shared seam between the commit path and the live preview.
pub(crate) struct SubstitutionEdit {
    /// The single char-offset delta rewriting `range_first` through the last scanned line.
    pub delta: EditDelta,
    /// Total matches replaced.
    pub count: usize,
    /// Post-apply byte ranges of each inserted replacement, absolute in the rewritten buffer.
    pub replaced_ranges: Vec<std::ops::Range<usize>>,
    /// First line of the resolved range; where the commit path parks the cursor.
    pub range_first: usize,
    /// First line that actually matched (where the preview scrolls to).
    pub first_match_line: usize,
}

/// The text of lines `first..=last` as one string, plus the char and byte offsets it starts at.
///
/// The last line's own break is **excluded**, and that is the entire enforcement of the range
/// bound: a `\n` pattern can never consume the break separating `last` from the line after it,
/// so `:'<,'>s` cannot edit outside the selection.  One divergence from real vim follows — a
/// single-line `:s/\n//` cannot join with the next line.
///
/// `:%s` resolves `last` to ropey's phantom line after a trailing newline, which has no break to
/// strip, so it does see (and may consume) the file's final newline.
pub(crate) fn region_haystack(
    buffer: &crate::document::Buffer,
    first: usize,
    last: usize,
) -> (String, usize, usize) {
    let start_char = buffer.line_to_char(first);
    let last_line = buffer.rope().line(last);
    let end_char = buffer.line_to_char(last) + last_line.len_chars() - line_break_len(last_line);
    let hay = buffer.rope().slice(start_char..end_char).to_string();
    let start_byte = buffer.rope().char_to_byte(start_char);
    (hay, start_char, start_byte)
}

/// Length in chars of the line-break ending `line`, or 0 for the buffer's last line.
///
/// Not a bare `strip_suffix('\n')`: ropey splits lines on the full Unicode set (lone `\r`, VT,
/// FF, NEL, LS, PS as well as LF).  `\r\n` never reaches here — text is normalized on load —
/// but a lone `\r` in content still splits a line.  Each break is one char, which keeps `(?m)$`
/// anchoring in the same place.
fn line_break_len(line: ropey::RopeSlice) -> usize {
    let n = line.len_chars();
    if n == 0 {
        return 0;
    }
    let last = line.char(n - 1);
    usize::from(matches!(
        last,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    ))
}

/// Visit every match a substitution over `hay` would act on, in document order, passing each
/// one's captures and the **buffer line its match starts on**.  `Ok(false)` when `on_match`
/// broke out (the preview's match cap).
///
/// The single match-finding implementation, driven by the commit path and both preview modes,
/// so what the preview highlights is by construction what Enter replaces.
///
/// - **`global`** delegates to `captures_iter`, keeping the engine's empty-match advancement
///   rules unchanged.
/// - **Non-global** implements vim's per-line rule — the first match *starting on* each line —
///   resuming at the line after the last one the match covered.  `captures_from_pos` (not
///   `captures(&hay[pos..])`) keeps the preceding text as context, so `(?m)^` fires only after
///   a real line break and lookbehind still sees what precedes.  The resume is strictly past
///   the match start even for a zero-width match, so the loop always terminates.
pub(crate) fn for_each_region_match<F>(
    buffer: &crate::document::Buffer,
    base_byte: usize,
    hay: &str,
    re: &Regex,
    global: bool,
    mut on_match: F,
) -> Result<bool, ExError>
where
    F: FnMut(&fancy_regex::Captures<'_, str>, usize) -> std::ops::ControlFlow<()>,
{
    if global {
        for cap in re.captures_iter(hay) {
            let caps = cap.map_err(|e| ExError::InvalidRegex(e.to_string()))?;
            let start = caps.get(0).expect("group 0 is always present").start();
            if on_match(&caps, buffer.byte_to_line(base_byte + start)).is_break() {
                return Ok(false);
            }
        }
        return Ok(true);
    }

    let mut pos = 0usize;
    while pos <= hay.len() {
        let Some(caps) = re
            .captures_from_pos(hay, pos)
            .map_err(|e| ExError::InvalidRegex(e.to_string()))?
        else {
            return Ok(true);
        };
        let whole = caps.get(0).expect("group 0 is always present");
        let start_line = buffer.byte_to_line(base_byte + whole.start());
        if on_match(&caps, start_line).is_break() {
            return Ok(false);
        }
        // Last line the match actually covered.  A non-empty match ending exactly at a line
        // start stopped *before* that line's first char, so that line is still eligible;
        // without this correction the scan would skip it wholesale.
        let end_line = buffer.byte_to_line(base_byte + whole.end());
        let covered = if whole.end() > whole.start()
            && base_byte + whole.end() == buffer.rope().line_to_byte(end_line)
        {
            end_line.saturating_sub(1).max(start_line)
        } else {
            end_line.max(start_line)
        };
        // Resume at the next line's start, or end the walk when that is past the region.
        let next = covered + 1;
        pos = if next >= buffer.line_count() {
            hay.len() + 1
        } else {
            match buffer.rope().line_to_byte(next).checked_sub(base_byte) {
                Some(rel) if rel <= hay.len() => rel,
                _ => hay.len() + 1,
            }
        };
    }
    Ok(true)
}

/// Build the combined edit for a substitution without applying it.  `Ok(None)` when the pattern
/// never matched (or the buffer is empty) — the commit path turns that into "Pattern not found".
///
/// `match_cap` bounds the walk for the live preview, stopping on a **match** boundary rather
/// than a line boundary: `removed` is then the prefix of the region `inserted` actually
/// transformed, which stays a verbatim slice of buffer text however matches straddle lines.
/// The commit path passes `None`.
pub(crate) fn build_substitution(
    buffer: &crate::document::Buffer,
    cursor_line: usize,
    re: &Regex,
    sub: &Substitution,
    visual_range: Option<(usize, usize)>,
    match_cap: Option<usize>,
) -> Result<Option<SubstitutionEdit>, ExError> {
    let Some((first, last)) =
        resolve_substitute_lines(buffer, cursor_line, sub.range, visual_range)
    else {
        return Ok(None);
    };
    let (hay, start_char, base_byte) = region_haystack(buffer, first, last);

    // `out`'s byte 0 sits at `base_byte` and text before the region is untouched, so a span in
    // `out` is already a valid absolute post-apply byte range.
    let mut out = String::new();
    let mut copied = 0usize;
    let mut total = 0usize;
    let mut replaced_ranges = Vec::new();
    let mut first_match_line = None;

    let completed =
        for_each_region_match(buffer, base_byte, &hay, re, sub.global, |caps, line| {
            let whole = caps.get(0).expect("group 0 is always present");
            out.push_str(&hay[copied..whole.start()]);
            let span_start = out.len();
            out.push_str(&expand_replacement(&sub.replacement, caps));
            replaced_ranges.push(base_byte + span_start..base_byte + out.len());
            copied = whole.end();
            total += 1;
            first_match_line.get_or_insert(line);
            if match_cap.is_some_and(|cap| total >= cap) {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        })?;

    if total == 0 {
        return Ok(None);
    }
    // `copied` is always a match end, hence a char boundary in both strings.
    let removed = if completed {
        out.push_str(&hay[copied..]);
        hay
    } else {
        hay[..copied].to_owned()
    };

    Ok(Some(SubstitutionEdit {
        delta: EditDelta {
            offset: start_char,
            removed,
            inserted: out,
        },
        count: total,
        replaced_ranges,
        range_first: first,
        first_match_line: first_match_line.unwrap_or(first),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `rep: None` models a missing replacement field (`:s/foo`, no second delimiter).
    fn sub(
        range: SubstituteRange,
        pat: &str,
        rep: Option<&str>,
        global: bool,
        ignore_case: bool,
    ) -> ExCommand {
        ExCommand::Substitute(Substitution {
            range,
            pattern: pat.to_owned(),
            replacement: rep.unwrap_or("").to_owned(),
            replacement_present: rep.is_some(),
            global,
            ignore_case,
        })
    }

    use SubstituteRange::{AllLines, CurrentLine, VisualRange};

    #[test]
    fn parses_write_quit_variants() {
        assert_eq!(parse_ex("w"), Ok(ExCommand::Write));
        assert_eq!(parse_ex("write"), Ok(ExCommand::Write));
        assert_eq!(parse_ex("q"), Ok(ExCommand::Quit));
        assert_eq!(parse_ex("wq"), Ok(ExCommand::WriteQuit));
        assert_eq!(parse_ex("x"), Ok(ExCommand::WriteQuitIfModified));
        assert_eq!(parse_ex("xit"), Ok(ExCommand::WriteQuitIfModified));
        // Leading / trailing whitespace is tolerated.
        assert_eq!(parse_ex("  w  "), Ok(ExCommand::Write));
    }

    #[test]
    fn parses_write_as_forms() {
        // `:w <path>` / `:write <path>` write a copy (keep the current file).
        assert_eq!(
            parse_ex("w notes.md"),
            Ok(ExCommand::WriteCopy {
                path: "notes.md".to_owned(),
                force: false,
            })
        );
        assert_eq!(
            parse_ex("write notes.md"),
            Ok(ExCommand::WriteCopy {
                path: "notes.md".to_owned(),
                force: false,
            })
        );
        // `:saveas <path>` re-points the buffer at the new path.
        assert_eq!(
            parse_ex("saveas notes.md"),
            Ok(ExCommand::WriteAs {
                path: "notes.md".to_owned(),
                force: false,
            })
        );
        // A bare `:saveas` prompts for a path.
        assert_eq!(parse_ex("saveas"), Ok(ExCommand::SaveAsPrompt));
        // `:wq <path>` writes a copy, then quits.
        assert_eq!(
            parse_ex("wq out.md"),
            Ok(ExCommand::WriteQuitCopy {
                path: "out.md".to_owned(),
                force: false,
            })
        );
        // A bare `:w!` writes to the current path (force needs no path).
        assert_eq!(parse_ex("w!"), Ok(ExCommand::Write));
        // `!` on a named destination sets force (skips the overwrite prompt).
        assert_eq!(
            parse_ex("w! notes.md"),
            Ok(ExCommand::WriteCopy {
                path: "notes.md".to_owned(),
                force: true,
            })
        );
        assert_eq!(
            parse_ex("saveas! out.md"),
            Ok(ExCommand::WriteAs {
                path: "out.md".to_owned(),
                force: true,
            })
        );
        assert_eq!(
            parse_ex("wq! out.md"),
            Ok(ExCommand::WriteQuitCopy {
                path: "out.md".to_owned(),
                force: true,
            })
        );
        // Internal spaces in the path are preserved.
        assert_eq!(
            parse_ex("w my file.md"),
            Ok(ExCommand::WriteCopy {
                path: "my file.md".to_owned(),
                force: false,
            })
        );
    }

    #[test]
    fn parses_a_bare_line_address() {
        assert_eq!(parse_ex("42"), Ok(ExCommand::GoToLine(42)));
        assert_eq!(parse_ex(" 1 "), Ok(ExCommand::GoToLine(1)));
        // `:$` is the last line; the motion layer does the clamping.
        assert_eq!(parse_ex("$"), Ok(ExCommand::GoToLine(u32::MAX)));
        // A number past `u32` saturates rather than erroring.
        assert_eq!(
            parse_ex("99999999999999"),
            Ok(ExCommand::GoToLine(u32::MAX))
        );
        // A visual range prefix is not a line address.
        assert_eq!(
            parse_ex("'<,'>42"),
            Err(ExError::UnknownCommand("'<,'>42".to_owned()))
        );
    }

    #[test]
    fn unknown_command_errors() {
        assert_eq!(
            parse_ex("nope"),
            Err(ExError::UnknownCommand("nope".to_owned()))
        );
        // A bare `s` with no delimiter is not a known command.
        assert_eq!(parse_ex("s"), Err(ExError::UnknownCommand("s".to_owned())));
    }

    #[test]
    fn parses_line_substitution() {
        assert_eq!(
            parse_ex("s/foo/bar/"),
            Ok(sub(CurrentLine, "foo", Some("bar"), false, false))
        );
        // Trailing delimiter optional.
        assert_eq!(
            parse_ex("s/foo/bar"),
            Ok(sub(CurrentLine, "foo", Some("bar"), false, false))
        );
        // Missing replacement deletes the match.
        assert_eq!(
            parse_ex("s/foo"),
            Ok(sub(CurrentLine, "foo", None, false, false))
        );
    }

    #[test]
    fn replacement_present_tracks_the_second_delimiter() {
        // Both parse to an empty replacement; the live preview keys highlight-only vs.
        // deletion-preview off which one typed the second delimiter.
        assert_eq!(
            parse_ex("s/foo/"),
            Ok(sub(CurrentLine, "foo", Some(""), false, false))
        );
        // An escaped delimiter is field content, not a terminator.
        assert_eq!(
            parse_ex(r"s/foo\/bar"),
            Ok(sub(CurrentLine, "foo/bar", None, false, false))
        );
    }

    #[test]
    fn parses_global_substitution_and_flags() {
        assert_eq!(
            parse_ex("%s/a/b/g"),
            Ok(sub(AllLines, "a", Some("b"), true, false))
        );
        assert_eq!(
            parse_ex("%s/a/b/gi"),
            Ok(sub(AllLines, "a", Some("b"), true, true))
        );
        assert_eq!(
            parse_ex("s/a/b/i"),
            Ok(sub(CurrentLine, "a", Some("b"), false, true))
        );
    }

    #[test]
    fn parses_visual_range_substitution() {
        // The `'<,'>` marks vim inserts when `:` is pressed in Visual mode.
        assert_eq!(
            parse_ex("'<,'>s/foo/bar/g"),
            Ok(sub(VisualRange, "foo", Some("bar"), true, false))
        );
        // A `%` after the range wins (last range specifier wins, as in vim).
        assert_eq!(
            parse_ex("'<,'>%s/a/b/"),
            Ok(sub(AllLines, "a", Some("b"), false, false))
        );
        // The write / quit family ignores a `'<,'>` prefix and acts on the whole buffer.
        assert_eq!(parse_ex("'<,'>w"), Ok(ExCommand::Write));
        assert_eq!(parse_ex("'<,'>wq"), Ok(ExCommand::WriteQuit));
        assert_eq!(parse_ex("'<,'>q"), Ok(ExCommand::Quit));
        assert_eq!(parse_ex("'<,'>x"), Ok(ExCommand::WriteQuitIfModified));
        // A genuinely unknown command keeps the prefix in the error.
        assert_eq!(
            parse_ex("'<,'>nope"),
            Err(ExError::UnknownCommand("'<,'>nope".to_owned()))
        );
    }

    #[test]
    fn unknown_flag_errors() {
        assert_eq!(parse_ex("s/a/b/z"), Err(ExError::UnknownFlag('z')));
    }

    #[test]
    fn escaped_delimiter_is_literal() {
        // `\/` in the pattern is a literal slash; regex escapes survive.
        assert_eq!(
            parse_ex(r"s/a\/b/c\.d/"),
            Ok(sub(CurrentLine, "a/b", Some(r"c\.d"), false, false))
        );
    }

    // ── Region matching ───────────────────────────────────────────────────

    use crate::document::Buffer;

    /// Compile a vim pattern exactly as the commit path does.
    fn re(pattern: &str) -> Regex {
        RegexBuilder::new(&translate_pattern(pattern).expect("translatable"))
            .multi_line(true)
            .build()
            .expect("compiles")
    }

    /// Unwrap the `Substitution` out of the `sub()` helper's `ExCommand`.
    fn substitution(cmd: ExCommand) -> Substitution {
        match cmd {
            ExCommand::Substitute(s) => s,
            other => panic!("not a substitution: {other:?}"),
        }
    }

    #[test]
    fn region_haystack_excludes_the_last_lines_own_newline() {
        let b = Buffer::from_str("a\nb\nc\n");
        // The break after "b" belongs to line 1 and is dropped, so nothing can reach line 2.
        assert_eq!(region_haystack(&b, 0, 1).0, "a\nb");
        // `:%s` resolves `last` to the phantom line after the trailing newline, so the region
        // really is the whole file.
        assert_eq!(region_haystack(&b, 0, b.line_count() - 1).0, "a\nb\nc\n");
        // A buffer with no trailing newline loses nothing either.
        let b2 = Buffer::from_str("a\nb");
        assert_eq!(region_haystack(&b2, 0, b2.line_count() - 1).0, "a\nb");
        // Offsets are the region's start, not the buffer's.
        let (hay, start_char, start_byte) = region_haystack(&b, 1, 1);
        assert_eq!((hay.as_str(), start_char, start_byte), ("b", 2, 2));
    }

    #[test]
    fn non_global_skips_lines_consumed_by_a_multiline_match() {
        let b = Buffer::from_str("a\nb\nc\nd\ne");
        let s = substitution(sub(AllLines, r".\n.", Some("X"), false, false));
        let edit = build_substitution(&b, 0, &re(&s.pattern), &s, None, None)
            .unwrap()
            .expect("matched");
        // Match 1 covers lines 0-1, so the scan resumes at line 2; match 2 covers 2-3.
        assert_eq!(edit.count, 2);
        assert_eq!(edit.delta.inserted, "X\nX\ne");
    }

    #[test]
    fn a_match_ending_at_a_line_start_leaves_that_line_eligible() {
        // Pattern `\n` ends exactly on the next line's first byte; a resume rule of
        // `end_line + 1` would silently skip every other line.
        let b = Buffer::from_str("a\nb\nc\nd");
        let s = substitution(sub(AllLines, r"\n", Some("-"), false, false));
        let edit = build_substitution(&b, 0, &re(&s.pattern), &s, None, None)
            .unwrap()
            .expect("matched");
        assert_eq!(edit.count, 3, "one per line, none skipped");
        assert_eq!(edit.delta.inserted, "a-b-c-d");
    }

    #[test]
    fn match_cap_truncates_at_a_match_boundary() {
        let b = Buffer::from_str("a\na\na\na\na\n");
        let s = substitution(sub(AllLines, r"a\n", Some("b"), true, false));
        let edit = build_substitution(&b, 0, &re(&s.pattern), &s, None, Some(2))
            .unwrap()
            .expect("matched");
        assert_eq!(edit.count, 2);
        // `removed` must stay a verbatim prefix of the region: a *line*-boundary cut would
        // misalign it once matches straddle lines.
        assert_eq!(edit.delta.removed, "a\na\n");
        assert_eq!(edit.delta.inserted, "bb");
        assert!(b.contents().starts_with(&edit.delta.removed));
    }

    #[test]
    fn a_match_cannot_escape_the_resolved_range() {
        // The break after "b" is outside the range, so only the one after "a" can match.
        let b = Buffer::from_str("a\nb\nc\nd");
        let s = substitution(sub(VisualRange, r"\n", Some("-"), true, false));
        let edit = build_substitution(&b, 0, &re(&s.pattern), &s, Some((0, 1)), None)
            .unwrap()
            .expect("matched");
        assert_eq!(edit.count, 1);
        assert_eq!(edit.delta.removed, "a\nb");
        assert_eq!(edit.delta.inserted, "a-b");
    }

    #[test]
    fn replaced_ranges_are_absolute_post_apply_byte_ranges() {
        let b = Buffer::from_str("foo\nfoo\n");
        let s = substitution(sub(AllLines, "foo", Some("XY"), true, false));
        let edit = build_substitution(&b, 0, &re(&s.pattern), &s, None, None)
            .unwrap()
            .expect("matched");
        assert_eq!(edit.delta.inserted, "XY\nXY\n");
        // Offsets index the rewritten buffer; text before the region is unchanged.
        assert_eq!(edit.replaced_ranges, vec![0..2, 3..5]);
    }
}
