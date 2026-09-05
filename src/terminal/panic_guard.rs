//! Tells the process-wide panic hook that a panic is *expected* and will be caught, so it doesn't
//! tear the terminal down on the way past.
//!
//! `main`'s hook calls [`restore`](super::restore) and chains to the default hook — right for a
//! panic that ends the process, wrong for one inside `catch_unwind`.  The hook runs *before*
//! unwinding and can't tell the two apart, so a caught panic used to leave the app still running on
//! a terminal restored out from under it: no alt screen, no raw mode, and no way back short of
//! killing the process.  Seven `catch_unwind` sites had this; the likeliest to fire is
//! [`markdown::highlight`](crate::markdown::highlight)'s tokenizer, which runs synchronously on the
//! render thread over attacker-controlled code-block text through a backtracking regex engine.
//!
//! **Bind the guard in the same expression or block as its `catch_unwind`.**  Left live past the
//! catch it claims a panic is expected when nobody will catch it — the original defect with the
//! sign flipped, costing a silent death on a worker thread or an unwind out of `main` with the
//! alternate screen still up.
//!
//! The counter is thread-local because the hook runs on the panicking thread: a process-global flag
//! would let one thread's guarded section silence another's genuine crash, and the workers run
//! concurrently with the render thread as a matter of course.  It is a counter rather than a flag
//! because guarded sections nest — `image_dispatch`'s guarded decode calls
//! `diagram::render_mermaid_svg`, which guards a `catch_unwind` of its own.

use std::cell::Cell;

thread_local! {
    /// How many [`ExpectedPanic`] guards are live on this thread.
    static EXPECTED: Cell<usize> = const { Cell::new(0) };
}

/// Marks the current thread as being inside a `catch_unwind` while alive.  Create one immediately
/// before the `catch_unwind` that will do the catching:
///
/// ```ignore
/// let _guard = ExpectedPanic::new();
/// catch_unwind(AssertUnwindSafe(|| risky()))
/// ```
///
/// `Drop` runs during unwinding — after the hook, before `catch_unwind` returns — so the count is
/// correct again by the time control comes back, and a nested guard is restored, not cleared.
#[derive(Debug)]
pub struct ExpectedPanic(());

impl ExpectedPanic {
    #[allow(clippy::new_without_default)] // a `Default` guard would be silently inert
    pub fn new() -> Self {
        adjust(1);
        Self(())
    }
}

impl Drop for ExpectedPanic {
    fn drop(&mut self) {
        adjust(-1);
    }
}

/// `try_with`, never `with`: this runs during unwinding and thread teardown, where the
/// thread-local may already be destroyed and `with` would panic inside a panic, which aborts.
fn adjust(delta: isize) {
    let _ = EXPECTED.try_with(|c| {
        let next = if delta >= 0 {
            c.get().saturating_add(delta as usize)
        } else {
            c.get().saturating_sub(delta.unsigned_abs())
        };
        c.set(next);
    });
}

/// Is the panicking thread inside an [`ExpectedPanic`] guard?  `false` when the thread-local is
/// unavailable, so a panic during thread teardown still restores the terminal: erring that way
/// prints a stray stack trace, the other way leaves a live TUI on a dead terminal.
pub fn panic_is_expected() -> bool {
    EXPECTED.try_with(|c| c.get() > 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_marks_and_unmarks_the_thread() {
        assert!(!panic_is_expected());
        {
            let _g = ExpectedPanic::new();
            assert!(panic_is_expected());
        }
        assert!(!panic_is_expected());
    }

    #[test]
    fn guards_nest() {
        let outer = ExpectedPanic::new();
        {
            let _inner = ExpectedPanic::new();
            assert!(panic_is_expected());
        }
        assert!(panic_is_expected(), "the outer guard is still live");
        drop(outer);
        assert!(!panic_is_expected());
    }

    #[test]
    fn the_guard_survives_the_unwind_it_exists_for() {
        // After the catch the thread must be unmarked, or the *next* genuine panic on it would be
        // silently swallowed.
        let caught = {
            let _g = ExpectedPanic::new();
            std::panic::catch_unwind(|| {
                assert!(panic_is_expected(), "marked while inside");
                panic!("expected");
            })
        };
        assert!(caught.is_err());
        assert!(!panic_is_expected(), "unmarked once the guard is dropped");
    }

    #[test]
    fn a_hook_shaped_like_main_s_does_not_restore_for_a_guarded_panic() {
        // `restore()` can't run in a test (it would scribble escapes at the harness), so a flag
        // stands in for it; the *decision* is the half that was wrong.  The count is thread-local
        // because the hook is process-global and the suite runs in parallel — another thread's
        // `#[should_panic]` would otherwise be counted as ours.
        thread_local! {
            static RESTORED: Cell<usize> = const { Cell::new(0) };
        }
        fn restored() -> usize {
            RESTORED.with(Cell::get)
        }

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {
            if panic_is_expected() {
                return;
            }
            let _ = RESTORED.try_with(|c| c.set(c.get() + 1));
        }));

        let guarded = {
            let _g = ExpectedPanic::new();
            std::panic::catch_unwind(|| panic!("caught"))
        };
        assert!(guarded.is_err());
        assert_eq!(
            restored(),
            0,
            "a caught panic must not tear the terminal down"
        );

        // ...and an unguarded panic still does, which is the point of the hook.
        let bare = std::panic::catch_unwind(|| panic!("uncaught by intent"));
        assert!(bare.is_err());
        assert_eq!(
            restored(),
            1,
            "an unexpected panic must still restore the terminal"
        );

        std::panic::set_hook(previous);
    }

    #[test]
    fn the_flag_does_not_leak_to_another_thread() {
        let _g = ExpectedPanic::new();
        assert!(panic_is_expected());
        let seen = std::thread::spawn(panic_is_expected).join().unwrap();
        assert!(!seen, "another thread must not inherit the guard");
    }
}
