//! Monochrome built-in theme: no color escapes, only text attribute modifiers.  Chosen
//! automatically on first launch when the terminal reports `Ansi16` or `NoColor`.
//!
//! Every palette slot is [`Color::Reset`], so even a site reading `theme.palette.*` directly emits
//! a color-free escape and the terminal's own default fg/bg shows through.  Attributes are kept
//! because SGR works at any color depth.

use ratatui::style::{Color, Modifier, Style};

use crate::config::theme::{Palette, Theme};

/// Colorless palette.  `light` is `false` only because the field is non-optional; the
/// appearance-mode filter classifies monochrome by its registry name.
pub fn palette() -> Palette {
    let r = Color::Reset;
    Palette {
        text: r,
        text_muted: r,
        bg: r,
        bg_muted: r,
        surface: r,
        surface_elevated: r,
        primary: r,
        secondary: r,
        accent: r,
        link: r,
        success: r,
        warning: r,
        error: r,
        code: r,
        diff_add: r,
        diff_delete: r,
        light: false,
    }
}

pub fn theme() -> Theme {
    Theme {
        palette: palette(),

        h1: Style::default().add_modifier(Modifier::BOLD),
        h1_rule: Style::default(),
        h2: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        h3: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        h4: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        h5: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        h6: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),

        bold: Style::default().add_modifier(Modifier::BOLD),
        italic: Style::default().add_modifier(Modifier::ITALIC),
        strikethrough: Style::default().add_modifier(Modifier::CROSSED_OUT),
        highlight: Style::default().add_modifier(Modifier::REVERSED),
        code_span: Style::default().add_modifier(Modifier::REVERSED),
        code_span_dim: Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM),
        link_text: Style::default().add_modifier(Modifier::UNDERLINED),
        link_file: Style::default().add_modifier(Modifier::UNDERLINED),
        link_heading: Style::default().add_modifier(Modifier::UNDERLINED),
        image_placeholder: Style::default().add_modifier(Modifier::ITALIC),
        footnote: Style::default(),

        code_block_border: Style::default(),
        code_block_lang: Style::default().add_modifier(Modifier::ITALIC),
        code_block_text: Style::default(),

        // With no color to spend, only the two classes that carry meaning without one are marked:
        // a comment because it is *not* code, a keyword because it leads its line.  Marking more
        // would turn most of a line into attributes and distinguish nothing.
        syntax_keyword: Style::default().add_modifier(Modifier::BOLD),
        syntax_string: Style::default(),
        syntax_comment: Style::default().add_modifier(Modifier::DIM),
        syntax_number: Style::default(),
        syntax_type: Style::default(),
        syntax_function: Style::default(),
        syntax_attribute: Style::default(),
        blockquote_bar: Style::default(),
        // DIM, not ITALIC: with no wash available, a blanket italic left `*emphasis*` inside a
        // quote with nothing to say (issue #33).  Dimming marks the region and leaves bold /
        // italic / reversed free to read on top.
        blockquote_text: Style::default().add_modifier(Modifier::DIM),
        rule: Style::default(),
        frontmatter_delimiter: Style::default(),
        frontmatter_key: Style::default(),
        frontmatter_value: Style::default(),

        list_bullet: Style::default(),
        list_number: Style::default(),

        task_unchecked: Style::default(),
        task_checked: Style::default().add_modifier(Modifier::BOLD),
        task_complete_text: Style::default().add_modifier(Modifier::CROSSED_OUT),
        task_strikethrough: true,

        table_border: Style::default(),
        table_header: Style::default().add_modifier(Modifier::BOLD),
        table_header_border: Style::default(),
        table_cell: Style::default(),
        table_row_even: Style::default(),
        table_row_odd: Style::default().add_modifier(Modifier::DIM),
        table_drop_indicator: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        table_drop_target: Style::default().add_modifier(Modifier::REVERSED),
        table_handle: Style::default(),
        table_handle_delete: Style::default().add_modifier(Modifier::BOLD),

        status_bar: Style::default().add_modifier(Modifier::REVERSED),
        status_mode_preview: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_mode_rendered: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_mode_raw: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_mode_vim_normal: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_mode_vim_insert: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_mode_vim_visual: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_filename: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        status_info: Style::default().add_modifier(Modifier::REVERSED),
        status_modified: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_breadcrumb_sep: Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM),
        status_breadcrumb_ancestor: Style::default()
            .add_modifier(Modifier::REVERSED | Modifier::DIM),
        status_breadcrumb_current: Style::default()
            .add_modifier(Modifier::REVERSED | Modifier::BOLD),

        hint_bar: Style::default(),
        hint_chord: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        hint_label: Style::default(),

        transient_info: Style::default().add_modifier(Modifier::REVERSED),
        transient_success: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        transient_warning: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        transient_error: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

        modal_bg: Style::default().add_modifier(Modifier::REVERSED),
        modal_title_normal: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        // No color for urgency; DIM keeps the title distinct on dim-aware terminals.
        modal_title_warning: Style::default()
            .add_modifier(Modifier::BOLD | Modifier::REVERSED | Modifier::DIM),
        modal_title_error: Style::default()
            .add_modifier(Modifier::BOLD | Modifier::REVERSED | Modifier::DIM),
        modal_close_hint: Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM),
        modal_item: Style::default().add_modifier(Modifier::REVERSED),
        modal_item_hint: Style::default().add_modifier(Modifier::REVERSED),
        modal_item_selected: Style::default().add_modifier(Modifier::BOLD),
        // DIM reads as "marked but quiet", distinct from BOLD (focused) and plain (unselected)
        // without REVERSED, which is already the unselected `modal_item` state here.
        modal_item_selected_unfocused: Style::default().add_modifier(Modifier::DIM),
        modal_item_selected_hint: Style::default().add_modifier(Modifier::BOLD),
        modal_description: Style::default().add_modifier(Modifier::REVERSED),
        modal_section_heading: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        // Filled when focused, plain BOLD when not — the colored themes' filled-vs-outlined
        // pattern.
        modal_input_unfocused: Style::default().add_modifier(Modifier::BOLD),
        modal_input_focused: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        modal_button_focused: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

        normal: Style::default(),
        selection: Style::default().add_modifier(Modifier::REVERSED),
        // Dim rather than invert, so plain / muted / focused stay three distinct tiers.
        selection_muted: Style::default().add_modifier(Modifier::DIM),
        status_mode_search: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        active_line: Style::default(),
        cursor: Style::default().add_modifier(Modifier::REVERSED),

        line_number: Style::default().add_modifier(Modifier::DIM),

        // Glyphs alone separate track from thumb; the active state inverts.
        scrollbar_track: Style::default(),
        scrollbar_thumb: Style::default(),
        scrollbar_thumb_active: Style::default().add_modifier(Modifier::REVERSED),

        // No saturated line bg available, so a changed line REVERSES and its inline highlights
        // add BOLD on top.
        diff_add_line: Style::default().add_modifier(Modifier::REVERSED),
        diff_delete_line: Style::default().add_modifier(Modifier::REVERSED),
        // Context plain / unfocused dim / focused reversed: three tiers without color.
        diff_add_line_unfocused: Style::default().add_modifier(Modifier::DIM),
        diff_delete_line_unfocused: Style::default().add_modifier(Modifier::DIM),
        diff_add_inline: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        diff_delete_inline: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        // No REVERSED here, so an unfocused hunk's inline highlights don't pop off its dim line.
        diff_add_inline_unfocused: Style::default().add_modifier(Modifier::DIM),
        diff_delete_inline_unfocused: Style::default().add_modifier(Modifier::DIM),
        // The label ("Accepted" / "Rejected") plus bold/dim carries the decision state.
        diff_decision_pending: Style::default().add_modifier(Modifier::DIM),
        diff_decision_accepted: Style::default().add_modifier(Modifier::BOLD),
        diff_decision_rejected: Style::default().add_modifier(Modifier::BOLD),
        // Recedes to DIM like an unfocused line; the label still spells the decision.
        diff_decision_unfocused: Style::default().add_modifier(Modifier::DIM),
        status_mode_diff: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_bar_diff: Style::default().add_modifier(Modifier::REVERSED),
        hint_bar_diff: Style::default().add_modifier(Modifier::REVERSED),
    }
}
