mod action;
mod bookmark;
mod list;
mod modal;
mod view;

pub use action::{dispatch_action_after_hooks, has_conflict_context, has_file_context};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::{App, AppMode};
use crate::keymap;

/// Number of rows to jump for page-up/page-down style navigation.
const PAGE_SIZE: usize = 15;

/// Result of handling an input event.
pub enum Action {
    /// Quit the application.
    Quit,
    /// No action needed (already handled by mutating App).
    None,
    /// Run a jj CLI command (captured output), then refresh the DAG.
    RunJj(crate::jj_command::JJCommand),
    /// Suspend the TUI, run an interactive jj command, then resume.
    SuspendAndRunJj(crate::jj_command::JJCommand),
    /// Snapshot the working copy and reload the DAG.
    Refresh,
    /// Evaluate a new revset and refresh the view.
    UpdateRevset(String),
    /// Suspend TUI and open $EDITOR to edit the revset.
    EditRevsetInEditor,
    /// Suspend TUI and open $EDITOR on a working copy file at a line.
    EditWorkingCopyFile { path: String, line: usize },
    /// Suspend TUI, export file at a revision to a temp file, open in $EDITOR.
    EditFileAtRevision {
        commit_id: crate::types::CommitId,
        path: crate::types::RepoPath,
        line: usize,
    },
    /// Pre-hooks completed after yielding; dispatch the deferred action.
    DeferredDispatch {
        action: crate::keymap::AppAction,
        flags: crate::keymap::CommandFlags,
    },
    /// Run `jj new <commit>`, then open $EDITOR on the file, then refresh.
    CheckoutAndEdit {
        commit_id: crate::types::CommitId,
        path: String,
        line: usize,
    },
}

/// Handle a key press, dispatching through the keymap trie and app mode.
pub fn handle_key(
    app: &mut App,
    keymaps: &crate::keymap::Keymaps,
    lua: &crate::lua::LuaEngine,
    key: KeyEvent,
) -> Action {
    let Some(node) = keymap::key_event_to_node(&key) else {
        return Action::None;
    };
    let keymap = keymaps.for_view(app.active_view);

    let registry = &keymaps.registry;

    match &app.mode {
        AppMode::Normal => action::handle_normal_key(app, registry, lua, keymap, &node),
        AppMode::Submenu {
            children, flags, ..
        } => {
            let children = children.clone();
            let flags = *flags;
            action::handle_submenu_key(app, registry, lua, &children, flags, &node)
        }
        AppMode::CommandOutput { .. } => {
            let retry = app.mode.take_command_retry();
            if node.key == keymap_parser::Key::Esc {
                Action::None
            } else if !retry.is_empty() {
                app.mode = AppMode::FollowUp {
                    prompt: "Retry?".into(),
                    options: retry,
                };
                Action::None
            } else {
                action::handle_normal_key(app, registry, lua, keymap, &node)
            }
        }
        AppMode::Help { .. } => {
            use keymap_parser::Key;
            match node.key {
                Key::Char('j') | Key::Down => {
                    if let AppMode::Help { scroll } = &mut app.mode {
                        *scroll = scroll.saturating_add(1);
                    }
                    Action::None
                }
                Key::Char('k') | Key::Up => {
                    if let AppMode::Help { scroll } = &mut app.mode {
                        *scroll = scroll.saturating_sub(1);
                    }
                    Action::None
                }
                // Dismiss without forwarding.
                Key::Esc | Key::Char('q') | Key::Char('?') => {
                    app.exit_overlay();
                    Action::None
                }
                // Dismiss and forward printable keys to the underlying mode.
                Key::Char(_) => {
                    app.exit_overlay();
                    match &app.mode {
                        AppMode::Normal => {
                            action::handle_normal_key(app, registry, lua, keymap, &node)
                        }
                        AppMode::TargetSelect { .. } => modal::handle_target_select(app, key),
                        AppMode::CommitSelect { .. } => modal::handle_commit_select(app, key),
                        _ => Action::None,
                    }
                }
                // Ignore everything else (function keys, modifier combos,
                // spurious escape sequences from terminal resize, etc.).
                _ => Action::None,
            }
        }
        AppMode::TextInput { .. } => modal::handle_text_input(app, lua, key),
        AppMode::SearchInput => modal::handle_search_input(app, key),
        AppMode::TargetSelect { .. } => modal::handle_target_select(app, key),
        AppMode::CommitSelect { .. } => modal::handle_commit_select(app, key),
        AppMode::FollowUp { .. } => modal::handle_follow_up(app, key),
        AppMode::SelectFromList(_) => list::handle_select_from_list(app, lua, key),
        AppMode::Jump { .. } => modal::handle_jump(app, key),
    }
}

/// Navigate to a screen position (shared by mouse handlers).
fn mouse_select_row(app: &mut App, mouse: &MouseEvent, list_offset: u16) {
    let screen_line = (mouse.row.saturating_sub(list_offset)) as usize;
    let row = app.row_at_screen_line(screen_line);
    app.select_row(row);
}

/// Handle a mouse event.
pub fn handle_mouse(app: &mut App, mouse: MouseEvent, list_offset: u16) -> Action {
    match &app.mode {
        // Modes where mouse interaction in the DAG list makes sense.
        AppMode::TargetSelect { .. } | AppMode::CommitSelect { .. } => {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    mouse_select_row(app, &mouse, list_offset);
                    Action::None
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    // Select and confirm.
                    mouse_select_row(app, &mouse, list_offset);
                    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                    match &app.mode {
                        AppMode::TargetSelect { .. } => modal::handle_target_select(app, enter),
                        AppMode::CommitSelect { .. } => modal::handle_commit_select(app, enter),
                        _ => Action::None,
                    }
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
        // Dismiss command output only on a deliberate click, not mouse movement.
        AppMode::CommandOutput { .. } => {
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                app.mode = AppMode::Normal;
            }
            Action::None
        }
        // Ignore mouse in modal input modes (text input, search, follow-up, list).
        AppMode::TextInput { .. }
        | AppMode::SearchInput
        | AppMode::FollowUp { .. }
        | AppMode::SelectFromList(_) => Action::None,
        // Normal, Submenu, Help: standard DAG navigation.
        _ => match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                app.mode = AppMode::Normal;
                mouse_select_row(app, &mouse, list_offset);
                Action::None
            }
            MouseEventKind::Down(MouseButton::Right) => {
                app.mode = AppMode::Normal;
                mouse_select_row(app, &mouse, list_offset);
                app.toggle_fold();
                Action::None
            }
            MouseEventKind::ScrollUp => {
                app.mode = AppMode::Normal;
                app.move_up();
                Action::None
            }
            MouseEventKind::ScrollDown => {
                app.mode = AppMode::Normal;
                app.move_down();
                Action::None
            }
            _ => Action::None,
        },
    }
}
