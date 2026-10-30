mod action;
mod bookmark;
mod list;
mod modal;
mod target;
mod view;

pub use view::dag::{complete_hunk_edit, staged_resolution};

pub use action::{availability, dispatch_action_after_hooks, no_toggles};
pub use modal::apply_completions;

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

/// What happens once a jj command finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    /// Refresh, then run the post-hooks of the action whose flow started
    /// the command, if one did.
    Refresh { hook: Option<keymap::AppAction> },
    /// Resume the suspended Lua thread that yielded it with its result.
    ResumeLua,
}

/// Result of handling an input event.
pub enum Action {
    /// Quit the application.
    Quit,
    /// No action needed (already handled by mutating App).
    None,
    /// Ask jj off the UI thread what can complete the command prompt's
    /// `input`; the answer goes to [`apply_completions`].
    CompleteCommand { input: String },
    /// Run a jj CLI command. Its `terminal_use` decides whether the TUI
    /// steps aside for it; `completion` what happens once it finishes.
    RunJj {
        cmd: crate::jj_command::JJCommand,
        completion: Completion,
    },
    /// Snapshot the working copy and reload the DAG.
    Refresh,
    /// Re-read `init.lua` and swap in the runtime it produces.
    ReloadConfig,
    /// Evaluate a new revset and refresh the view.
    UpdateRevset(String),
    /// Suspend TUI and open $EDITOR to edit the revset.
    EditRevsetInEditor,
    /// Suspend TUI and open $EDITOR on a working copy file at a line.
    EditWorkingCopyFile { path: String, line: usize },
    /// Suspend TUI, edit a whole conflicted file's assembled resolution
    /// (picks applied, unpicked hunks as markers) in $EDITOR; apply via
    /// `jj resolve` if changed. Whole-file counterpart to `EditConflictHunk`.
    EditConflictFile {
        change_id: crate::types::RevisionArg,
        path: crate::types::RepoPath,
        content: bstr::BString,
        flags: crate::keymap::CommandFlags,
    },
    /// Suspend TUI, edit one conflict hunk's resolution in $EDITOR; store
    /// it as the hunk's pick (pure UI state until picks are applied).
    /// Addressed by stable IDs: row indices must not cross a suspend.
    EditConflictHunk {
        commit_id: crate::types::CommitId,
        path: crate::types::RepoPath,
        hunk_idx: crate::idx::ConflictHunkIdx,
        seed: bstr::BString,
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

impl Action {
    /// Run `cmd` and refresh, attributed to no action yet.
    pub fn run(cmd: crate::jj_command::JJCommand) -> Self {
        Self::RunJj {
            cmd,
            completion: Completion::Refresh { hook: None },
        }
    }

    /// Attribute a command not yet attributed to `origin`, the action whose
    /// flow produced it, so that action's post-hooks run after it.
    pub fn attributed_to(self, origin: Option<keymap::AppAction>) -> Self {
        match self {
            Self::RunJj {
                cmd,
                completion: Completion::Refresh { hook: None },
            } => Self::RunJj {
                cmd,
                completion: Completion::Refresh { hook: origin },
            },
            other => other,
        }
    }
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
    // A key in a prompt continues the chain an action started: whatever
    // prompt or command it leads to belongs to that action too.
    let origin = app.mode.origin().cloned();
    let action = app.as_user_input(|app| handle_key_in_mode(app, keymaps, lua, key, &node));
    if let Some(origin) = &origin {
        app.mode.adopt_origin(origin);
    }
    action.attributed_to(origin.map(|o| o.action))
}

fn handle_key_in_mode(
    app: &mut App,
    keymaps: &crate::keymap::Keymaps,
    lua: &crate::lua::LuaEngine,
    key: KeyEvent,
    node: &keymap_parser::Node,
) -> Action {
    let keymap = keymaps.for_view(app.active_view);
    let registry = &keymaps.registry;
    let node = node.clone();

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
            if node.key == keymap_parser::Key::Esc {
                app.mode = AppMode::Normal;
                Action::None
            } else if app.mode.open_command_retry() {
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

/// Put the cursor on the row under the mouse, if it is over one: a click on
/// the header, the status bar or empty space picks nothing. Returns whether
/// it picked a row, so a click's follow-up (fold, confirm) acts only then.
fn mouse_select_row(app: &mut App, mouse: &MouseEvent, list_offset: u16) -> bool {
    let Some(screen_line) = mouse.row.checked_sub(list_offset) else {
        return false;
    };
    match app.row_at_screen_line(screen_line as usize) {
        Some(row) => {
            app.select_row(row);
            true
        }
        None => false,
    }
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
                    // Select and confirm, only on a row.
                    if !mouse_select_row(app, &mouse, list_offset) {
                        return Action::None;
                    }
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
                if mouse_select_row(app, &mouse, list_offset) {
                    app.toggle_fold();
                }
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
    use crate::keymap::{ActionRegistry, AppAction, Keymaps, default_bindings, try_parse_key};
    use crate::types::ActiveView;

    fn delta(keymaps: &Keymaps, key: &str) -> Option<i16> {
        let node = try_parse_key(key).expect("parsable key");
        scroll_delta(keymaps.for_view(ActiveView::Dag), &node)
    }

    /// Overlays scroll on whatever keys the movements are bound to. One
    /// that has no sections and no extent leaves other movements, and other
    /// keys, to the mode's own handling.
    #[test]
    fn overlays_scroll_on_the_movement_keys_and_nothing_else() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let page = PAGE_SIZE as i16;
        for (key, scrolls) in [
            ("j", Some(1)),
            ("k", Some(-1)),
            ("ctrl-d", Some(page)),
            ("ctrl-u", Some(-page)),
            ("shift-j", None),
            ("$", None),
            ("q", None),
        ] {
            assert_eq!(delta(&keymaps, key), scrolls, "{key}");
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

#[cfg(test)]
mod hook_attribution_tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{Action, Completion, handle_key};
    use crate::app::{App, AppMode, Invocation};
    use crate::jj_command::{JJCommand, JJCommandKind};
    use crate::keymap::{ActionRegistry, AppAction, CommandFlags, Keymaps, default_bindings};
    use crate::lua::LuaEngine;
    use crate::types::{FollowUpAction, FollowUpOption};

    fn press(app: &mut App, code: KeyCode) -> Action {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let lua = LuaEngine::for_test();
        handle_key(app, &keymaps, &lua, KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn hook(action: &Action) -> Option<AppAction> {
        match action {
            Action::RunJj {
                completion: Completion::Refresh { hook },
                ..
            } => *hook,
            _ => panic!("expected a jj command"),
        }
    }

    /// Output of a failed command offering a retry with `r`.
    fn output_offering_retry(app: &mut App, ran_by: Option<AppAction>) {
        app.mode = AppMode::command_output(
            "$ jj undo".into(),
            None,
            Vec::new(),
            false,
            vec![FollowUpOption {
                key: 'r',
                label: "retry",
                action: FollowUpAction::Execute(JJCommand {
                    kind: JJCommandKind::Undo,
                    flags: CommandFlags::empty(),
                }),
            }],
        );
        if let Some(action) = ran_by {
            app.mode.adopt_origin(&Invocation {
                action,
                toggles: super::no_toggles(),
            });
        }
    }

    /// A command reached through a prompt belongs to the action that opened
    /// the prompt: describe's post-hooks run after the describe it submits.
    #[test]
    fn a_prompted_command_runs_the_opening_actions_hooks() {
        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(app.mode, AppMode::TextInput { .. }));

        let submitted = press(&mut app, KeyCode::Enter);
        assert_eq!(hook(&submitted), Some(AppAction::Describe));
    }

    /// A retry offered on a command's output runs that command's hooks.
    #[test]
    fn a_retry_runs_the_original_commands_hooks() {
        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        output_offering_retry(&mut app, Some(AppAction::Abandon));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, AppMode::FollowUp { .. }));

        let retried = press(&mut app, KeyCode::Char('r'));
        assert_eq!(hook(&retried), Some(AppAction::Abandon));
    }

    /// Keys pressed in between belong to nobody's chain: a retry of a
    /// command no action ran is not handed the last key's hooks.
    #[test]
    fn an_unrelated_key_lends_no_hook() {
        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        press(&mut app, KeyCode::Char('j'));
        output_offering_retry(&mut app, None);
        press(&mut app, KeyCode::Enter);

        let retried = press(&mut app, KeyCode::Char('r'));
        assert_eq!(hook(&retried), None);
    }
}

#[cfg(test)]
mod mouse_tests {
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    use super::handle_mouse;
    use crate::app::App;
    use crate::idx::{EntryIdx, RowIdx};

    /// One header line, then a ten-line list holding three commits' rows.
    const HEADER: u16 = 1;

    fn app() -> App {
        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        for (change, commit) in [("rlvkpnrz", "8ccbb3dc"), ("zsuskuln", "9ddcc4ed")] {
            app.push_test_commit(crate::dag::CommitInfo::for_test(change, commit));
        }
        app.rebuild_rows();
        app.last_header_height = HEADER;
        app.last_list_height = 10;
        app
    }

    fn click(app: &mut App, button: MouseButton, screen_row: u16) {
        let event = MouseEvent {
            kind: MouseEventKind::Down(button),
            column: 5,
            row: screen_row,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(app, event, HEADER);
    }

    /// A click on a row puts the cursor there; the header, the status bar
    /// and the list's empty space hold no row, so a click on them leaves the
    /// cursor where it was.
    #[test]
    fn a_click_selects_the_row_under_it_and_off_the_rows_picks_nothing() {
        let mut clicked = app();
        let last = RowIdx::new(clicked.rows.len() - 1);
        clicked.cursor = last;
        click(&mut clicked, MouseButton::Left, HEADER);
        assert_eq!(clicked.cursor, RowIdx::new(0));

        for screen_row in [0, HEADER + 5, HEADER + 10, HEADER + 11] {
            let mut missed = app();
            // Off row 0, where a click above the list would land if it
            // counted as the first row.
            missed.cursor = last;
            click(&mut missed, MouseButton::Left, screen_row);
            assert_eq!(missed.cursor, last, "screen row {screen_row}");
        }
    }

    /// A right click folds what it is on; on the status bar, nothing.
    #[test]
    fn a_right_click_off_the_rows_folds_nothing() {
        let mut app = app();
        click(&mut app, MouseButton::Right, HEADER + 10);
        assert!((0..3).all(|i| !app.is_commit_unfolded(EntryIdx::new(i))));
        click(&mut app, MouseButton::Right, HEADER);
        assert!(app.is_commit_unfolded(EntryIdx::new(0)));
    }
}
