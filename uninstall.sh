#!/bin/sh
# shellcheck shell=sh
#
# edamame uninstaller.
#
# Removes the edamame binary installed by the shell installer or copied by hand,
# and optionally the configuration and data directories that no installer touches.
#
# Homebrew and `cargo install` manage the binary themselves, so this script does
# not delete a binary owned by either: it prints the right `brew`/`cargo`
# command and still offers to remove the config and data directories they leave
# behind.
#
# Usage:
#   curl -LsSf https://raw.githubusercontent.com/mijowi/edamame/main/uninstall.sh | sh
#
# Flags (pass with `... | sh -s -- --purge`, or when running a local copy):
#   --purge        remove the binary AND the config/data directories, no prompt
#   --yes, -y      non-interactive; remove only the binary, keep config/data
#   --help, -h     print this help and exit
#
# Environment overrides (mirror the installer / the app):
#   EDAMAME_INSTALL_DIR   where the binary was installed
#   CARGO_HOME            defaults to ~/.cargo
#   XDG_CONFIG_HOME       config dir base (config lives at <base>/edamame)
#   XDG_DATA_HOME         data dir base on Linux (ignored on macOS)

set -u

APP_NAME="edamame"

# ── Flags ───────────────────────────────────────────────────────────────────

PURGE=0        # remove config/data without asking
ASSUME_YES=0   # never prompt; keep config/data (the safe default)

usage() {
    cat <<EOF
edamame uninstaller

Removes the edamame binary (when it was installed by the shell installer or
copied by hand) and, on request, the config and data directories.

Usage:
  curl -LsSf https://raw.githubusercontent.com/mijowi/edamame/main/uninstall.sh | sh
  sh uninstall.sh [--purge | --yes]

Options:
  --purge       also remove config and data directories, without prompting
  --yes, -y     non-interactive; remove only the binary, keep config/data
  --help, -h    show this help
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --purge) PURGE=1 ;;
        --yes | -y) ASSUME_YES=1 ;;
        --help | -h)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown option '$1'" >&2
            usage >&2
            exit 1
            ;;
    esac
    shift
done

# ── Output helpers ──────────────────────────────────────────────────────────

say() { printf '%s\n' "$1"; }
warn() { printf 'warning: %s\n' "$1" >&2; }
err() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

# Read a single line from the controlling terminal, so prompts work even when
# the script is being piped into `sh` (stdin is then the script itself).
read_tty() {
    # `[ -r /dev/tty ]` can pass while the open still fails (e.g. no controlling
    # terminal under setsid), so guard the read itself and swallow its error.
    # stderr is redirected first, on purpose: a failing `< /dev/tty` open reports
    # to whatever fd 2 is at that point, so it must already be /dev/null.
    # shellcheck disable=SC2039  # read -r is POSIX
    read -r "$1" 2>/dev/null < /dev/tty
}

# ── Path resolution ─────────────────────────────────────────────────────────

home_dir() {
    if [ -n "${HOME:-}" ]; then
        echo "$HOME"
    elif [ -n "${USER:-}" ]; then
        getent passwd "$USER" 2>/dev/null | cut -d: -f6
    else
        getent passwd "$(id -un)" 2>/dev/null | cut -d: -f6
    fi
}
HOME_DIR="$(home_dir)"
[ -n "$HOME_DIR" ] || err "could not determine your home directory"

# Best-effort canonical path; falls back to the input if nothing is available.
canonicalize() {
    _p="$1"
    if command -v realpath >/dev/null 2>&1; then
        realpath "$_p" 2>/dev/null || echo "$_p"
    elif command -v readlink >/dev/null 2>&1 && readlink -f "$_p" >/dev/null 2>&1; then
        readlink -f "$_p"
    else
        echo "$_p"
    fi
}

