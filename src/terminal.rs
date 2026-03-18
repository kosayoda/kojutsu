use std::io::{self, stdout, Stdout};
use std::panic;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Enter the alternate screen, enable raw mode and mouse capture, and return
/// a [`Terminal`] handle ready for drawing.
pub fn init() -> io::Result<Term> {
    install_panic_hook();
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture, Hide)?;
    Terminal::new(CrosstermBackend::new(stdout()))
}

/// Leave the alternate screen, disable raw mode and mouse capture.
pub fn restore() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture, Show)?;
    Ok(())
}

/// Ensure the terminal is restored even on panic so the user doesn't get
/// stuck in raw mode.
fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = restore();
        original(info);
    }));
}
