//! Helpers for the `--diff` difftool presentation: reading the two sides, deciding whether to
//! render them as Markdown, and naming the session in the status bar.
//!
//! In the library rather than `main.rs` so they are reachable from tests.

use std::path::Path;

use anyhow::{Context, Result};

use super::nav::is_markdown_path;

/// 128 + `SIGINT`. Only reached on a platform without process groups; on Unix [`stop_walk`]
/// dies from the signal itself.
const EXIT_INTERRUPTED: i32 = 130;

/// Read one side of a `--diff` pair. git's `/dev/null` for a missing side reads as empty; a
/// non-UTF-8 (binary) side fails with the path named, since both sides are usually temp files.
pub fn read_side(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {} for review", path.display()))?;
    // A CRLF side must not diff as wholly changed against an LF side.
    Ok(crate::document::buffer::normalize_newlines(raw))
}

/// Whether a `--diff` pair is Markdown at all. Either side is enough, because git passes
/// `/dev/null` for the missing half of an add or delete. `main` declines a non-Markdown pair by
/// name before reading either side, which is what lets the `git difftool` recipe run without a
/// pathspec.
pub fn is_markdown_pair(old: &Path, new: &Path) -> bool {
    is_markdown_path(old) || is_markdown_path(new)
}

/// Status-bar label for a difftool session: the new side's basename (git difftool preserves it
/// under its temp dir), the old side's for a delete, both for a rename. `null` is dropped as a
/// name so `/dev/null` never labels a side; a real file named `null` is the accepted collision.
pub fn diff_label(old: &Path, new: &Path) -> String {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| n != "null")
    };
    match (name(old), name(new)) {
        (Some(o), Some(n)) if o != n => format!("{o} → {n}"),
        (_, Some(n)) => n,
        (Some(o), None) => o,
        (None, None) => "[diff]".to_owned(),
    }
}

/// True when git is driving us over a list of paths (as opposed to a hand-typed
/// `edamame --diff a b`). Gates [`stop_walk`].
///
/// Despite the name this cannot be narrowed to `git difftool`: `GIT_DIFF_PATH_TOTAL` is set by
/// git's external-diff machinery, so a gitattributes `diff.<driver>.command` under plain
/// `git diff` sets it too, while `GIT_DIFFTOOL_EXTCMD` is set only by `difftool -x` (not `-t`).
/// Measured on git 2.50.1; either variable is enough, and the breadth is correct because all
/// three invocations share the fact the gate needs.
pub fn under_git_difftool() -> bool {
    is_difftool_env(
        std::env::var_os("GIT_DIFFTOOL_EXTCMD").is_some(),
        std::env::var_os("GIT_DIFF_PATH_TOTAL").is_some(),
    )
}

/// Split out of [`under_git_difftool`] so it is testable without touching the environment.
fn is_difftool_env(extcmd: bool, path_total: bool) -> bool {
    extcmd || path_total
}

/// End a `git difftool` walk the way `Ctrl-C` would have; never returns.
///
/// A signal rather than an exit code because `git difftool--helper` discards the tool's status
/// (bare `exit 0` unless `GIT_DIFFTOOL_TRUST_EXIT_CODE`). Raw mode clears `ISIG`, so the terminal
/// cannot raise `SIGINT` itself; sending it to our process group kills git alongside us, which
/// cleans its temp dir and prints nothing (an exit-code stop prints `fatal: external diff died`).
/// Only the ordinary `Quit` binding reaches here; `Ctrl-C` stays `Action::Copy`.
///
/// The caller must have restored the terminal first: nothing after this point runs.
pub fn stop_walk() -> ! {
    #[cfg(unix)]
    // SAFETY: `signal` and `kill` are async-signal-safe and take no pointers. Resetting the
    // disposition first means our own copy of the signal terminates us even if something
    // installed a handler; `kill(0, …)` addresses our process group, the tty's foreground group.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::kill(0, libc::SIGINT);
    }
    // Unreachable on Unix; elsewhere a non-zero status at least stops a `--trust-exit-code` walk.
    std::process::exit(EXIT_INTERRUPTED);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn label(old: &str, new: &str) -> String {
        diff_label(&PathBuf::from(old), &PathBuf::from(new))
    }

    #[test]
    fn a_modified_file_is_labelled_by_its_name() {
        assert_eq!(
            label("/tmp/git-difftool.a/left/docs/x.md", "docs/x.md"),
            "x.md"
        );
    }

    #[test]
    fn an_add_or_delete_is_labelled_by_the_side_that_exists() {
        assert_eq!(label("/dev/null", "docs/new.md"), "new.md");
        assert_eq!(label("docs/gone.md", "/dev/null"), "gone.md");
    }

    #[test]
    fn a_rename_names_both_sides() {
        assert_eq!(label("old/a.md", "new/b.md"), "a.md → b.md");
    }

    #[test]
    fn a_nameless_pair_falls_back_to_a_placeholder() {
        assert_eq!(label("/dev/null", "/dev/null"), "[diff]");
    }

    #[test]
    fn is_markdown_pair_accepts_either_side() {
        let md = PathBuf::from("notes.md");
        let devnull = PathBuf::from("/dev/null");
        assert!(is_markdown_pair(&devnull, &md));
        assert!(is_markdown_pair(&md, &devnull));
        assert!(is_markdown_pair(
            &PathBuf::from("a.markdown"),
            &PathBuf::from("b.MD")
        ));
    }

    #[test]
    fn is_markdown_pair_refuses_a_non_markdown_pair() {
        assert!(!is_markdown_pair(
            &PathBuf::from("src/main.rs"),
            &PathBuf::from("src/main.rs")
        ));
        assert!(!is_markdown_pair(
            &PathBuf::from("/dev/null"),
            &PathBuf::from("deploy.sh")
        ));
    }

    /// `PATH_TOTAL` alone must suffice: it is the only signal `difftool -t` raises.
    #[test]
    fn a_difftool_walk_is_recognised_from_either_variable() {
        assert!(is_difftool_env(true, false), "only -x sets EXTCMD");
        assert!(
            is_difftool_env(false, true),
            "-t and a gitattributes driver set PATH_TOTAL alone",
        );
        assert!(is_difftool_env(true, true));
    }

    #[test]
    fn a_hand_typed_invocation_is_not_a_difftool_walk() {
        assert!(!is_difftool_env(false, false));
    }

    /// Unix-only because it is about the OS null device; an empty file is covered below.
    #[test]
    #[cfg(unix)]
    fn read_side_reads_dev_null_as_an_empty_side() {
        assert_eq!(read_side(Path::new("/dev/null")).unwrap(), "");
    }

    #[test]
    fn read_side_reads_an_empty_file_as_an_empty_side() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.md");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(read_side(&path).unwrap(), "");
    }

    #[test]
    fn read_side_names_the_path_it_could_not_read() {
        let err = read_side(Path::new("/nonexistent/edamame-difftool-fixture.md"))
            .expect_err("missing path must fail");
        assert!(
            format!("{err}").contains("edamame-difftool-fixture.md"),
            "the error must name the file: {err}",
        );
    }
}
