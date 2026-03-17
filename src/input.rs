use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind,
};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::app::{App, AppMode, PendingCommand};
use crate::jj_command::JJCommand;
use crate::keymap::{self, AppAction, Keymap, LookupResult};
use crate::repo::JjRepo;

/// Result of handling an input event.
pub enum Action {
    /// Quit the application.
    Quit,
    /// No action needed (already handled by mutating App).
    None,
    /// Run a jj CLI command (captured output), then refresh the DAG.
    RunJj(JJCommand),
    /// Suspend the TUI, run an interactive jj command, then resume.
    SuspendAndRunJj(JJCommand),
    /// Snapshot the working copy and reload the DAG.
    Refresh,
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
        AppMode::TextInput { .. } => handle_text_input(app, key),
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
        AppAction::Refresh => Action::Refresh,
        AppAction::Abandon => make_abandon_command(app, false, false),
        AppAction::AbandonKeepBookmarks => make_abandon_command(app, true, false),
        AppAction::AbandonRestoreDescendants => make_abandon_command(app, false, true),
        AppAction::Describe => enter_describe_input(app, false),
        AppAction::DescribeIgnoreImmutable => enter_describe_input(app, true),
        AppAction::DescribeInEditor => make_describe_editor_command(app, false),
        AppAction::DescribeInEditorIgnoreImmutable => make_describe_editor_command(app, true),
        AppAction::Edit => make_edit_command(app, false),
        AppAction::EditIgnoreImmutable => make_edit_command(app, true),
        AppAction::New => make_new_command(app, false, false, false),
        AppAction::NewInsertAfter => make_new_command(app, true, false, false),
        AppAction::NewInsertBefore => make_new_command(app, false, true, false),
        AppAction::NewNoEdit => make_new_command(app, false, false, true),
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

fn make_edit_command(app: &App, ignore_immutable: bool) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    Action::RunJj(JJCommand::Edit {
        change_id: change_id.to_string(),
        ignore_immutable,
    })
}

fn make_new_command(app: &App, insert_after: bool, insert_before: bool, no_edit: bool) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    Action::RunJj(JJCommand::New {
        change_id: change_id.to_string(),
        insert_after,
        insert_before,
        no_edit,
    })
}

fn enter_describe_input(app: &mut App, ignore_immutable: bool) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    let current_desc = app.selected_description().unwrap_or("").to_string();
    let change_id = change_id.to_string();

    app.mode = AppMode::TextInput {
        prompt: "describe: ".to_string(),
        input: Input::new(current_desc),
        on_submit: PendingCommand::Describe {
            change_id,
            ignore_immutable,
        },
    };
    Action::None
}

fn make_describe_editor_command(app: &App, ignore_immutable: bool) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    Action::SuspendAndRunJj(JJCommand::DescribeInEditor {
        change_id: change_id.to_string(),
        ignore_immutable,
    })
}

fn handle_text_input(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        // Enter: submit
        KeyCode::Enter => {
            // Take ownership of the TextInput fields.
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::TextInput {
                input, on_submit, ..
            } = mode
            {
                let message = input.to_string();
                let cmd = on_submit.into_jj_command(message);
                Action::RunJj(cmd)
            } else {
                Action::None
            }
        }
        // Esc: cancel
        KeyCode::Esc => {
            app.mode = AppMode::Normal;
            Action::None
        }
        // Everything else: forward to tui-input
        _ => {
            if let AppMode::TextInput { input, .. } = &mut app.mode {
                input.handle_event(&Event::Key(key));
            }
            Action::None
        }
    }
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
