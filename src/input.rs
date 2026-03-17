use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::app::{App, AppMode};
use crate::keymap::{self, AppAction, Keymap, LookupResult};
use crate::repo::JjRepo;

/// Result of handling an input event.
pub enum Action {
    /// Quit the application.
    Quit,
    /// No action needed (already handled by mutating App).
    None,
    /// Run a jj CLI command, then refresh the DAG.
    RunJj {
        args: Vec<String>,
        display_cmd: String,
    },
}

/// Handle a key press, dispatching through the keymap trie and app mode.
pub fn handle_key(app: &mut App, jj: &JjRepo, keymap: &'static Keymap, key: KeyEvent) -> Action {
    let Some(node) = keymap::key_event_to_node(&key) else {
        return Action::None;
    };

    match &app.mode {
        AppMode::Normal => handle_normal_key(app, jj, keymap, &node),
        AppMode::Submenu { children, .. } => {
            // Copy the static reference before mutating app.mode.
            let children = *children;
            handle_submenu_key(app, jj, children, &node)
        }
        AppMode::CommandOutput { .. } => {
            // Any keypress dismisses the command output.
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
    // Esc always cancels the submenu.
    if node.key == keymap_parser::Key::Esc {
        app.mode = AppMode::Normal;
        return Action::None;
    }

    let result = Keymap::lookup_in(children, node);
    // Any key press exits the submenu, whether it matched or not.
    app.mode = AppMode::Normal;

    match result {
        LookupResult::Action(action) => dispatch_action(app, jj, action),
        LookupResult::Prefix { .. } => {
            // Nested submenus not supported yet.
            Action::None
        }
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
        AppAction::Abandon => abandon_action(app, &[]),
        AppAction::AbandonKeepBookmarks => abandon_action(app, &["--retain-bookmarks"]),
        AppAction::AbandonRestoreDescendants => abandon_action(app, &["--restore-descendants"]),
    }
}

fn abandon_action(app: &App, extra_args: &[&str]) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    let change_id = change_id.to_string();

    let mut args = vec!["abandon".to_string()];
    for arg in extra_args {
        args.push(arg.to_string());
    }
    args.push(change_id.clone());

    let display_cmd = format!("$ jj {}", args.join(" "));
    Action::RunJj { args, display_cmd }
}

/// Handle a mouse event.
pub fn handle_mouse(app: &mut App, jj: &JjRepo, mouse: MouseEvent, list_offset: u16) -> Action {
    // Mouse events cancel any pending submenu.
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
