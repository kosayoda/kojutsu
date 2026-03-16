use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::repo::JjRepo;

/// Result of handling a key event.
pub enum Action {
    /// Quit the application.
    Quit,
    /// No action needed (already handled by mutating App).
    None,
}

/// Handle a key press, updating app state and returning any top-level action.
pub fn handle_key(app: &mut App, jj: &JjRepo, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => {
            app.move_down();
            Action::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.move_up();
            Action::None
        }
        KeyCode::Tab => {
            app.toggle_fold(jj);
            Action::None
        }
        _ => Action::None,
    }
}
