//! Operator application — `d` / `c` / `y` against a resolved [`OpRange`].
//!
//! `vim_feed` (input layer) resolves the operator and range; this layer mutates the buffer and
//! returns the register payload rather than touching `VimState`, since the input layer owns the
//! register and the editor layer must not depend upward.
//!
//! **Single delta is load-bearing.** An operator issues exactly one `apply_delta` (never N
//! char-deletes), so `3dw` is one `u`.

use crate::document::EditDelta;
use crate::editor::vim_ops::motion::{first_non_blank, line_end_offset, OpRange};
use crate::editor::EditorState;

/// Editor-layer mirror of the input layer's `PendingOp`, kept here so `vim_ops` needs no
/// `use crate::input::…`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
}

/// What an operator produced, for the caller to fold back into `VimState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpResult {
    /// Text for the unnamed register.  Empty when the operator covered no characters (the
    /// caller then leaves the register alone).
    pub register_text: String,
    /// Linewise register flag — drives `p`/`P` open-a-new-line behavior.
    pub linewise: bool,
    /// `true` for `c` (change): the caller switches to Insert sub-mode.
    pub enter_insert: bool,
}

/// Apply `op` over `range`, returning the register payload.  Linewise spans synthesize a
/// trailing newline for the register and, on delete, consume a neighboring newline so no blank
/// line is left behind.
pub fn execute_operator(editor: &mut EditorState, op: Operator, range: OpRange) -> OpResult {
    match range {
        OpRange::Chars(r) => exec_charwise(editor, op, r.start, r.end),
        OpRange::Lines { first, last } => exec_linewise(editor, op, first, last),
    }
}

// ── Charwise ──────────────────────────────────────────────────────────────────

fn exec_charwise(editor: &mut EditorState, op: Operator, start: usize, end: usize) -> OpResult {
    let len = editor.buffer.len_chars();
    let start = start.min(len);
    let end = end.min(len);
    let text = if start < end {
        editor.buffer.slice_to_string(start, end)
    } else {
        String::new()
    };
    match op {
        Operator::Yank => {
            editor.flash_yank(start, end);
            editor.cursor.offset = start;
            editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
            editor.update_cursor_block();
        }
        Operator::Delete | Operator::Change => {
            if start < end {
                // `apply_delta` parks the cursor at `start`, the correct post-edit position.
                editor.apply_delta(EditDelta {
                    offset: start,
                    removed: text.clone(),
                    inserted: String::new(),
                });
            } else {
                editor.cursor.offset = start;
            }
        }
    }
    OpResult {
        register_text: text,
        linewise: false,
        enter_insert: op == Operator::Change,
    }
}

// ── Linewise ──────────────────────────────────────────────────────────────────

fn exec_linewise(editor: &mut EditorState, op: Operator, first: usize, last: usize) -> OpResult {
    let buf = &editor.buffer;
    let line_count = buf.line_count();
    if line_count == 0 {
        return OpResult {
            register_text: String::new(),
            linewise: true,
            enter_insert: op == Operator::Change,
        };
    }
    let first = first.min(line_count - 1);
    let last = last.min(line_count - 1).max(first);

    let content_start = buf.line_to_char(first);
    let len = buf.len_chars();
    // End of the line block including its trailing newline (or EOF for the last line).
    let block_end = if last + 1 < line_count {
        buf.line_to_char(last + 1)
    } else {
        len
    };

    // Always newline-terminated, so the linewise flag and the text stay consistent for `p`.
    let mut register_text = buf.slice_to_string(content_start, block_end);
    if !register_text.ends_with('\n') {
        register_text.push('\n');
    }

    match op {
        Operator::Yank => {
            editor.flash_yank(content_start, block_end);
            editor.cursor.offset = content_start;
            editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
            editor.update_cursor_block();
        }
        Operator::Delete => {
            // Deleting the final line(s) has no trailing newline to remove, so consume the
            // *preceding* one instead — otherwise a stray empty line remains.
            let (del_start, del_end) = if block_end >= len && content_start > 0 {
                (content_start - 1, len)
            } else {
                (content_start, block_end)
            };
            let removed = editor.buffer.slice_to_string(del_start, del_end);
            editor.apply_delta(EditDelta {
                offset: del_start,
                removed,
                inserted: String::new(),
            });
            // Vim's `dd` landing rule: first non-blank of the line now at the deletion point.
            let landing = del_start.min(editor.buffer.len_chars());
            let line = editor.buffer.char_to_line(landing);
            editor.cursor.offset = first_non_blank(&editor.buffer, line);
            editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
            editor.update_cursor_block();
        }
        Operator::Change => {
            // `cc` keeps one empty line: remove only to the last line's content end so the
            // terminating newline survives.
            let content_end = line_end_offset(&editor.buffer, last);
            if content_start < content_end {
                let removed = editor.buffer.slice_to_string(content_start, content_end);
                editor.apply_delta(EditDelta {
                    offset: content_start,
                    removed,
                    inserted: String::new(),
                });
            }
            editor.cursor.offset = content_start.min(editor.buffer.len_chars());
            editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
            editor.update_cursor_block();
        }
    }

    OpResult {
        register_text,
        linewise: true,
        enter_insert: op == Operator::Change,
    }
}
