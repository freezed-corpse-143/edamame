//! Suspending the TUI to run `$VISUAL` / `$EDITOR` on a file, and the OS-handler fallback.
//!
//! [`App::run_external_editor`] owns the suspend/resume window; the entry points for
//! `config.toml`, the current buffer, and a theme file wrap it with their own save/reload.

use std::io::Stdout;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::modal;
use crate::config;
use crate::config::{Config, KeyMap, Theme};
use crate::terminal::ColorDepth;
use crate::ui::ModalKind;

use super::flash::MessageKind;
use super::theme_fallback;
use super::{App, AppEvent};

/// Result of [`App::run_external_editor`]; tells the caller whether a post-exit reload makes sense.
pub(super) enum ExternalEditorOutcome {
    /// No shell editor set; the path went to `open::that`. No suspend happened.
    OsHandler,
    /// The TUI could not be suspended. An error was already shown.
    SuspendFailed,
    /// The editor process ran (or failed to launch).
    Exited(std::io::Result<std::process::ExitStatus>),
}

impl App {
    /// Open `config.toml` in the user's editor, then reload the config and live-apply theme,
    /// keybindings, and the rest of the editor-facing settings.
    pub(super) fn open_config_in_editor(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        // `--no-config` excludes the config dir in both directions: the seeding save would
        // write it and the reload would pull the user's real settings in. Refuse the whole flow.
        if !config::config_writes_allowed() {
            self.notify(
                "The config file is not in use while --no-config is in effect",
                ModalKind::Warning,
            );
            return;
        }
        let Some(path) = Config::config_path() else {
            self.notify("No config directory available", ModalKind::Error);
            return;
        };
        if !path.exists() {
            if let Err(e) = self.config.save() {
                tracing::warn!(error = %e, "failed to seed config.toml before editor launch");
                self.notify(format!("Config save failed: {e}"), ModalKind::Error);
                return;
            }
        }

        let outcome = self.run_external_editor(&path, terminal, rx);

        // Reload even on the OS-handler fallback: the user may still have edited the file.
        let truecolor = self.capabilities.color_depth == ColorDepth::TrueColor;
        // `persist_fallback = false`: the user just hand-edited this file, so a theme whose
        // file is not on disk yet must not be silently rewritten out of it (startup goes the
        // other way; see `Config::load`).
        // `previous_downgrade` decides whether a substitution below is new (worth a modal) or
        // the one already acknowledged at startup.
        let previous_downgrade = self.config.theme_downgraded_from.clone();
        match Config::load(truecolor, false) {
            Ok(loaded) => {
                self.config = loaded.config;
                self.keybindings = loaded.keybindings;
                match KeyMap::build(&self.keybindings) {
                    Ok(km) => self.keymap = Some(km),
                    Err(e) => {
                        tracing::warn!(error = %e, "rebuilt KeyMap failed after editor exit");
                    }
                }
                // Re-apply the indexed-color substitution before building the theme: the reload
                // restored the on-disk palette, which on a non-truecolor terminal is exactly
                // the one swapped away at startup.
                let mut theme_file = loaded.theme;
                if let Some(d) = theme_fallback::apply(&mut self.config, &self.capabilities) {
                    theme_file = d.theme_file;
                    if previous_downgrade.as_deref() != Some(d.configured.as_str()) {
                        self.modal_stack
                            .push(Box::new(modal::ThemeDowngradeModal::new(
                                d.configured,
                                d.substituted,
                            )));
                        self.needs_draw = true;
                    }
                }
                let monochrome = self.capabilities.color_depth == ColorDepth::NoColor;
                let new_theme: &'static Theme =
                    Box::leak(Box::new(Theme::from_file(&theme_file, monochrome)));
                self.theme = new_theme;
                self.editor.set_theme(new_theme);
                // Everything else the editor reads out of `Config`, through the one site
                // `App::new` and the document-swap path share, so no setting is silently
                // deferred to the next launch.
                let (images_on, diagrams_on) =
                    (self.images_layout_enabled(), self.diagrams_layout_enabled());
                super::configure_new_editor(&mut self.editor, &self.config, images_on, diagrams_on);
                if let Some(modal) = modal::ConfigWarningModal::from_warnings(&loaded.warnings) {
                    self.modal_stack.push(Box::new(modal));
                    self.needs_draw = true;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to reload config after editor exit");
            }
        }

        match outcome {
            ExternalEditorOutcome::Exited(Ok(s)) if s.success() => {
                self.flash("Configuration updated", MessageKind::Success);
            }
            ExternalEditorOutcome::Exited(Ok(s)) => {
                // Non-zero exit is often deliberate (`:cq`), so a hint rather than a modal.
                self.flash(format!("Editor exited {s}"), MessageKind::Info);
            }
            ExternalEditorOutcome::Exited(Err(e)) => {
                self.notify(format!("Editor failed: {e}"), ModalKind::Error);
            }
            ExternalEditorOutcome::SuspendFailed | ExternalEditorOutcome::OsHandler => {}
        }
    }

    /// Save the current buffer, open it in the user's editor, and reload it from disk afterward
    /// so a later save from edamame cannot overwrite the external edits.
    pub(super) fn open_current_file_in_editor(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        let Some(path) = self.editor.buffer.path().map(|p| p.to_path_buf()) else {
            self.notify("No file path for buffer", ModalKind::Error);
            return;
        };

        if self.editor.dirty {
            if let Err(e) = self.save_buffer() {
                tracing::warn!(error = %e, "failed to save buffer before editor launch");
                self.notify(format!("Save failed: {e}"), ModalKind::Error);
                return;
            }
        }

        let outcome = self.run_external_editor(&path, terminal, rx);

        // Not on the OS-handler fallback: it returns immediately, and reloading while the user
        // is still editing elsewhere would discard their in-edamame edits.
        if matches!(outcome, ExternalEditorOutcome::Exited(_)) {
            if let Err(e) = self.load_file_into_editor(path) {
                tracing::warn!(error = %e, "failed to reload buffer after editor exit");
                self.notify(format!("Reload failed: {e}"), ModalKind::Error);
                return;
            }
        }

        match outcome {
            ExternalEditorOutcome::Exited(Ok(s)) if s.success() => {
                self.flash("File reloaded", MessageKind::Success);
            }
            ExternalEditorOutcome::Exited(Ok(s)) => {
                self.flash(format!("Editor exited {s}"), MessageKind::Info);
            }
            ExternalEditorOutcome::Exited(Err(e)) => {
                self.notify(format!("Editor failed: {e}"), ModalKind::Error);
            }
            ExternalEditorOutcome::SuspendFailed | ExternalEditorOutcome::OsHandler => {}
        }
    }

    /// Open a theme `.toml` in the user's editor and reload the active theme afterward.
    /// Reached from the success modal after `Action::CreateCustomTheme`.
    pub(super) fn open_theme_in_editor(
        &mut self,
        path: &Path,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        if !path.exists() {
            self.notify(
                format!("Theme file no longer exists: {}", path.display()),
                ModalKind::Error,
            );
            return;
        }
        let outcome = self.run_external_editor(path, terminal, rx);

        match outcome {
            ExternalEditorOutcome::Exited(Ok(s)) if s.success() => {
                self.apply_active_theme();
                self.flash("Theme reloaded", MessageKind::Success);
            }
            ExternalEditorOutcome::Exited(Ok(s)) => {
                self.apply_active_theme();
                self.flash(format!("Editor exited {s}"), MessageKind::Info);
            }
            ExternalEditorOutcome::Exited(Err(e)) => {
                self.notify(format!("Editor failed: {e}"), ModalKind::Error);
            }
            ExternalEditorOutcome::SuspendFailed | ExternalEditorOutcome::OsHandler => {}
        }
    }

    /// Suspend the TUI, run `$VISUAL` / `$EDITOR` on `path`, and resume. Owns only the
    /// suspend/resume window; callers handle any pre-launch save and post-exit reload.
    pub(super) fn run_external_editor(
        &mut self,
        path: &Path,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) -> ExternalEditorOutcome {
        let editor = std::env::var("VISUAL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            });

        let Some(editor) = editor else {
            self.spawn_open_worker(path.display().to_string());
            self.flash("Opening with system default", MessageKind::Info);
            return ExternalEditorOutcome::OsHandler;
        };

        // Pause the crossterm read thread so the editor has stdin to itself. Otherwise both
        // `read()` the same fd and bytes get split: the `1;rgb:...` artifact users reported was
        // neovim's OSC 11 reply with some bytes stolen by us.
        if let Some(p) = self.read_paused.as_ref() {
            p.store(true, Ordering::Release);
        }
        // Drop the watch for the duration: the editor's writes must not reach the main mpsc as
        // organic events. The forced reconcile on resume is how its edits get picked up.
        if let Some(w) = self.watcher.as_mut() {
            let _ = w.unwatch();
        }
        // The poll loop wakes every 100 ms; wait a little longer so the read thread has entered
        // its paused branch, then discard anything parsed during the overlap.
        std::thread::sleep(Duration::from_millis(120));
        while rx.try_recv().is_ok() {}

        if let Err(e) = crate::terminal::restore() {
            tracing::warn!(error = %e, "failed to suspend terminal for editor");
            self.notify(format!("Editor failed: {e}"), ModalKind::Error);
            if let Some(p) = self.read_paused.as_ref() {
                p.store(false, Ordering::Release);
            }
            return ExternalEditorOutcome::SuspendFailed;
        }

        let status = std::process::Command::new(&editor).arg(path).status();

        // Always re-enter, even if the editor failed, or the user is stranded half-suspended.
        let mouse = self.capabilities.mouse;
        let kbd = self.capabilities.keyboard_enhancement;
        let restore_result = crate::terminal::re_enter(mouse, kbd);
        if let Err(e) = restore_result {
            tracing::error!(error = %e, "failed to re-enter TUI after editor");
            self.notify(format!("Terminal restore failed: {e}"), ModalKind::Error);
        }
        // Some terminals acknowledge the re-enter sequences (kitty keyboard, mouse mode); keep
        // the read thread paused briefly so those bytes are drained rather than raced.
        std::thread::sleep(Duration::from_millis(30));
        while rx.try_recv().is_ok() {}
        if let Some(p) = self.read_paused.as_ref() {
            p.store(false, Ordering::Release);
        }
        // Re-arm the watcher and force a reconcile; both best-effort.
        if let Some(file_path) = self.file_path.clone() {
            if let Some(w) = self.watcher.as_mut() {
                if let Err(e) = w.watch(&file_path) {
                    tracing::warn!(target: "watcher", error = %e, "watch re-arm after editor failed");
                }
                if let Err(e) = w.force_reconcile() {
                    tracing::warn!(target: "watcher", error = %e, "force_reconcile after editor failed");
                }
            }
        }

        // Ratatui caches the previous frame; the clear forces a full redraw, and it also wiped
        // whatever a graphics protocol had on screen.
        let _ = terminal.clear();
        self.editor.images.invalidate_native_paints();
        self.needs_draw = true;

        ExternalEditorOutcome::Exited(status)
    }

    /// Call `open::that` on a worker thread (xdg-open can take hundreds of ms) and report the
    /// outcome via `AppEvent::LinkOpenResult`.
    pub(super) fn spawn_open_worker(&self, target: String) {
        let Some(tx) = self.app_tx.clone() else {
            return;
        };
        std::thread::spawn(move || {
            let result = open::that(&target).map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::LinkOpenResult(result));
        });
    }
}
