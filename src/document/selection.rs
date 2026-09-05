use crate::document::Buffer;

/// A text selection as two char offsets; the selected range is `min..max` of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection started (fixed during Shift+move).
    pub anchor: usize,
    /// The moving end (typically the cursor).
    pub active: usize,
}

/// The rendered-screen region of one table cell: the rendered-line range of its logical row plus
/// the char-column band of the cell's content area. Stored on a [`VisualSelection`] that began
/// inside the cell so painting, copy, and drag extension stay confined to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellBand {
    /// Inclusive rendered-line range of every wrapped sub-line of the logical table row.
    pub lines: (usize, usize),
    /// Half-open rendered char-column range of the cell's content area.
    pub cols: (usize, usize),
}

/// A selection over the rendered view in Preview mode, stored as `(rendered_line, char_col)`
/// tuples so copy yields exactly the rendered text rather than the Markdown source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualSelection {
    pub anchor: (usize, usize),
    /// The moving end (mouse pointer).
    pub active: (usize, usize),
    /// `Some` when the selection began inside a table cell; see [`CellBand`].
    pub band: Option<CellBand>,
}

impl VisualSelection {
    /// A plain (unbanded) selection from `anchor` to `active`.
    pub fn span(anchor: (usize, usize), active: (usize, usize)) -> Self {
        Self {
            anchor,
            active,
            band: None,
        }
    }

    /// Normalized `(start, end)` with `start <= end` in row-major order.
    pub fn range(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.active {
            (self.anchor, self.active)
        } else {
            (self.active, self.anchor)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.active
    }
}

impl Selection {
    /// Zero-width selection at `anchor`; used by tests.
    #[allow(dead_code)]
    pub fn new(anchor: usize) -> Self {
        Self {
            anchor,
            active: anchor,
        }
    }

    /// The half-open char range `[start, end)` of the selected text.
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.active), self.anchor.max(self.active))
    }

    pub fn selected_text(&self, buf: &Buffer) -> String {
        let (start, end) = self.range();
        let end = end.min(buf.len_chars());
        if start >= end {
            return String::new();
        }
        buf.slice_to_string(start, end)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Buffer;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    #[test]
    fn new_selection_is_empty() {
        let s = Selection::new(5);
        assert!(s.is_empty());
        assert_eq!(s.range(), (5, 5));
    }

    #[test]
    fn range_forward() {
        let s = Selection {
            anchor: 2,
            active: 7,
        };
        assert_eq!(s.range(), (2, 7));
    }

    #[test]
    fn range_backward() {
        let s = Selection {
            anchor: 7,
            active: 2,
        };
        assert_eq!(s.range(), (2, 7));
    }

    #[test]
    fn selected_text_forward() {
        let b = buf("hello world");
        let s = Selection {
            anchor: 6,
            active: 11,
        };
        assert_eq!(s.selected_text(&b), "world");
    }

    #[test]
    fn selected_text_backward() {
        let b = buf("hello world");
        let s = Selection {
            anchor: 11,
            active: 6,
        };
        assert_eq!(s.selected_text(&b), "world");
    }

    #[test]
    fn selected_text_empty() {
        let b = buf("hello");
        let s = Selection::new(3);
        assert_eq!(s.selected_text(&b), "");
    }
}
