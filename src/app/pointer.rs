//! OSC 22 pointer-shape feedback.

use crate::terminal::{set_pointer_shape, PointerShape};

use super::App;

impl App {
    /// Emit an OSC 22 pointer-shape escape, only when the shape actually changes.
    pub(super) fn update_pointer_shape(&mut self, shape: PointerShape) {
        if self.last_pointer_shape == shape {
            return;
        }
        // Writes straight to stdout, which libtest doesn't capture.
        if !cfg!(test) {
            set_pointer_shape(shape);
        }
        self.last_pointer_shape = shape;
    }
}
