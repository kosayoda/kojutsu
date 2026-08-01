mod action;
mod bookmark;
mod list;
mod modal;
mod view;

pub use view::dag::{complete_hunk_edit, staged_resolution};

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

/// The character a key types, if it is text rather than a command. Modes that
/// consume text have to check this before resolving anything through the
/// keymap: a movement bound to a letter must still type that letter here.
fn typed_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => Some(c),
        _ => None,
    }
}

/// How far an overlay should scroll for `node`, read from the keymap so the
/// movement keys are whichever ones the user bound. Overlays have no sections
/// and no absolute extent to move to, so only the four relative movements
/// apply.
fn scroll_delta(keymap: &crate::keymap::Keymap, node: &keymap_parser::Node) -> Option<i16> {
    use crate::keymap::AppAction;
    match keymap.builtin_action(node)? {
        AppAction::MoveDown => Some(1),
        AppAction::MoveUp => Some(-1),
        AppAction::PageDown => Some(PAGE_SIZE as i16),
        AppAction::PageUp => Some(-(PAGE_SIZE as i16)),
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
    /// Suspend TUI, edit a whole conflicted file's assembled resolution
    /// (picks applied, unpicked hunks as markers) in $EDITOR; apply via
    /// `jj resolve` if changed. Whole-file counterpart to `EditConflictHunk`.
    EditConflictFile {
        change_id: crate::types::ChangeId,
        path: crate::types::RepoPath,
        content: String,
        flags: crate::keymap::CommandFlags,
    },
    /// Suspend TUI, edit one conflict hunk's resolution in $EDITOR; store
    /// it as the hunk's pick (pure UI state until picks are applied).
    /// Addressed by stable IDs — row indices must not cross a suspend.
    EditConflictHunk {
        commit_id: crate::types::CommitId,
        path: crate::types::RepoPath,
        hunk_idx: crate::idx::ConflictHunkIdx,
        seed: String,
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
            action::handle_submenu_key(app, registry, lua, keymap, &children, flags, &node)
        }
        AppMode::CommandOutput(state) => {
            // When the output overflows, scroll keys scroll without
            // dismissing; when it fits, they dismiss like any other key.
            let overlay_base = app.last_list_height + crate::ui::STATUS_AREA_HEIGHT;
            if let (Some(delta), 1..) =
                (scroll_delta(keymap, &node), state.max_scroll(overlay_base))
            {
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
            if let Some(delta) = scroll_delta(keymap, &node) {
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
                        AppMode::TargetSelect { .. } | AppMode::CommitSelect { .. } => {
                            select_key(app, registry, lua, keymap, key, &node)
                        }
                        _ => Action::None,
                    }
                }
                // Ignore everything else (function keys, modifier combos,
                // spurious escape sequences from terminal resize, etc.).
                _ => Action::None,
            }
        }
        AppMode::TextInput { .. } => modal::handle_text_input(app, lua, key),
        AppMode::SearchInput => modal::handle_search_input(app, keymap, key),
        AppMode::TargetSelect { .. } | AppMode::CommitSelect { .. } => {
            select_key(app, registry, lua, keymap, key, &node)
        }
        AppMode::FollowUp { .. } => modal::handle_follow_up(app, key),
        AppMode::SelectFromList(_) => list::handle_select_from_list(app, lua, keymap, key),
        AppMode::Jump(_) => modal::handle_jump(app, key),
        AppMode::CommandRunning(state) => {
            use keymap_parser::Key;
            let ctrl = (node.modifiers & keymap_parser::Modifier::Ctrl as u8) != 0;
            let shift = (node.modifiers & keymap_parser::Modifier::Shift as u8) != 0;
            if let Some(delta) = scroll_delta(keymap, &node) {
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

/// Dispatch a key in target- or commit-select. The mode gets first refusal on
/// its own keys; anything it declines resolves through the keymap, which is
/// what keeps movement bindings (and any rebinding of them) working here.
fn select_key(
    app: &mut App,
    registry: &crate::keymap::ActionRegistry,
    lua: &crate::lua::LuaEngine,
    keymap: &crate::keymap::Keymap,
    key: KeyEvent,
    node: &keymap_parser::Node,
) -> Action {
    let owned = if matches!(app.mode, AppMode::TargetSelect { .. }) {
        modal::handle_target_select(app, key)
    } else {
        modal::handle_commit_select(app, key)
    };
    match owned {
        Some(action) => action,
        None => action::handle_select_navigation(app, registry, lua, keymap, node),
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
                    // Enter is a key both modes own, so neither declines it
                    // and no keymap fallback is needed here.
                    let confirmed = match &app.mode {
                        AppMode::TargetSelect { .. } => modal::handle_target_select(app, enter),
                        AppMode::CommitSelect { .. } => modal::handle_commit_select(app, enter),
                        _ => None,
                    };
                    confirmed.unwrap_or(Action::None)
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

#[cfg(test)]
mod scroll_delta_tests {
    use super::{PAGE_SIZE, scroll_delta};
    use crate::app::ActiveView;
    use crate::keymap::{ActionRegistry, AppAction, Keymaps, default_bindings, try_parse_key};

    fn delta(keymaps: &Keymaps, key: &str) -> Option<i16> {
        let node = try_parse_key(key).expect("parsable key");
        scroll_delta(keymaps.for_view(ActiveView::Dag), &node)
    }

    #[test]
    fn overlays_scroll_on_the_default_movement_keys() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        assert_eq!(delta(&keymaps, "j"), Some(1));
        assert_eq!(delta(&keymaps, "down"), Some(1));
        assert_eq!(delta(&keymaps, "k"), Some(-1));
        assert_eq!(delta(&keymaps, "up"), Some(-1));
        assert_eq!(delta(&keymaps, "ctrl-d"), Some(PAGE_SIZE as i16));
        assert_eq!(delta(&keymaps, "pagedown"), Some(PAGE_SIZE as i16));
        assert_eq!(delta(&keymaps, "ctrl-u"), Some(-(PAGE_SIZE as i16)));
        assert_eq!(delta(&keymaps, "pageup"), Some(-(PAGE_SIZE as i16)));
    }

    /// An overlay has no sections and no extent, so movements that mean
    /// nothing there fall through to the mode's own handling.
    #[test]
    fn other_keys_do_not_scroll() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        for key in ["shift-j", "0", "$", "q", "esc", "x"] {
            assert_eq!(delta(&keymaps, key), None, "{key} should not scroll");
        }
    }

    /// The point of reading the keymap: a rebound movement key scrolls, and
    /// the default it replaced no longer does.
    #[test]
    fn a_rebound_movement_key_scrolls() {
        use crate::keymap::{ActionId, BindTarget, BindingSpec, HelpGroup, Scope};

        let j = try_parse_key("j").unwrap();
        let mut specs: Vec<BindingSpec> = default_bindings()
            .into_iter()
            .filter(|s| s.keys.as_slice() != [j.clone()])
            .collect();
        specs.push(BindingSpec {
            keys: smallvec::smallvec![try_parse_key(",").unwrap()],
            target: BindTarget::Action {
                id: ActionId::Builtin(AppAction::MoveDown),
                description: "move down".into(),
                group: HelpGroup::Navigation,
            },
            scope: Scope::All,
        });
        let keymaps = Keymaps::build(specs, ActionRegistry::new());

        assert_eq!(delta(&keymaps, ","), Some(1));
        assert_eq!(delta(&keymaps, "j"), None);
    }
}
