use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::app::{App, AppMode};
use crate::jj_command::JJCommand;
use crate::keymap::{self, AppAction, Keymap, LookupResult};
use crate::repo::JjRepo;

/// Result of handling an input event.
pub enum Action {
    /// Quit the application.
    Quit,
    /// No action needed (already handled by mutating App).
    None,
    /// Run a jj CLI command, then refresh the DAG.
    RunJj(JJCommand),
}

/// Handle a key press, dispatching through the keymap trie and app mode.
pub fn handle_key(app: &mut App, jj: &JjRepo, keymap: &'static Keymap, key: KeyEvent) -> Action {
    let Some(node) = keymap::key_event_to_node(&key) else {
        return Action::None;
    };

    match &app.mode {
        AppMode::Normal => handle_normal_key(app, jj, keymap, &node),
        AppMode::Submenu { children, .. } => {
            let children = *children;
            handle_submenu_key(app, jj, children, &node)
        }
        AppMode::CommandOutput { .. } => {
            app.mode = AppMode::Normal;
            Action::None
        }
    }
}

fn handle_normal_key(
    app: &mut App,
    jj: &JjRepo,
    keymap: &'static Keymap,
    node: &keymap_parser::Node,
) -> Action {
    match keymap.lookup(node) {
        LookupResult::Action(action) => dispatch_action(app, jj, action),
        LookupResult::Prefix { label, children } => {
            app.mode = AppMode::Submenu { label, children };
            Action::None
        }
        LookupResult::Unbound => Action::None,
    }
}

fn handle_submenu_key(
    app: &mut App,
    jj: &JjRepo,
    children: &'static [(keymap_parser::Node, keymap::KeymapNode)],
    node: &keymap_parser::Node,
) -> Action {
    if node.key == keymap_parser::Key::Esc {
        app.mode = AppMode::Normal;
        return Action::None;
    }

    let result = Keymap::lookup_in(children, node);
    app.mode = AppMode::Normal;

    match result {
        LookupResult::Action(action) => dispatch_action(app, jj, action),
        LookupResult::Prefix { .. } => Action::None,
        LookupResult::Unbound => Action::None,
    }
}

fn dispatch_action(app: &mut App, jj: &JjRepo, action: AppAction) -> Action {
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
        AppAction::Abandon => make_abandon_command(app, false, false),
        AppAction::AbandonKeepBookmarks => make_abandon_command(app, true, false),
        AppAction::AbandonRestoreDescendants => make_abandon_command(app, false, true),
    }
}

fn make_abandon_command(app: &App, retain_bookmarks: bool, restore_descendants: bool) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    Action::RunJj(JJCommand::Abandon {
        change_id: change_id.to_string(),
        retain_bookmarks,
        restore_descendants,
    })
}

/// Handle a mouse event.
pub fn handle_mouse(app: &mut App, jj: &JjRepo, mouse: MouseEvent, list_offset: u16) -> Action {
    app.mode = AppMode::Normal;

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
