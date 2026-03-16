use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::App;
use crate::repo::JjRepo;

/// Result of handling an input event.
pub enum Action {
    /// Quit the application.
    Quit,
    /// No action needed (already handled by mutating App).
    None,
}

/// Handle a key press, updating app state and returning any top-level action.
pub fn handle_key(app: &mut App, jj: &JjRepo, key: KeyEvent) -> Action {
    match key.code {
        // Quit
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,

        // Navigation
        KeyCode::Char('j') | KeyCode::Down => {
            app.move_down();
            Action::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.move_up();
            Action::None
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.page_down(15);
            Action::None
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.page_up(15);
            Action::None
        }
        KeyCode::PageDown => {
            app.page_down(15);
            Action::None
        }
        KeyCode::PageUp => {
            app.page_up(15);
            Action::None
        }

        // Jump to working copy
        KeyCode::Char('@') => {
            app.jump_to_working_copy();
            Action::None
        }

        // Fold/unfold
        KeyCode::Tab => {
            app.toggle_fold(jj);
            Action::None
        }

        // Refresh
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let revset = app.revset.clone();
            app.refresh(jj, &revset);
            Action::None
        }

        _ => Action::None,
    }
}

/// Handle a mouse event. Returns the viewport offset of the list widget
/// so click positions can be translated to row indices.
pub fn handle_mouse(app: &mut App, jj: &JjRepo, mouse: MouseEvent, list_offset: u16) -> Action {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Translate screen row to list row index.
            let row = (mouse.row.saturating_sub(list_offset)) as usize + app.scroll_offset();
            app.select_row(row);
            Action::None
        }
        MouseEventKind::Down(MouseButton::Right) => {
            let row = (mouse.row.saturating_sub(list_offset)) as usize + app.scroll_offset();
            app.select_row(row);
            app.toggle_fold(jj);
            Action::None
        }
        MouseEventKind::ScrollUp => {
            app.move_up();
            Action::None
        }
        MouseEventKind::ScrollDown => {
            app.move_down();
            Action::None
        }
        _ => Action::None,
    }
}
