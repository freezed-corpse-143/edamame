use crate::document::EditDelta;
use crate::editor::list_edit;
use crate::editor::{EditorState, Mode};

use super::coord::click_to_char_offset;

/// If `(col, row)` falls on a task-list checkbox, toggle it and return `true`; `false` lets the
/// caller fall through to cursor placement.
pub(super) fn toggle_checkbox_at(
    state: &mut EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> bool {
    if state.mode == Mode::Raw {
        return false;
    }
    let Some(offset) = click_to_char_offset(state, col, row, viewport_width) else {
        return false;
    };
    let source = state.buffer.contents();
    let click_byte = state.buffer.rope().char_to_byte(offset);
    let Some(info) = list_edit::find_list_at(&source, click_byte) else {
        return false;
    };
    let Some(item_idx) = list_edit::cursor_item_idx(&info, click_byte) else {
        return false;
    };
    let item = &info.items[item_idx];
    let Some(task_box) = item.task_box else {
        return false;
    };

    // Hitbox is the whole `• [ ]` prefix; the trailing space after `]` falls through so the
    // caret can be placed right before the task text.
    let hit_start = item.start;
    let hit_end = task_box + 3;
    if click_byte >= hit_start && click_byte < hit_end {
        if let Some(res) = list_edit::toggle_checkbox(&info, &source, click_byte) {
            // A 1-for-1 char replacement, so offsets stay valid.  Cursor tracking state is
            // preserved across the edit: `update_cursor_block` would reset the reveal timer
            // and flash the block rendered → raw.
            let offset_char = state.buffer.rope().byte_to_char(res.delta.offset);
            let delta = EditDelta {
                offset: offset_char,
                removed: res.delta.removed,
                inserted: res.delta.inserted,
            };
            let saved_offset = state.cursor.offset;
            let saved_preferred = state.cursor.preferred_col;
            let saved_block_idx = state.cursor_block_idx;
            let saved_line_idx = state.cursor_line_idx;
            let saved_entered_at = state.cursor_block_entered_at;
            state.apply_delta(delta);
            state.cursor.offset = saved_offset.min(state.buffer.len_chars());
            state.cursor.preferred_col = saved_preferred;
            state.cursor_block_idx = saved_block_idx;
            state.cursor_line_idx = saved_line_idx;
            state.cursor_block_entered_at = saved_entered_at;
            return true;
        }
    }
    false
}
