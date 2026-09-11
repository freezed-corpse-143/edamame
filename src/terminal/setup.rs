use std::io::Stdout;

use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

/// The ratatui `Terminal` plus whether the kitty keyboard protocol is really
/// available.
///
/// That flag comes from `supports_keyboard_enhancement()`, **not** from pushing
/// the flags: `PushKeyboardEnhancementFlags` is write-only, so `execute!`
/// returns `Ok` on a terminal that ignored it entirely.  Deriving it from the
/// push reported `true` everywhere, including Apple Terminal.
pub struct TerminalSetup {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    pub keyboard_enhancement: bool,
}

/// Set up the terminal for TUI rendering: raw mode, alternate screen, and — where
/// supported — the kitty keyboard protocol, so chords like `Shift+Enter` can be
/// told apart from their legacy escape-code equivalents.
pub fn setup() -> Result<TerminalSetup> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // Turns a terminal-native paste into one `Event::Paste`, the only path that
    // works when the host terminal can reach the clipboard but this process
    // cannot (SSH, Wayland without data-control, WSL).
    let _ = execute!(stdout, EnableBracketedPaste);
    // Best-effort focus reporting; the editor hides its cursor while
    // unfocused.
    let _ = execute!(stdout, EnableFocusChange);
    // kitty's own detection procedure, and the only way to learn the answer:
    // the push is write-only.  Must run after `enable_raw_mode` and before the
    // App spawns its event reader — the query reads its reply off the tty, so a
    // competing reader would eat it.  A terminal answering neither query costs
    // crossterm's 2 s timeout; anything answering DA1 returns at once.
    let keyboard_enhancement = crossterm::terminal::supports_keyboard_enhancement()
        .inspect_err(|e| tracing::warn!(error = %e, "keyboard enhancement probe failed"))
        .unwrap_or(false);
    if keyboard_enhancement {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(TerminalSetup {
        terminal,
        keyboard_enhancement,
    })
}

/// Enable xterm mouse reporting.  Gated on `capabilities.mouse` by the caller:
/// on `TERM=linux` / `TERM=dumb` the escape bytes would be echoed as literal
/// output.
pub fn enable_mouse() -> Result<()> {
    execute!(std::io::stdout(), EnableMouseCapture)?;
    Ok(())
}

/// Disable mouse capture, so the terminal never keeps reporting after exit.
pub fn disable_mouse() {
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
}

/// Supported pointer shapes for [`set_pointer_shape`].  Modern OSC 22 terminals
/// take CSS cursor names, older ones X11 cursor-font names, so both are emitted
/// and whichever the terminal understands wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerShape {
    /// I-beam cursor: shown when the pointer is over editable text.
    Text,
    /// Pointing-hand cursor: shown over clickable elements (checkboxes, links).
    Hand,
    /// Default arrow cursor; restores the terminal's native cursor on
    /// shutdown.
    Default,
}

/// Ask the terminal to change the mouse pointer shape.  Best-effort; both name
/// dialects are emitted (see [`PointerShape`]), which costs ~20 bytes per update
/// and avoids probing which the host prefers.
pub fn set_pointer_shape(shape: PointerShape) {
    use std::io::Write;
    let (x11, css) = match shape {
        PointerShape::Text => ("xterm", "text"),
        PointerShape::Hand => ("hand2", "pointer"),
        PointerShape::Default => ("left_ptr", "default"),
    };
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]22;{x11}\x07\x1b]22;{css}\x07");
    let _ = stdout.flush();
}

/// Restore the terminal.  Must run before the process exits, even on error, or
/// the terminal is left in raw mode.
pub fn restore() -> Result<()> {
    set_pointer_shape(PointerShape::Default);
    disable_mouse();
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = execute!(std::io::stdout(), DisableFocusChange);
    let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// Re-enter the TUI after a suspension (shelling out to `$EDITOR`): [`setup`]
/// minus the `Terminal` construction, which the caller still owns.  Pass the
/// same flags the original setup got, so transient features stay consistent.
/// Alt-screen errors propagate; the optional features fail silently, as in
/// `setup`.
pub fn re_enter(mouse: bool, keyboard_enhancement: bool) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let _ = execute!(stdout, EnableBracketedPaste);
    let _ = execute!(stdout, EnableFocusChange);
    if keyboard_enhancement {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    if mouse {
        let _ = execute!(stdout, EnableMouseCapture);
    }
    Ok(())
}
