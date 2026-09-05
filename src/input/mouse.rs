//! Mouse event parsing and dispatch: raw `crossterm::event::MouseEvent` → [`MouseAction`].
//!
//! The dispatcher tracks click timing and drag state so it can surface double/triple-click and
//! click-drag semantics from the flat terminal event stream.  Coordinates are translated at
//! dispatch time: events outside the document area return `None`, ones inside are reported
//! relative to it.

use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

/// Maximum interval between consecutive clicks that still count as a "chord"
/// (double-click, triple-click).  400 ms matches the common X11 default.
pub const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// Default lines per wheel tick, absent a `config.editor.mouse_scroll_lines` override.
pub const DEFAULT_WHEEL_STEP: usize = 1;

/// High-level mouse action produced by the dispatcher.  Coordinates are relative to the
/// document area (the drawable region, excluding the status bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Single left click: place the cursor, clear the selection, become the drag anchor.
    /// `modifiers` distinguishes a plain click from a Ctrl-click (follow link).
    Click {
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    },
    /// Second click within [`MULTI_CLICK_WINDOW`] at the same cell — select the word.
    DoubleClick {
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    },
    /// Third click within [`MULTI_CLICK_WINDOW`] at the same cell — select the line.
    TripleClick {
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    },
    /// Left-button drag: extend the selection from the anchor to `(col, row)`.
    Drag { col: u16, row: u16 },
    /// Left-button release.  Informational — lets a caller tell "dragging" from "settled".
    Release,
    /// Wheel scroll; positive scrolls *down*, already multiplied by `wheel_step`.
    Scroll(i32),
}

/// Stateful mouse dispatcher.  Owns click-counting and drag state.
pub struct MouseDispatcher {
    last_click_time: Option<Instant>,
    last_click_cell: Option<(u16, u16)>,
    click_count: u32,
    left_down: bool,
    /// Whether the current gesture included a `Drag`.  A press that turned into one is not
    /// part of a double-click chord: without this, re-grabbing the same cell after an
    /// unsatisfying drag arrives as a `DoubleClick` and arms no drag at all.
    dragged_since_down: bool,
    /// Lines per wheel tick, seeded from `config.editor.mouse_scroll_lines`.
    wheel_step: usize,
}

impl Default for MouseDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseDispatcher {
    pub fn new() -> Self {
        Self::with_wheel_step(DEFAULT_WHEEL_STEP)
    }

    /// Construct a dispatcher with a caller-supplied wheel step.
    pub fn with_wheel_step(wheel_step: usize) -> Self {
        Self {
            last_click_time: None,
            last_click_cell: None,
            click_count: 0,
            left_down: false,
            dragged_since_down: false,
            wheel_step: wheel_step.max(1),
        }
    }

    /// Update the wheel step at runtime, so a settings-overlay change takes effect live.
    pub fn set_wheel_step(&mut self, wheel_step: usize) {
        self.wheel_step = wheel_step.max(1);
    }

    /// Current wheel step, for the settings live-update tests.
    #[cfg(test)]
    pub fn wheel_step(&self) -> usize {
        self.wheel_step
    }

    /// Translate a raw mouse event into a [`MouseAction`].  `doc_area` is the document area in
    /// terminal coordinates; events outside it return `None`, so a drag that leaves the area
    /// freezes the selection until it re-enters.
    pub fn dispatch(&mut self, event: MouseEvent, doc_area: Rect) -> Option<MouseAction> {
        let in_area = contains(doc_area, event.column, event.row);
        let (rel_col, rel_row) = if in_area {
            (event.column - doc_area.x, event.row - doc_area.y)
        } else {
            (0, 0)
        };

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if in_area => {
                self.left_down = true;
                let now = Instant::now();
                let same_cell = self.last_click_cell == Some((rel_col, rel_row));
                let within_threshold = self
                    .last_click_time
                    .map(|t| now.duration_since(t) <= MULTI_CLICK_WINDOW)
                    .unwrap_or(false);
                if same_cell && within_threshold && !self.dragged_since_down {
                    self.click_count = (self.click_count + 1).min(3);
                } else {
                    self.click_count = 1;
                }
                self.dragged_since_down = false;
                self.last_click_time = Some(now);
                self.last_click_cell = Some((rel_col, rel_row));
                Some(match self.click_count {
                    1 => MouseAction::Click {
                        col: rel_col,
                        row: rel_row,
                        modifiers: event.modifiers,
                    },
                    2 => MouseAction::DoubleClick {
                        col: rel_col,
                        row: rel_row,
                        modifiers: event.modifiers,
                    },
                    _ => MouseAction::TripleClick {
                        col: rel_col,
                        row: rel_row,
                        modifiers: event.modifiers,
                    },
                })
            }
            MouseEventKind::Drag(MouseButton::Left) if self.left_down => {
                // Set even for a drag outside the document area — the gesture was still a drag.
                self.dragged_since_down = true;
                in_area.then_some(MouseAction::Drag {
                    col: rel_col,
                    row: rel_row,
                })
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.left_down = false;
                Some(MouseAction::Release)
            }
            MouseEventKind::ScrollDown => Some(MouseAction::Scroll(self.wheel_step as i32)),
            MouseEventKind::ScrollUp => Some(MouseAction::Scroll(-(self.wheel_step as i32))),
            _ => None,
        }
    }
}