# Config dir: XDG_CONFIG_HOME if absolute, else ~/.config — on every platform,
# macOS included (matches Config::config_dir).
config_dir() {
    case "${XDG_CONFIG_HOME:-}" in
        /*) echo "${XDG_CONFIG_HOME}/${APP_NAME}" ;;
        *) echo "${HOME_DIR}/.config/${APP_NAME}" ;;
    esac
}

# Data dir mirrors dirs::data_dir(): ~/Library/Application Support on macOS
# (XDG is NOT honored there), else XDG_DATA_HOME or ~/.local/share.
data_dir() {
    case "$(uname -s)" in
        Darwin) echo "${HOME_DIR}/Library/Application Support/${APP_NAME}" ;;
        *)
            case "${XDG_DATA_HOME:-}" in
                /*) echo "${XDG_DATA_HOME}/${APP_NAME}" ;;
                *) echo "${HOME_DIR}/.local/share/${APP_NAME}" ;;
            esac
            ;;
    esac
}

CONFIG_DIR="$(config_dir)"
DATA_DIR="$(data_dir)"

# ── Locate the binary ───────────────────────────────────────────────────────

BIN_PATH=""
if command -v "$APP_NAME" >/dev/null 2>&1; then
    BIN_PATH="$(command -v "$APP_NAME")"
else
    # Not on PATH; probe the places the installer and cargo use.
    CARGO_BIN="${CARGO_HOME:-${HOME_DIR}/.cargo}/bin"
    _candidates="${CARGO_BIN}/$APP_NAME
${HOME_DIR}/.local/bin/$APP_NAME
/usr/local/bin/$APP_NAME"
    # Only probe a custom install dir when one is actually set, so an empty
    # EDAMAME_INSTALL_DIR does not turn into "/bin/edamame" and match a stray file.
    if [ -n "${EDAMAME_INSTALL_DIR:-}" ]; then
        _candidates="${EDAMAME_INSTALL_DIR}/bin/$APP_NAME
${EDAMAME_INSTALL_DIR}/$APP_NAME
${_candidates}"
    fi
    # Iterate newline-separated candidates (paths here never contain newlines).
    _oldifs="$IFS"
    IFS='
'
    for _cand in $_candidates; do
        if [ -f "$_cand" ]; then
            BIN_PATH="$_cand"
            break
        fi
    done
    IFS="$_oldifs"
fi

# ── Detect how the binary was installed ─────────────────────────────────────
#
# "brew"   — Homebrew manages it: defer to `brew uninstall`.
# "cargo"  — `cargo install` registered it: defer to `cargo uninstall`.
# "plain"  — shell installer or a manual copy: safe to remove directly.
# ""       — no binary found.

INSTALL_METHOD=""

detect_method() {
    [ -n "$BIN_PATH" ] || return 0

    # Homebrew: the formula is registered, or the binary sits under brew's prefix.
    if command -v brew >/dev/null 2>&1; then
        if brew list "$APP_NAME" >/dev/null 2>&1; then
            INSTALL_METHOD="brew"
            return 0
        fi
        _brew_prefix="$(brew --prefix 2>/dev/null || true)"
        if [ -n "$_brew_prefix" ]; then
            _resolved="$(canonicalize "$BIN_PATH")"
            case "$_resolved" in
                "${_brew_prefix}"/*)
                    INSTALL_METHOD="brew"
                    return 0
                    ;;
            esac
        fi
    fi

    # cargo install records the crate here; the shell installer lands in the same
    # ~/.cargo/bin but never writes this file, so it distinguishes the two.
    _crates="${CARGO_HOME:-${HOME_DIR}/.cargo}/.crates.toml"
    if [ -f "$_crates" ] && grep -q "^\"${APP_NAME} " "$_crates" 2>/dev/null; then
        INSTALL_METHOD="cargo"
        return 0
    fi

    INSTALL_METHOD="plain"
}
detect_method

# ── Remove the binary ───────────────────────────────────────────────────────

BINARY_REMOVED=0

case "$INSTALL_METHOD" in
    brew)
        say "Homebrew manages this install. To remove the binary, run:"
        say ""
        say "    brew uninstall $APP_NAME"
        say ""
        ;;
    cargo)
        say "This binary was installed with cargo. To remove it, run:"
        say ""
        say "    cargo uninstall $APP_NAME"
        say ""
        ;;
    plain)
        if rm -f "$BIN_PATH" 2>/dev/null; then
            say "Removed binary: $BIN_PATH"
            BINARY_REMOVED=1
        else
            warn "could not remove $BIN_PATH — retrying with elevated permissions"
            if command -v sudo >/dev/null 2>&1 && sudo rm -f "$BIN_PATH"; then
                say "Removed binary: $BIN_PATH"
                BINARY_REMOVED=1
            else
                warn "failed to remove $BIN_PATH; delete it manually"
            fi
        fi
        ;;
    *)
        say "No $APP_NAME binary found on PATH or in the usual locations."
        say "If you know where it is, delete that file by hand."
        ;;
esac

# Remove leftovers that are unambiguously edamame's. The PATH plumbing is NOT
# touched: the shell installer routes PATH through cargo's own ~/.cargo/env and
# the shared `. "$HOME/.cargo/env"` rc-file line, so ~/.cargo/bin stays on PATH
# for every other cargo tool. Only the edamame-named fish fragment is ours.
FISH_ENV="${HOME_DIR}/.config/fish/conf.d/${APP_NAME}.env.fish"
if [ -f "$FISH_ENV" ]; then
    rm -f "$FISH_ENV" && say "Removed fish PATH fragment: $FISH_ENV"
fi
RECEIPT="${CONFIG_DIR}/${APP_NAME}-receipt.json"
if [ -f "$RECEIPT" ]; then
    rm -f "$RECEIPT" && say "Removed install receipt: $RECEIPT"
fi

# ── Configuration and data directories ──────────────────────────────────────

remove_user_dirs() {
    _removed_any=0
    for _dir in "$CONFIG_DIR" "$DATA_DIR"; do
        if [ -d "$_dir" ]; then
            if rm -rf "$_dir"; then
                say "Removed: $_dir"
                _removed_any=1
            else
                warn "could not remove $_dir"
            fi
        fi
    done
    [ "$_removed_any" = 1 ] || say "No config or data directories to remove."
}

CONFIG_EXISTS=0
[ -d "$CONFIG_DIR" ] && CONFIG_EXISTS=1
[ -d "$DATA_DIR" ] && CONFIG_EXISTS=1

if [ "$PURGE" = 1 ]; then
    say ""
    remove_user_dirs
elif [ "$CONFIG_EXISTS" = 0 ]; then
    : # nothing to offer
elif [ "$ASSUME_YES" = 1 ]; then
    say ""
    say "Keeping your configuration and data (pass --purge to remove them):"
    [ -d "$CONFIG_DIR" ] && say "    $CONFIG_DIR"
    [ -d "$DATA_DIR" ] && say "    $DATA_DIR"
else
    say ""
    say "These hold your themes, keybindings, and settings — no installer removes them:"
    [ -d "$CONFIG_DIR" ] && say "    $CONFIG_DIR"
    [ -d "$DATA_DIR" ] && say "    $DATA_DIR"
    say ""
    printf 'Remove them too? [y/N] '
    if read_tty _answer; then
        case "$_answer" in
            y | Y | yes | YES)
                remove_user_dirs
                ;;
            *)
                say "Kept."
                ;;
        esac
    else
        say "No terminal available to confirm; kept. Re-run with --purge to remove them."
    fi
fi

# ── Closing note ────────────────────────────────────────────────────────────

if [ "$BINARY_REMOVED" = 1 ]; then
    CARGO_BIN="${CARGO_HOME:-${HOME_DIR}/.cargo}/bin"
    case "$BIN_PATH" in
        "$CARGO_BIN"/*)
            say ""
            say "Note: $CARGO_BIN stays on your PATH — it belongs to cargo, not edamame."
            ;;
    esac
fi

say ""
case "$INSTALL_METHOD" in
    brew | cargo)
        say "Finish up by running the command above to remove the binary."
        ;;
    *)
        say "edamame has been uninstalled. Thanks for trying it."
        ;;
esac
