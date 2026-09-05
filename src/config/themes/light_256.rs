use ratatui::style::{Color, Modifier, Style};

use crate::config::theme::{Palette, Theme};

/// Companion light palette to [`super::dark_256`]: the same color families re-tuned for a
/// near-white page, darker and more saturated.  Inverse-text sites (`fg = bg`) need every brand
/// color dark enough to contrast with near-white, which is why the yellows shift to amber.
pub fn palette() -> Palette {
    Palette {
        text: Color::Indexed(234),
        text_muted: Color::Indexed(244),
        bg: Color::Indexed(254),
        bg_muted: Color::Indexed(252),
        surface: Color::Indexed(251),
        surface_elevated: Color::Indexed(250),

        primary: Color::Indexed(166),
        secondary: Color::Indexed(97),
        // Pale enough to carry near-black `text` (234) as a selection bg, dark enough to read as a
        // foreground on the near-white page.
        accent: Color::Indexed(75),
        link: Color::Indexed(21),

        success: Color::Indexed(28),
        // Darker than the dark palette's yellow so inverse-text sites stay legible here.
        warning: Color::Indexed(172),
        error: Color::Indexed(124),

        // Distinct enough from `secondary` that inline code reads as code, not chrome.
        code: Color::Indexed(91),

        // Distinct from `success` / `error` so no two palette slots share a value.
        diff_add: Color::Indexed(22),
        diff_delete: Color::Indexed(88),

        light: true,
    }
}

/// [`Theme::from_palette`] plus a curated heading ramp; see `dark_256::theme` for why.
pub fn theme() -> Theme {
    let mut t = Theme::from_palette(&palette());
    let bold = Modifier::BOLD;
    let underline = Modifier::UNDERLINED;
    // As in `dark_256`, but each shade pushed darker so it reads on a light page.
    let h1 = Color::Indexed(166); // primary, bright
    let h2 = Color::Indexed(53); // secondary, bright (dark purple)
    let h3 = Color::Indexed(130); // primary, medium
    let h4 = Color::Indexed(97); // secondary, medium
    let h5 = Color::Indexed(94); // primary, dull
    let h6 = Color::Indexed(60); // secondary, dull
    t.h1 = Style::default().fg(h1).add_modifier(bold);
    t.h1_rule = Style::default().fg(h1);
    t.h2 = Style::default().fg(h2).add_modifier(bold | underline);
    t.h3 = Style::default().fg(h3).add_modifier(bold | underline);
    t.h4 = Style::default().fg(h4).add_modifier(bold | underline);
    t.h5 = Style::default().fg(h5).add_modifier(bold | underline);
    t.h6 = Style::default().fg(h6).add_modifier(bold | underline);

    // Between `bg` (254) and the striped-row bg (252) so a code span inside a stripe still reads
    // as code.  Hand-picked because cube-stepping `code` (91) would shift hue.
    let code_bg = Color::Indexed(253);
    t.code_span = Style::default().fg(palette().code).bg(code_bg);
    t.code_span_dim = Style::default()
        .fg(palette().code)
        .bg(code_bg)
        .add_modifier(Modifier::DIM);
    t.code_block_border = Style::default().fg(palette().text).bg(code_bg);
    t.code_block_text = Style::default().fg(palette().text).bg(code_bg);

    // Hand-picked against `code_bg` (253) for `dark_256`'s reason; every entry clears 4.5:1 there.
    // The cube has no dark orange that does, so `keyword` takes the deep red light editor themes
    // conventionally give it and `attribute` moves to teal rather than crowding the reds — the hue
    // families differ from the RGB derivation's, the meanings do not.
    t.syntax_keyword = Style::default()
        .fg(Color::Indexed(124)) // deep red
        .add_modifier(Modifier::BOLD);
    t.syntax_string = Style::default().fg(Color::Indexed(22)); // dark green
    t.syntax_comment = Style::default()
        .fg(Color::Indexed(59)) // the most recessive grey still legible here
        .add_modifier(Modifier::ITALIC);
    t.syntax_number = Style::default().fg(Color::Indexed(58)); // dark olive
    t.syntax_type = Style::default().fg(Color::Indexed(91)); // purple, the `code` slot
    t.syntax_function = Style::default().fg(Color::Indexed(21)); // blue, the `link` slot
    t.syntax_attribute = Style::default().fg(Color::Indexed(23)); // teal

    // Hand-picked for the same reason.  With no room between `bg` (254) and `code_bg` (253), the
    // quote wash sits one step *past* the code surface rather than short of it: a code span inside
    // a quote still separates, in the other direction.  252 is also the striped-row bg, which a
    // quote can never sit inside.
    t.blockquote_text = Style::default().bg(Color::Indexed(252));

    // As in `dark_256`: the derived blend would leave the bare `surface` grey (251) on a 254 page.
    // A washed `accent` still reads as a highlight while the focused match's 75 stays stronger.
    let selection_muted_bg = Color::Indexed(153);
    t.selection_muted = Style::default().bg(selection_muted_bg).fg(palette().text);

    // Diff washes; see `dark_256` for why the `blend` no-op matters most here.  Unlike the dark
    // theme, this palette affords the full hierarchy the derived styles intend — every pale tint
    // below clears 7:1 against `text` (234), so all four levels go to focus and inline depth rather
    // than legibility: pale wash → stronger wash (focused) → muted patch → saturated patch.  The
    // non-focused inline shades are the greyer members of each ramp, deeper than their wash without
    // competing with the focused hunk's brighter fills.
    t.diff_add_line_unfocused = Style::default().bg(Color::Indexed(194)); // #d7ffd7
    t.diff_add_line = Style::default().bg(Color::Indexed(157)); // #afffaf
    t.diff_delete_line_unfocused = Style::default().bg(Color::Indexed(224)); // #ffd7d7
    t.diff_delete_line = Style::default().bg(Color::Indexed(217)); // #ffafaf

    // No foreground is pinned: `text` clears 7:1 on all four shades, so unlike the dark theme's
    // bright green there is nothing to rescue, and leaving fg unset lets the markdown's own colors
    // show through.
    t.diff_add_inline_unfocused = Style::default().bg(Color::Indexed(151)); // #afd7af
    t.diff_delete_inline_unfocused = Style::default().bg(Color::Indexed(181)); // #d7afaf
    t.diff_add_inline = Style::default()
        .bg(Color::Indexed(114)) // #87d787
        .add_modifier(Modifier::BOLD);
    t.diff_delete_inline = Style::default()
        .bg(Color::Indexed(210)) // #ff8787
        .add_modifier(Modifier::BOLD);

    // Green on the status line, red on the hint line, mirroring the adds-below / deletes-above
    // stacking, in the same wash shades as the document rows.
    t.status_bar_diff = Style::default().bg(Color::Indexed(194)).fg(palette().text);
    t.hint_bar_diff = Style::default().bg(Color::Indexed(224)).fg(palette().text);
    t
}
