mod action;
mod bookmark;
mod list;
mod modal;
mod view;

pub use view::dag::{maybe_offer_apply, resolution_command};

pub use action::{dispatch_action_after_hooks, has_conflict_context, has_file_context};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::{App, AppMode};
use crate::keymap;

/// Number of rows to jump for page-up/page-down style navigation.
const PAGE_SIZE: usize = 15;

/// Lines scrolled per mouse-wheel tick in overlay panes.
const WHEEL_SCROLL_LINES: i16 = 3;

/// Map scroll keys to a line delta for overlay panes (positive = down).
fn scroll_delta(node: &keymap_parser::Node) -> Option<i16> {
    use keymap_parser::Key;
    let ctrl = (node.modifiers & keymap_parser::Modifier::Ctrl as u8) != 0;
    match node.key {
        Key::Char('j') | Key::Down => Some(1),
        Key::Char('k') | Key::Up => Some(-1),
        Key::Char('d') if ctrl => Some(PAGE_SIZE as i16),
        Key::Char('u') if ctrl => Some(-(PAGE_SIZE as i16)),
        _ => None,
    }
}

/// Result of handling an input event.
pub enum Action {
    /// Quit the application.
    Quit,
    /// No action needed (already handled by mutating App).
    None,
    /// Run a jj CLI command (captured output), then refresh the DAG.
    RunJj(crate::jj_command::JJCommand),
    /// Run a jj CLI command yielded by a suspended Lua thread; completion
    /// resumes the thread with the result instead of refreshing.
    RunJjForLua(crate::jj_command::JJCommand),
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
    /// Suspend TUI, edit an assembled conflict resolution (picks applied,
    /// unpicked hunks as markers) in $EDITOR; apply via `jj resolve` if
    /// changed.
    EditConflictResolution {
        change_id: crate::types::ChangeId,
        path: crate::types::RepoPath,
        content: String,
        flags: crate::keymap::CommandFlags,
    },
    /// Suspend TUI, edit one conflict hunk's resolution in $EDITOR; store
    /// it as the hunk's pick (pure UI state until picks are applied).
    EditConflictHunk {
        hunk: crate::types::ConflictHunkRef,
        seed: String,
        path: crate::types::RepoPath,
        flags: crate::keymap::CommandFlags,
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

    match &mut app.mode {
        AppMode::Normal => action::handle_normal_key(app, registry, lua, keymap, &node),
        AppMode::Submenu {
            children, flags, ..
        } => {
            let children = children.clone();
            let flags = *flags;
            action::handle_submenu_key(app, registry, lua, &children, flags, &node)
        }
        AppMode::CommandOutput(state) => {
            // When the output overflows, scroll keys scroll without
            // dismissing; when it fits, they dismiss like any other key.
            let overlay_base = app.last_list_height + crate::ui::STATUS_AREA_HEIGHT;
            if let (Some(delta), 1..) = (scroll_delta(&node), state.max_scroll(overlay_base)) {
                state.scroll = state.scroll.saturating_add_signed(delta);
                return Action::None;
            }
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
        AppMode::Help { scroll } => {
            use keymap_parser::Key;
            if let Some(delta) = scroll_delta(&node) {
                *scroll = scroll.saturating_add_signed(delta);
                return Action::None;
            }
            match node.key {
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
        AppMode::Jump(_) => modal::handle_jump(app, key),
        AppMode::CommandRunning(state) => {
            use keymap_parser::Key;
            let ctrl = (node.modifiers & keymap_parser::Modifier::Ctrl as u8) != 0;
            let shift = (node.modifiers & keymap_parser::Modifier::Shift as u8) != 0;
            if let Some(delta) = scroll_delta(&node) {
                // scroll_from_bottom counts up toward older lines, so the
                // delta is inverted.
                state.scroll_from_bottom = state
                    .scroll_from_bottom
                    .saturating_add_signed(-delta as isize);
                return Action::None;
            }
            match node.key {
                Key::Esc => state.kill.kill(),
                Key::Char('c') if ctrl => state.kill.kill(),
                // Jump back to the live tail (G is delivered as shift+g).
                Key::Char('g') if shift => state.scroll_from_bottom = 0,
                Key::Char('$') => state.scroll_from_bottom = 0,
                _ => {}
            }
            Action::None
        }
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
    match &mut app.mode {
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
        // Dismiss command output only on a deliberate click, not mouse
        // movement; the wheel scrolls the output.
        AppMode::CommandOutput(state) => {
            match mouse.kind {
                MouseEventKind::Down(_) => {
                    app.mode = AppMode::Normal;
                }
                MouseEventKind::ScrollDown => {
                    state.scroll = state.scroll.saturating_add_signed(WHEEL_SCROLL_LINES);
                }
                MouseEventKind::ScrollUp => {
                    state.scroll = state.scroll.saturating_add_signed(-WHEEL_SCROLL_LINES);
                }
                _ => {}
            }
            Action::None
        }
        // The wheel scrolls the live output of a running command; other
        // mouse events are ignored (Esc/^C cancels, clicks don't dismiss).
        AppMode::CommandRunning(state) => {
            match mouse.kind {
                MouseEventKind::ScrollDown => {
                    state.scroll_from_bottom = state
                        .scroll_from_bottom
                        .saturating_sub(WHEEL_SCROLL_LINES as usize);
                }
                MouseEventKind::ScrollUp => {
                    state.scroll_from_bottom = state
                        .scroll_from_bottom
                        .saturating_add(WHEEL_SCROLL_LINES as usize);
                }
                _ => {}
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
