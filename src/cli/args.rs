//! Argument parsing: `Vec<OsString>` → [`Invocation`].  [`Invocation::parse`] is pure, so every
//! rule is unit-testable.
//!
//! Arguments are [`OsString`], not `String`: `std::env::args()` *panics* on a non-UTF-8
//! argument, which on Linux is a legal file name.  Flags are matched only after a successful
//! `to_str()`, so such a path falls through to the positional arm and reaches `PathBuf` intact.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What the command line asked edamame to do.  The informational variants print and exit; only
/// [`Invocation::Run`] and [`Invocation::Diff`] continue into terminal setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Start the editor.
    Run {
        /// File to open; `None` starts an empty, unnamed buffer.
        file: Option<PathBuf>,
        opts: RunOpts,
    },
    /// Print the flag list and exit.
    Help,
    /// Print `edamame <version>` and exit.
    Version,
    /// Print the diagnostic report and exit.
    Doctor,
    /// Read-only review of two files (`--diff <old> <new>`), for use as a `git difftool`.
    ///
    /// A variant rather than a `RunOpts` flag: it takes two paths, opens nothing for editing,
    /// and never starts the watcher — git's paths are usually temp files it deletes on exit.
    Diff {
        /// The "before" side — git's `$LOCAL`.
        old: PathBuf,
        /// The "after" side — git's `$REMOTE`.
        new: PathBuf,
        opts: RunOpts,
    },
}

/// Flags that modify a normal editor run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunOpts {
    /// `--no-config`: ignore `~/.config/edamame` entirely — no
    /// scaffolding, no reads, and (via `Config::persist`) no writes.
    pub no_config: bool,
    /// `--log`: force `[dev] logging = true` for this run without
    /// editing `config.toml`.
    pub log: bool,
}

/// A command line edamame can't act on.  Printed to stderr alongside
/// [`super::USAGE`], with exit status 2.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    #[error("unknown option '{0}'")]
    UnknownOption(String),
    /// One file per process; a second positional is more likely a typo'd flag than an intent
    /// worth guessing.  The parenthetical matters: this same error covers a three-path
    /// `--diff`, where the bare rule would contradict the command the user just ran.
    #[error("unexpected argument '{0}' — edamame opens one file at a time (two with --diff)")]
    ExtraArgument(String),
    /// A bare `-` means stdin, which edamame cannot read — the capability probe needs stdin
    /// for itself.  Rejecting it beats a confusing "no such file: -".
    #[error("reading from stdin is not supported")]
    StdinNotSupported,
    /// Neither `--diff` path has a defensible default; guessing would silently review
    /// something other than what git asked for.
    #[error("--diff needs exactly two files: edamame --diff <old> <new>")]
    DiffNeedsTwoFiles,
}

impl Invocation {
    /// Parse arguments **excluding** `argv[0]`.
    ///
    /// Informational flags outrank a run — `--help`, then `--version`, then `--doctor` — and a
    /// file argument alongside one of them is ignored rather than rejected.  `--` ends flag
    /// parsing, so a file named `--doctor` is reachable as `edamame -- --doctor`.
    ///
    /// Positionals are collected into a list because `--diff` takes two and may appear *after*
    /// them (`edamame a.md b.md --diff`).  The "one file at a time" rule is therefore enforced
    /// at the end, once the flags are known, so a stray extra file cannot suppress `--help`.
    pub fn parse<I>(args: I) -> Result<Self, CliError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut files: Vec<PathBuf> = Vec::new();
        // The second positional, remembered verbatim so a non-`--diff` run names *it* in the
        // error rather than whichever argument overflowed the list.
        let mut extra: Option<String> = None;
        let mut opts = RunOpts::default();
        let mut help = false;
        let mut version = false;
        let mut doctor = false;
        let mut diff = false;
        let mut positional_only = false;

        for arg in args {
            // A non-UTF-8 argument can never match a flag; skip to the positional arm.
            let flag = if positional_only {
                None
            } else {
                arg.to_str().filter(|s| s.starts_with('-'))
            };

            match flag {
                Some("--") => positional_only = true,
                Some("-h" | "--help") => help = true,
                Some("-V" | "--version") => version = true,
                Some("--doctor") => doctor = true,
                Some("--diff") => diff = true,
                Some("--no-config") => opts.no_config = true,
                Some("--log") => opts.log = true,
                Some("-") => return Err(CliError::StdinNotSupported),
                Some(other) => return Err(CliError::UnknownOption(other.to_owned())),
                None => {
                    // Two is the most any mode accepts, so a third can never become valid.
                    if files.len() >= 2 {
                        return Err(CliError::ExtraArgument(arg.to_string_lossy().into_owned()));
                    }
                    if files.len() == 1 {
                        extra = Some(arg.to_string_lossy().into_owned());
                    }
                    files.push(PathBuf::from(arg));
                }
            }
        }

