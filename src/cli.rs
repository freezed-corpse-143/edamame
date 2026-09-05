//! Command-line front end — everything edamame can do *without* starting the TUI.
//!
//! The tiny flag surface is hand-parsed rather than routed through `clap` to keep the dependency
//! graph narrow.  `main` dispatches over [`Invocation`]: print and exit, or hand a [`RunOpts`] to
//! the normal startup path.  Lives in the library crate so the parser's unit tests are reachable
//! from `cargo test --lib`.  See `docs/dev/cli.md`.

pub mod args;
pub mod doctor;
pub mod help;

pub use args::{split_startup_anchor, CliError, Invocation, RunOpts};
pub use doctor::run as run_doctor;
pub use help::{help_text, version_line, USAGE};
