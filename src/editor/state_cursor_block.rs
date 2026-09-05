//! Cursor-block tracking and the jitter-suppression reveal timer.

use std::time::Instant;

use crate::editor::state::ImageReveal;
use crate::editor::{EditorState, Mode, RAW_REVEAL_DELAY};

impl EditorState {
    /// Call after any cursor movement in Rendered mode: refreshes the cursor block, its
    /// buffer line range, and the raw-reveal timer.  The timer resets on every change of
    /// **buffer line** (not block), so a fifty-line table reveals row by row with the same
    /// delay as a one-line paragraph.
    pub fn update_cursor_block(&mut self) {
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        let previous_block_idx = self.cursor_block_idx;
        self.cursor_block_idx = self.parsed.source_map.block_for_byte(cursor_byte);

        // Lets rendered_view extract the raw block source during a typing burst without
        // consulting the stale source_map; in-line edits never move line indices.
        self.cursor_block_line_range = self.cursor_block_idx.and_then(|idx| {
            let byte_range = self.parsed.source_map.original_range_for_block(idx)?;
            let rope = self.buffer.rope();
            let total_bytes = rope.len_bytes();
            let start_byte = byte_range.start.min(total_bytes);
            let end_byte = byte_range.end.min(total_bytes);
            let start_char = rope.byte_to_char(start_byte);
            // `end_byte - 1` so a range ending on `\n` doesn't claim the next line.
            let end_char = rope.byte_to_char(end_byte.saturating_sub(1).max(start_byte));
            let start_line = rope.char_to_line(start_char);
            let end_line = rope.char_to_line(end_char).max(start_line);
            Some(start_line..end_line + 1)
        });

        // A mermaid block reveals as one unit, so re-arming the timer on intra-block line
        // moves would flash the image placeholder back in between them.
        let (current_line, _) = self.cursor.line_col(&self.buffer);
        if Some(current_line) != self.cursor_line_idx {
            let staying_in_mermaid = previous_block_idx == self.cursor_block_idx
                && self
                    .cursor_block_idx
                    .is_some_and(|idx| self.parsed.is_mermaid_block(idx));
            self.cursor_line_idx = Some(current_line);
            if !staying_in_mermaid {
                self.cursor_block_entered_at = Some(Instant::now());
            }
        }
        self.cursor_blink.reset();
    }

    /// Whether the cursor should be painted this frame.
    pub fn cursor_visible(&self) -> bool {
        self.terminal_focused
            && self.mode != Mode::Preview
            && (self.modal_open || self.cursor_blink.is_visible())
    }

    /// Whether the cursor block should show raw source.  False during the `RAW_REVEAL_DELAY`
    /// window, during a mouse drag (the click anchor must not shift), and while a search or
    /// `:s` preview is active (blocks must not flip to raw under the highlights).
    pub fn cursor_block_revealed(&self) -> bool {
        if self.drag_in_progress {
            return false;
        }
        if self.search.is_some() {
            return false;
        }
        if self.substitute_preview.is_some() {
            return false;
        }
        match self.cursor_block_entered_at {
            None => true,
            Some(t) => t.elapsed() >= RAW_REVEAL_DELAY,
        }
    }

    /// Bring [`EditorState::image_reveal`] in line with the cursor, re-parsing when it
    /// changed; returns `true` when it did.  Called every event-loop pass from
    /// `App::prepare_viewport` because the reveal is time-driven and has no action site of
    /// its own, so the no-op path must not allocate.
    pub fn sync_image_reveal(&mut self) -> bool {
        let target = self.image_reveal_target();
        let unchanged = match (target, self.image_reveal.as_ref()) {
            (Some((ordinal, url, rows)), Some(cur)) => {
                ordinal == cur.ordinal && url == cur.url.as_str() && rows == cur.rows
            }
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return false;
        }
        self.image_reveal = target.map(|(ordinal, url, rows)| ImageReveal {
            ordinal,
            url: url.to_owned(),
            rows,
        });
        // Source is untouched, so every byte range survives the re-parse.
        self.refresh_parsed();
        true
    }

    /// The reservation the reveal wants for the cursor position: `(ordinal into
    /// `ParsedDoc::image_blocks`, URL, one row per raw source line)`, or `None` outside a
    /// revealed image block.  See [`ImageReveal`] for why the URL alone can't name a block.
    /// The URL is borrowed, not cloned, because this runs every event-loop pass.
    fn image_reveal_target(&self) -> Option<(usize, &str, usize)> {
        if self.mode != Mode::Rendered {
            return None;
        }
        // Mid-typing the parse (and a diagram's source-hashed URL) is stale; an in-line edit
        // can't change the line count, so hold the current reservation.
        if self.parsed_dirty {
            return self
                .image_reveal
                .as_ref()
                .map(|r| (r.ordinal, r.url.as_str(), r.rows));
        }
        if !self.cursor_block_revealed() {
            return None;
        }
        let cursor_byte = self.buffer.rope().char_to_byte(self.cursor.offset);
        let block_idx = self.parsed.source_map.block_for_byte(cursor_byte)?;
        if !self.parsed.is_image_block(block_idx) {
            return None;
        }
        let ordinal = self
            .parsed
            .image_blocks
            .iter()
            .position(|info| info.block_idx == block_idx)?;
        let url = self.parsed.image_blocks[ordinal].url.as_str();
        let range = self.parsed.source_map.original_range_for_block(block_idx)?;
        // Parse-time range, so read the parse-time source (also avoids `Buffer::contents()`
        // allocating the whole document every frame).
        let contents = self.parsed.source();
        let source = contents.get(range.start..range.end.min(contents.len()))?;
        // Same split the painter uses, so reserved rows and painted lines can't disagree.
        let rows = crate::ui::rendered_view::revealed_source_line_count(source);
        Some((ordinal, url, rows))
    }
}
