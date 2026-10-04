//! Terminal setup and quirks: raw mode / alternate screen lifecycle, tmux
//! zoom, and detecting which graphics protocol album art can use.

use crossterm::event::{DisableFocusChange, EnableFocusChange};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, execute};
use ratatui_image::picker::{Picker, ProtocolType};
use std::io::stdout;

/// Enters raw mode and the alternate screen; restores both on drop.
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn new() -> std::io::Result<Self> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen, EnableFocusChange)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = stdout().execute(DisableFocusChange);
        let _ = stdout().execute(LeaveAlternateScreen);
    }
}

/// Hands the terminal back to the shell for a moment (sign-in prints a link and
/// waits in the normal screen). Pair with [`resume_tui`].
pub fn suspend_tui() {
    let _ = disable_raw_mode();
    let _ = stdout().execute(DisableFocusChange);
    let _ = stdout().execute(LeaveAlternateScreen);
}

pub fn resume_tui() -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableFocusChange)
}

/// Restores the terminal before the default hook prints, so a panic message
/// is readable instead of landing inside the alternate screen.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        default_hook(info);
    }));
}

/// Zooms/unzooms the tmux pane, so fullscreen Now Playing fills the window.
/// No-op outside tmux.
pub fn tmux_toggle_zoom() {
    if std::env::var("TMUX").is_ok() {
        let _ = std::process::Command::new("tmux")
            .args(["resize-pane", "-Z"])
            .status();
    }
}

/// Whether the real terminal, possibly wrapped in tmux, is Ghostty.
///
/// `TERM`/`TERM_PROGRAM` are not enough: tmux overwrites both for every
/// pane. It does still know the outer client's terminal, and reports it via
/// `display-message '#{client_termtype}'` (e.g. `ghostty 1.3.1`).
fn is_ghostty() -> bool {
    if std::env::var("TERM_PROGRAM").is_ok_and(|t| t.eq_ignore_ascii_case("ghostty"))
        || std::env::var("TERM").is_ok_and(|t| t.contains("ghostty"))
    {
        return true;
    }
    if std::env::var("TMUX").is_ok()
        && let Ok(output) = std::process::Command::new("tmux")
            .args(["display-message", "-p", "#{client_termtype}"])
            .output()
    {
        return String::from_utf8_lossy(&output.stdout)
            .to_lowercase()
            .contains("ghostty");
    }
    false
}

/// Probes the terminal for a graphics protocol (Kitty, iTerm2, Sixel), with
/// two corrections for what the stock probe gets wrong. `None` means
/// detection failed and album art falls back to a text placeholder.
///
/// Must run after entering the alternate screen and before the event loop
/// starts reading input, because the probe reads stdin itself.
pub fn detect_graphics_picker() -> Option<Picker> {
    let Some(mut picker) = Picker::from_query_stdio().ok() else {
        log::warn!("graphics protocol detection failed, falling back to text/placeholder art");
        return None;
    };

    // ratatui-image has no Ghostty handling, and in Ghostty the probe settles
    // on Halfblocks even though Ghostty implements the Kitty protocol.
    if picker.protocol_type() == ProtocolType::Halfblocks && is_ghostty() {
        log::info!("probe said Halfblocks but the terminal is Ghostty, using Kitty");
        picker.set_protocol_type(ProtocolType::Kitty);
    }
    log::info!("graphics protocol in use: {:?}", picker.protocol_type());

    // The probe also misreports the cell size, which makes the encoder send
    // images at a lower resolution than the cells they are stretched over
    // (visibly pixelated). The window-size ioctl is exact. `Picker` has no
    // font-size setter, so rebuild it with the deprecated `from_fontsize`.
    if let Ok(win) = crossterm::terminal::window_size()
        && win.width > 0
        && win.height > 0
        && win.columns > 0
        && win.rows > 0
    {
        let (cell_w, cell_h) = (win.width / win.columns, win.height / win.rows);
        if cell_w > 0 && cell_h > 0 {
            let protocol = picker.protocol_type();
            #[allow(deprecated)]
            let mut corrected = Picker::from_fontsize(ratatui_image::FontSize::new(cell_w, cell_h));
            corrected.set_protocol_type(protocol);
            log::info!("corrected picker cell size to {cell_w}x{cell_h}px");
            picker = corrected;
        }
    }
    Some(picker)
}
