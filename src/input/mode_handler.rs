use crossterm::event::KeyEvent;

use crate::config::Action;
use crate::editor::EditorState;

pub mod default;
pub mod diff_keys;

/// A keybinding handler for an input mode; may inspect `EditorState` to return
/// context-sensitive `Action`s. The default implementation lives in `default.rs`.

pub trait ModeHandler {
    fn handle(&mut self, event: KeyEvent, state: &EditorState) -> Option<Action>;
}