fn contains(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn up(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn drag(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn wheel_down() -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        }
    }

    #[test]
    fn single_click_is_single_click() {
        let mut d = MouseDispatcher::new();
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::Click {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn click_release_then_click_same_cell_is_double() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::DoubleClick {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn third_click_is_triple_click() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::TripleClick {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn click_at_different_cell_resets_counter() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        assert_eq!(
            d.dispatch(down(10, 4), area()),
            Some(MouseAction::Click {
                col: 10,
                row: 4,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn ctrl_modifier_is_threaded_into_click() {
        let mut d = MouseDispatcher::new();
        let ctrl_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 1,
            modifiers: KeyModifiers::CONTROL,
        };
        match d.dispatch(ctrl_click, area()) {
            Some(MouseAction::Click { modifiers, .. }) => {
                assert!(modifiers.contains(KeyModifiers::CONTROL));
            }
            other => panic!("expected Click, got {other:?}"),
        }
    }

    #[test]
    fn drag_without_prior_down_is_ignored() {
        let mut d = MouseDispatcher::new();
        assert_eq!(d.dispatch(drag(5, 5), area()), None);
    }

    #[test]
    fn drag_after_down_reports_drag() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        assert_eq!(
            d.dispatch(drag(7, 3), area()),
            Some(MouseAction::Drag { col: 7, row: 3 })
        );
    }

    /// A press that became a drag isn't the first half of a chord, so re-grabbing the same
    /// cell must report a fresh `Click` — otherwise the retry silently arms nothing.
    #[test]
    fn drag_breaks_the_double_click_chord() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(drag(9, 2), area());
        d.dispatch(up(9, 2), area());
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::Click {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    /// …and the break doesn't persist: the press after the retry chords normally again.
    #[test]
    fn chord_resumes_after_a_dragless_click() {
        let mut d = MouseDispatcher::new();
        d.dispatch(down(5, 2), area());
        d.dispatch(drag(9, 2), area());
        d.dispatch(up(9, 2), area());
        d.dispatch(down(5, 2), area());
        d.dispatch(up(5, 2), area());
        assert_eq!(
            d.dispatch(down(5, 2), area()),
            Some(MouseAction::DoubleClick {
                col: 5,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn out_of_area_events_are_dropped() {
        let mut d = MouseDispatcher::new();
        let outside = Rect {
            x: 10,
            y: 10,
            width: 10,
            height: 10,
        };
        assert_eq!(d.dispatch(down(5, 2), outside), None);
    }

    #[test]
    fn scroll_down_emits_positive_step() {
        let mut d = MouseDispatcher::new();
        assert_eq!(
            d.dispatch(wheel_down(), area()),
            Some(MouseAction::Scroll(DEFAULT_WHEEL_STEP as i32))
        );
    }

    #[test]
    fn scroll_down_honours_configured_wheel_step() {
        let mut d = MouseDispatcher::with_wheel_step(3);
        assert_eq!(
            d.dispatch(wheel_down(), area()),
            Some(MouseAction::Scroll(3))
        );
    }

    #[test]
    fn wheel_step_is_clamped_to_at_least_one() {
        let mut d = MouseDispatcher::with_wheel_step(0);
        assert_eq!(
            d.dispatch(wheel_down(), area()),
            Some(MouseAction::Scroll(1))
        );
    }
}
