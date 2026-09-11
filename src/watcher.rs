//! Filesystem watcher. `NotifyWatcher` implements the `FileWatcher` trait over [`notify`];
//! events are debounced (200 ms) before the worker thread reads the file and pushes one
//! [`crate::app::AppEvent::Watcher`]. The worker is the single owner of disk reads for the
//! watched file (organic events, the debounce timer, and `force_reconcile` all funnel through
//! `do_read_and_send`), so the main thread never blocks on I/O.

pub mod debounce;
pub mod file_watcher;

pub use debounce::Debouncer;
pub use file_watcher::{FileWatcher, NotifyWatcher, WatchedChange, WatchedEvent};
