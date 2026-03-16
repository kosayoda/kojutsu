use keymap::Config;
use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::app::App;
use crate::keymap::AppAction;
use crate::repo::JjRepo;

/// Result of handling an input event.
pub enum Action {
    /// Quit the application.
    Quit,
    /// No action needed (already handled by mutating App).
    None,
}

/// Handle a key press by looking it up in the keymap and dispatching.
pub fn handle_key(app: &mut App, jj: &JjRepo, keys: &Config<AppAction>, key: KeyEvent) -> Action {
    let Some(action) = keys.get(&key) else {
        return Action::None;
    };

    match action {
        AppAction::Quit => Action::Quit,
        AppAction::MoveDown => {
            app.move_down();
            Action::None
        }
        AppAction::MoveUp => {
            app.move_up();
            Action::None
        }
        AppAction::MoveDownSection => {
            app.move_down_section();
            Action::None
        }
        AppAction::MoveUpSection => {
            app.move_up_section();
            Action::None
        }
        AppAction::PageDown => {
            app.page_down(15);
            Action::None
        }
        AppAction::PageUp => {
            app.page_up(15);
            Action::None
        }
        AppAction::JumpToWorkingCopy => {
            app.jump_to_working_copy();
            Action::None
        }
        AppAction::ToggleFold => {
            app.toggle_fold(jj);
            Action::None
        }
        AppAction::Refresh => {
            let revset = app.revset.clone();
            app.refresh(jj, &revset);
            Action::None
        }
    }
}

/// Handle a mouse event.
pub fn handle_mouse(app: &mut App, jj: &JjRepo, mouse: MouseEvent, list_offset: u16) -> Action {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
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
