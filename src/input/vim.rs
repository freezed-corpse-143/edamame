//! Vim modal input: `vim_feed` is the keystroke reducer, `editor::vim_ops` the apply layer
//! (the same split as `MouseDispatcher` → `mouse_ops::apply`).

pub mod cmdline;
pub mod feed;
pub mod state;

pub use feed::{vim_feed, VimOutcome};
pub use state::{VimState, VimSubMode};
