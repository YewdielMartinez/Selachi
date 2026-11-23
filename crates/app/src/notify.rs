//! Windows toast for long commands (PLAN A4).
//!
//! A command that ran at least 10 s notifies when the window is not focused
//! (`term_core::should_notify`). The toast shows the command (secrets redacted), how long it ran
//! and the exit code; clicking it brings the window forward and selects the tab. Toasts are posted
//! from a short-lived thread, so nothing here costs anything while idle.
//!
//! Unpackaged apps cannot register their own toast identity without a Start-menu shortcut, so the
//! toast is sent under the PowerShell app id (that is what the source line of the toast says)
//! until the installer (MSIX) registers "Selachi".

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use term_core::{Block, BlockState};

static ACTIVE: AtomicBool = AtomicBool::new(true);
static CLICKS: OnceLock<UnboundedSender<u64>> = OnceLock::new();

/// The shell tells us whether the window has the focus (it observes activation changes).
pub fn set_window_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
}

pub fn window_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Stream of tab ids whose toast was clicked (call once, from the shell).
pub fn click_receiver() -> UnboundedReceiver<u64> {
    let (tx, rx) = unbounded();
    let _ = CLICKS.set(tx);
    rx
}

/// `42 s`, `3 min 05 s`, `1 h 02 min`.
pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, 0, s) => format!("{s} s"),
        (0, m, s) => format!("{m} min {s:02} s"),
        (h, m, _) => format!("{h} h {m:02} min"),
    }
}

/// Title and two lines of a command's toast.
pub fn toast_text(block: &Block) -> (String, String, String) {
    let ok = block.state == BlockState::Succeeded;
    let title = if ok { "Comando terminado" } else { "Comando fallido" };
    let command = block.command.as_deref().map(|c| term_core::redact(c).0).unwrap_or_default();
    let command = command.lines().next().unwrap_or("").trim().to_string();
    let command = if command.chars().count() > 90 {
        format!("{}…", command.chars().take(89).collect::<String>())
    } else {
        command
    };
    let took = block.duration.map(format_duration).unwrap_or_default();
    let detail = match (ok, block.exit_code) {
        (true, _) => format!("Terminó en {took}"),
        (false, Some(code)) => format!("Falló en {took} (código {code})"),
        (false, None) => format!("Terminó en {took} sin código de salida"),
    };
    (title.to_string(), command, detail)
}

/// Posts the toast for `tab` (the tab entity id) from a short-lived thread.
pub fn long_command_toast(tab: u64, block: &Block) {
    let (title, line1, line2) = toast_text(block);
    toast(tab, title, line1, line2);
}

/// A toast for an agent that needs the user (approval, waiting, failed). Clicking it brings the
/// window and the tab forward, like the long-command one. `detail` must already be redacted.
pub fn attention_toast(tab: u64, title: &str, line1: &str, line2: &str) {
    toast(tab, title.to_string(), clip(line1, 90), clip(line2, 120));
}

/// `s` cut to `max` characters with an ellipsis, first line only.
pub fn clip(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > max { format!("{}…", line.chars().take(max.saturating_sub(1)).collect::<String>()) } else { line.to_string() }
}

#[cfg(windows)]
fn toast(tab: u64, title: String, line1: String, line2: String) {
    use tauri_winrt_notification::Toast;
    let spawned = std::thread::Builder::new().name("selachi-toast".into()).spawn(move || {
        let toast = Toast::new(Toast::POWERSHELL_APP_ID)
            .title(&title)
            .text1(&line1)
            .text2(&line2)
            .on_activated(move |_| {
                if let Some(tx) = CLICKS.get() {
                    let _ = tx.unbounded_send(tab);
                }
                Ok(())
            });
        if let Err(e) = toast.show() {
            log::warn!("could not show the notification: {e}");
        }
    });
    if let Err(e) = spawned {
        log::warn!("could not start the notification thread: {e}");
    }
}

#[cfg(not(windows))]
fn toast(_tab: u64, _title: String, _line1: String, _line2: String) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_one_short_line() {
        assert_eq!(clip("hola
mundo", 10), "hola");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(clip("abc", 5), "abc");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(format_duration(Duration::from_secs(42)), "42 s");
        assert_eq!(format_duration(Duration::from_secs(185)), "3 min 05 s");
        assert_eq!(format_duration(Duration::from_secs(3720)), "1 h 02 min");
    }
}