        if help {
            return Ok(Self::Help);
        }
        if version {
            return Ok(Self::Version);
        }
        if doctor {
            return Ok(Self::Doctor);
        }
        if diff {
            let mut it = files.into_iter();
            let (Some(old), Some(new)) = (it.next(), it.next()) else {
                return Err(CliError::DiffNeedsTwoFiles);
            };
            return Ok(Self::Diff { old, new, opts });
        }
        if let Some(extra) = extra {
            return Err(CliError::ExtraArgument(extra));
        }
        Ok(Self::Run {
            file: files.pop(),
            opts,
        })
    }
}

/// Split a `file.md#section` startup argument into file and heading — the command-line half of
/// deep linking.
///
/// Kept out of the pure [`Invocation::parse`] because `#` is legal in a file name, so the split
/// must ask the disk: the literal path wins, and only when it does not exist is the text after
/// the *last* `#` taken as a heading (both halves must be non-empty).  A non-UTF-8 argument is
/// never split.
pub fn split_startup_anchor(arg: &Path) -> (PathBuf, Option<String>) {
    split_startup_anchor_with(arg, |p| p.exists())
}

/// [`split_startup_anchor`] with the disk lookup injected, for testing.
fn split_startup_anchor_with(
    arg: &Path,
    exists: impl Fn(&Path) -> bool,
) -> (PathBuf, Option<String>) {
    let keep = || (arg.to_path_buf(), None);
    let Some(text) = arg.to_str() else {
        return keep();
    };
    // The *last* `#`, so a directory carrying one still resolves: `~/notes#2024/index.md#intro`.
    let Some((path, fragment)) = text.rsplit_once('#') else {
        return keep();
    };
    if path.is_empty() || fragment.is_empty() || exists(arg) {
        return keep();
    }
    (PathBuf::from(path), Some(fragment.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Invocation, CliError> {
        Invocation::parse(args.iter().map(OsString::from))
    }

    fn run(args: &[&str]) -> (Option<PathBuf>, RunOpts) {
        match parse(args).expect("parses") {
            Invocation::Run { file, opts } => (file, opts),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn no_arguments_starts_an_empty_buffer() {
        assert_eq!(run(&[]), (None, RunOpts::default()));
    }

    #[test]
    fn a_bare_path_is_the_file_to_open() {
        let (file, opts) = run(&["notes.md"]);
        assert_eq!(file, Some(PathBuf::from("notes.md")));
        assert_eq!(opts, RunOpts::default());
    }

    #[test]
    fn run_flags_combine_with_a_file_in_any_order() {
        let expected = RunOpts {
            no_config: true,
            log: true,
        };
        for args in [
            ["--no-config", "--log", "notes.md"],
            ["notes.md", "--no-config", "--log"],
            ["--log", "notes.md", "--no-config"],
        ] {
            let (file, opts) = run(&args);
            assert_eq!(file, Some(PathBuf::from("notes.md")), "{args:?}");
            assert_eq!(opts, expected, "{args:?}");
        }
    }

    #[test]
    fn informational_flags_have_both_spellings() {
        assert_eq!(parse(&["--help"]), Ok(Invocation::Help));
        assert_eq!(parse(&["-h"]), Ok(Invocation::Help));
        assert_eq!(parse(&["--version"]), Ok(Invocation::Version));
        assert_eq!(parse(&["-V"]), Ok(Invocation::Version));
        assert_eq!(parse(&["--doctor"]), Ok(Invocation::Doctor));
    }

    /// Help outranks version outranks doctor, and a stray file never turns a question into an
    /// error.
    #[test]
    fn informational_flags_win_over_a_run() {
        assert_eq!(
            parse(&["--doctor", "--version", "--help"]),
            Ok(Invocation::Help)
        );
        assert_eq!(parse(&["--doctor", "--version"]), Ok(Invocation::Version));
        assert_eq!(parse(&["notes.md", "--doctor"]), Ok(Invocation::Doctor));
        assert_eq!(
            parse(&["--no-config", "--version"]),
            Ok(Invocation::Version)
        );
    }

    /// `--` is the escape hatch for a file whose name looks like a flag.
    #[test]
    fn double_dash_ends_flag_parsing() {
        let (file, opts) = run(&["--log", "--", "--doctor"]);
        assert_eq!(file, Some(PathBuf::from("--doctor")));
        assert!(opts.log, "flags before `--` still apply");

        // Everything after `--` is positional, so a second one is an extra-argument error.
        assert_eq!(
            parse(&["--", "a.md", "--"]),
            Err(CliError::ExtraArgument("--".to_owned()))
        );
    }

    #[test]
    fn unknown_flags_and_extra_files_are_rejected() {
        assert_eq!(
            parse(&["--doctorr"]),
            Err(CliError::UnknownOption("--doctorr".to_owned()))
        );
        assert_eq!(
            parse(&["-x"]),
            Err(CliError::UnknownOption("-x".to_owned()))
        );
        // Bundled short flags are deliberately not supported.
        assert_eq!(
            parse(&["-hV"]),
            Err(CliError::UnknownOption("-hV".to_owned()))
        );
        assert_eq!(
            parse(&["a.md", "b.md"]),
            Err(CliError::ExtraArgument("b.md".to_owned()))
        );
    }

    // ── --diff ───────────────────────────────────────────────────

    fn diff(args: &[&str]) -> (PathBuf, PathBuf, RunOpts) {
        match parse(args).expect("parses") {
            Invocation::Diff { old, new, opts } => (old, new, opts),
            other => panic!("expected Diff, got {other:?}"),
        }
    }

    #[test]
    fn diff_takes_two_paths_in_git_difftool_order() {
        let (old, new, opts) = diff(&["--diff", "left.md", "right.md"]);
        assert_eq!(old, PathBuf::from("left.md"));
        assert_eq!(new, PathBuf::from("right.md"));
        assert_eq!(opts, RunOpts::default());
    }

    /// The flag may trail its operands: `difftool.<tool>.cmd` is a shell string users reorder
    /// freely.
    #[test]
    fn diff_accepts_the_flag_in_any_position() {
        for args in [
            ["--diff", "a.md", "b.md"],
            ["a.md", "--diff", "b.md"],
            ["a.md", "b.md", "--diff"],
        ] {
            let (old, new, _) = diff(&args);
            assert_eq!((old, new), (PathBuf::from("a.md"), PathBuf::from("b.md")));
        }
    }

    #[test]
    fn diff_combines_with_the_run_flags() {
        let (_, _, opts) = diff(&["--diff", "--no-config", "a.md", "--log", "b.md"]);
        assert_eq!(
            opts,
            RunOpts {
                no_config: true,
                log: true
            }
        );
    }

    /// Neither side has a defensible default, so a short command line is an error.
    #[test]
    fn diff_with_fewer_than_two_files_is_an_error() {
        assert_eq!(parse(&["--diff"]), Err(CliError::DiffNeedsTwoFiles));
        assert_eq!(
            parse(&["--diff", "only.md"]),
            Err(CliError::DiffNeedsTwoFiles)
        );
    }

    #[test]
    fn diff_still_rejects_a_third_file() {
        assert_eq!(
            parse(&["--diff", "a.md", "b.md", "c.md"]),
            Err(CliError::ExtraArgument("c.md".to_owned()))
        );
    }

    /// `--diff` is a run, so the informational flags still outrank it.
    #[test]
    fn informational_flags_win_over_a_diff() {
        assert_eq!(
            parse(&["--diff", "a.md", "b.md", "--help"]),
            Ok(Invocation::Help)
        );
        assert_eq!(parse(&["--diff", "--version"]), Ok(Invocation::Version));
    }

    /// Without `--diff` the second positional errors, naming the second file rather than
    /// whichever argument overflowed the list.
    #[test]
    fn two_files_without_diff_still_name_the_second_one() {
        assert_eq!(
            parse(&["a.md", "b.md"]),
            Err(CliError::ExtraArgument("b.md".to_owned()))
        );
        assert_eq!(
            parse(&["a.md", "b.md", "c.md"]),
            Err(CliError::ExtraArgument("c.md".to_owned()))
        );
    }

    #[test]
    fn a_bare_dash_is_refused_rather_than_opened_as_a_file() {
        assert_eq!(parse(&["-"]), Err(CliError::StdinNotSupported));
    }

    /// `std::env::args()` panics on a non-UTF-8 argument; `OsString` lets one survive.
    #[cfg(unix)]
    #[test]
    fn non_utf8_file_names_survive() {
        use std::os::unix::ffi::OsStringExt;

        let raw = OsString::from_vec(vec![b'n', 0xff, b'.', b'm', b'd']);
        let parsed = Invocation::parse([raw.clone()]).expect("parses");
        assert_eq!(
            parsed,
            Invocation::Run {
                file: Some(PathBuf::from(raw)),
                opts: RunOpts::default(),
            }
        );
    }
    // ── Startup anchor (`file.md#section`) ────────────────────────────

    /// Nothing on disk under either name — the `#` is a deep link.
    fn split(arg: &str) -> (PathBuf, Option<String>) {
        split_startup_anchor_with(Path::new(arg), |_| false)
    }

    #[test]
    fn startup_argument_splits_off_its_section() {
        assert_eq!(
            split("docs/editing.md#images"),
            (PathBuf::from("docs/editing.md"), Some("images".to_owned()))
        );
    }

    #[test]
    fn startup_argument_without_a_section_is_untouched() {
        assert_eq!(
            split("docs/editing.md"),
            (PathBuf::from("docs/editing.md"), None)
        );
    }

    #[test]
    fn an_empty_half_is_not_a_section() {
        assert_eq!(split("notes.md#"), (PathBuf::from("notes.md#"), None));
        assert_eq!(split("#notes.md"), (PathBuf::from("#notes.md"), None));
    }

    #[test]
    fn only_the_last_hash_splits() {
        assert_eq!(
            split("notes#2024/index.md#intro"),
            (
                PathBuf::from("notes#2024/index.md"),
                Some("intro".to_owned())
            )
        );
    }

    #[test]
    fn a_file_really_named_with_a_hash_wins_over_the_split() {
        let arg = Path::new("notes#1.md");
        assert_eq!(
            split_startup_anchor_with(arg, |p| p == Path::new("notes#1.md")),
            (PathBuf::from("notes#1.md"), None)
        );
    }
}
