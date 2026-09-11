//! Post-yank "flash": a brief highlight over just-yanked text, like neovim's
//! `vim.highlight.on_yank`.
//!
//! Lives on [`EditorState`] because the overlay painters read it like a search match and
//! the yank that arms it happens in `vim_ops::operator`; the App only feeds the expiry into
//! `next_deadline` and clears it when due.  Like [`crate::search::SearchState`] it is keyed
//! to `Buffer::version()`: any mutation invalidates the byte offsets, so
//! [`EditorState::active_yank_flash`] returns `None` rather than letting a painter slice a
//! shifted, mid-char span.

use std::time::{Duration, Instant};

use crate::editor::EditorState;

/// How long the yank highlight stays painted (neovim's default `on_yank` timeout).
pub const YANK_FLASH_DURATION: Duration = Duration::from_millis(150);

/// A pending yank-confirmation highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YankFlash {
    /// Yanked span in buffer bytes, end exclusive.
    pub start: usize,
    pub end: usize,
    pub armed_at: Instant,
    /// `Buffer::version()` the span was captured against; the offsets are only valid there.
    pub armed_version: u64,
}

impl EditorState {
    /// Arm the flash over the char range `[start_char, end_char)`; an empty span clears it.
    pub fn flash_yank(&mut self, start_char: usize, end_char: usize) {
        if start_char >= end_char {
            self.yank_flash = None;
            return;
        }
        let rope = self.buffer.rope();
        let len = rope.len_chars();
        let start = rope.char_to_byte(start_char.min(len));
        let end = rope.char_to_byte(end_char.min(len));
        if start >= end {
            self.yank_flash = None;
            return;
        }
        self.yank_flash = Some(YankFlash {
            start,
            end,
            armed_at: Instant::now(),
            armed_version: self.buffer.version(),
        });
    }

    /// The active flash, or `None` once it has faded or the buffer was mutated since arming.
    pub fn active_yank_flash(&self) -> Option<YankFlash> {
        let version = self.buffer.version();
        self.yank_flash
            .filter(|f| f.armed_version == version && f.armed_at.elapsed() < YANK_FLASH_DURATION)
    }

    /// Expiry instant for the run loop's `next_deadline`, so the fade-out redraws unprompted.
    pub fn yank_flash_deadline(&self) -> Option<Instant> {
        self.yank_flash.map(|f| f.armed_at + YANK_FLASH_DURATION)
    }

    /// Drop a flash that is no longer active; returns `true` when something was cleared.
    pub fn expire_yank_flash(&mut self) -> bool {
        if self.yank_flash.is_some() && self.active_yank_flash().is_none() {
            self.yank_flash = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::EditorState;

    fn editor(text: &str) -> EditorState {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        EditorState::new(Buffer::from_str(text), theme)
    }

    #[test]
    fn flash_yank_records_byte_range() {
        let mut ed = editor("héllo");
        ed.flash_yank(0, 3);
        let f = ed.yank_flash.expect("flash armed");
        assert_eq!((f.start, f.end), (0, 4)); // 'é' is two bytes
        assert!(ed.active_yank_flash().is_some());
    }

    #[test]
    fn empty_span_arms_nothing() {
        let mut ed = editor("hello");
        ed.flash_yank(2, 2);
        assert!(ed.yank_flash.is_none());
    }

    #[test]
    fn out_of_range_chars_are_clamped() {
        let mut ed = editor("hi");
        ed.flash_yank(0, 999);
        let f = ed.yank_flash.expect("flash armed");
        assert_eq!((f.start, f.end), (0, 2));
    }

    #[test]
    fn buffer_mutation_invalidates_the_flash() {
        let mut ed = editor("hello");
        ed.flash_yank(0, 3);
        assert!(ed.active_yank_flash().is_some());
        ed.buffer.insert(0, "x");
        assert!(ed.active_yank_flash().is_none());
        assert!(ed.expire_yank_flash());
        assert!(ed.yank_flash.is_none());
    }

    #[test]
    fn expired_flash_is_inactive_and_clears() {
        let mut ed = editor("hello");
        ed.flash_yank(0, 3);
        if let Some(f) = ed.yank_flash.as_mut() {
            f.armed_at = Instant::now() - YANK_FLASH_DURATION - Duration::from_millis(5);
        }
        assert!(ed.active_yank_flash().is_none());
        assert!(ed.yank_flash_deadline().is_some());
        assert!(ed.expire_yank_flash());
        assert!(ed.yank_flash.is_none());
        assert!(!ed.expire_yank_flash());
    }
}
