//! Vim operator / resolution layer — the editor-side half of the two-layer split
//! (mirrors `mouse_ops`).
//!
//! `input::vim::vim_feed` decides *what* the user asked for; this module resolves offsets
//! against the buffer and mutates `EditorState`.

pub mod edits;
pub mod ex;
pub mod incsearch;
pub mod motion;
pub mod operator;
pub mod preview;
pub mod search;
pub mod table;
pub mod text_object;
pub mod vim_regex;
pub mod visual;

pub use edits::{
    indent_lines, indent_list_item, join_lines, open_list_continue, paste, renumber_list_at_cursor,
    replace_char, replace_char_range, replace_range_with, set_case_range, toggle_case,
    toggle_case_range,
};
pub use ex::{execute_substitute, parse_ex, ExCommand};
pub use incsearch::{end_incsearch, update_incsearch, IncsearchSession};
pub use motion::{
    doubled_line_range, first_non_blank, line_end_offset, resolve_find_repeat, resolve_motion,
    resolve_motion_range, vertical_line_range, FindKind, Motion, OpRange,
};
pub use operator::{execute_operator, OpResult, Operator};
pub use preview::{clear_substitute_preview, update_substitute_preview, SubstitutePreview};
pub use search::word_under_cursor_at;
// The input layer must call these *scoped* resolvers, never the bare `motion::resolve_*`
// pair, so a new operator target cannot forget the table cell clamp.  `rg 'resolve_motion'
// src/input/vim/feed.rs` should stay empty.
pub use table::{
    cell_scope, clear_table_cell, cursor_row_kind, delete_table_row, insert_table_rows,
    lines_touch_a_table, op_range_breaks_a_table, open_table_row, range_breaks_a_table,
    resolve_scoped_motion, resolve_scoped_op_range, scope_offset, table_paste_plan,
    visual_cell_step, visual_endpoint_in_cell, CellLimit, CellScope, TableBreak, TableOpOutcome,
    TablePaste,
};
pub use text_object::{resolve_text_object_range, TextObject};
pub use visual::{
    visual_charwise_range, visual_line_bounds, visual_line_char_range, visual_span, VisualKind,
};
