use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind,
};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::app::{App, AppMode, MessageMode, PendingCommand, TargetOperation};
use crate::jj_command::JJCommand;
use crate::keymap::{self, AppAction, CommandFlags, Keymap, LookupResult};
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
        AppMode::Submenu {
            children, flags, ..
        } => {
            let children = *children;
            let flags = *flags;
            handle_submenu_key(app, jj, children, flags, &node)
        }
        AppMode::CommandOutput { .. } => {
            app.mode = AppMode::Normal;
            handle_normal_key(app, jj, keymap, &node)
        }
        AppMode::Help => {
            app.mode = AppMode::Normal;
            handle_normal_key(app, jj, keymap, &node)
        }
        AppMode::TextInput { .. } => handle_text_input(app, key),
        AppMode::TargetSelect { .. } => handle_target_select(app, key),
        AppMode::MessageChoice { .. } => handle_message_choice(app, key),
    }
}

fn handle_normal_key(
    app: &mut App,
    jj: &JjRepo,
    keymap: &'static Keymap,
    node: &keymap_parser::Node,
) -> Action {
    match keymap.lookup(node) {
        LookupResult::Action(action) => dispatch_action(app, jj, action, CommandFlags::empty()),
        LookupResult::Prefix { label, children } => {
            app.mode = AppMode::Submenu {
                label,
                children,
                flags: CommandFlags::empty(),
            };
            Action::None
        }
        LookupResult::Toggle(_) => Action::None, // toggles only work inside submenus
        LookupResult::Unbound => Action::None,
    }
}

fn handle_submenu_key(
    app: &mut App,
    jj: &JjRepo,
    children: &'static [(keymap_parser::Node, keymap::KeymapNode)],
    flags: CommandFlags,
    node: &keymap_parser::Node,
) -> Action {
    if node.key == keymap_parser::Key::Esc {
        app.mode = AppMode::Normal;
        return Action::None;
    }

    let result = Keymap::lookup_in(children, node);

    match result {
        LookupResult::Action(action) => {
            app.mode = AppMode::Normal;
            dispatch_action(app, jj, action, flags)
        }
        LookupResult::Toggle(flag) => {
            // Flip the flag, stay in submenu.
            if let AppMode::Submenu { flags, .. } = &mut app.mode {
                flags.toggle(flag);
            }
            Action::None
        }
        LookupResult::Prefix { .. } => {
            app.mode = AppMode::Normal;
            Action::None
        }
        LookupResult::Unbound => {
            app.mode = AppMode::Normal;
            Action::None
        }
    }
}

fn dispatch_action(app: &mut App, jj: &JjRepo, action: AppAction, flags: CommandFlags) -> Action {
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
        AppAction::ShowHelp => {
            app.mode = AppMode::Help;
            Action::None
        }
        AppAction::Abandon => make_command(app, |id| JJCommand::Abandon {
            change_id: id,
            flags,
        }),
        AppAction::Describe => enter_describe_input(app, flags),
        AppAction::DescribeInEditor => make_command(app, |id| JJCommand::DescribeInEditor {
            change_id: id,
            flags,
        }),
        AppAction::Edit => make_command(app, |id| JJCommand::Edit {
            change_id: id,
            flags,
        }),
        AppAction::New => make_command(app, |id| JJCommand::New {
            change_id: id,
            insert_after: false,
            insert_before: false,
            flags,
        }),
        AppAction::NewInsertAfter => make_command(app, |id| JJCommand::New {
            change_id: id,
            insert_after: true,
            insert_before: false,
            flags,
        }),
        AppAction::NewInsertBefore => make_command(app, |id| JJCommand::New {
            change_id: id,
            insert_after: false,
            insert_before: true,
            flags,
        }),
        AppAction::Squash => make_command(app, |id| JJCommand::Squash {
            change_id: id,
            target: None,
            message: MessageMode::Default,
            flags,
        }),
        AppAction::SquashInto => enter_target_select(app, TargetOperation::SquashInto, flags),
        AppAction::SquashOnto => enter_target_select(app, TargetOperation::SquashOnto, flags),
        AppAction::SquashAfter => enter_target_select(app, TargetOperation::SquashAfter, flags),
        AppAction::SquashBefore => enter_target_select(app, TargetOperation::SquashBefore, flags),
        AppAction::Undo => make_command(app, |_| JJCommand::Undo { flags }),
        AppAction::Redo => make_command(app, |_| JJCommand::Redo { flags }),
    }
}

/// Helper: build a command action from the selected change ID.
fn make_command(app: &App, build: impl FnOnce(String) -> JJCommand) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    let cmd = build(change_id.to_string());
    if cmd.is_interactive() {
        Action::SuspendAndRunJj(cmd)
    } else {
        Action::RunJj(cmd)
    }
}

fn enter_describe_input(app: &mut App, flags: CommandFlags) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    let current_desc = app.selected_description().unwrap_or("").to_string();
    let change_id = change_id.to_string();

    app.mode = AppMode::TextInput {
        prompt: "describe: ".to_string(),
        input: Input::new(current_desc),
        on_submit: PendingCommand::Describe { change_id, flags },
    };
    Action::None
}

fn handle_text_input(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Enter => {
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
        KeyCode::Esc => {
            app.mode = AppMode::Normal;
            Action::None
        }
        _ => {
            if let AppMode::TextInput { input, .. } = &mut app.mode {
                input.handle_event(&Event::Key(key));
            }
            Action::None
        }
    }
}

fn enter_target_select(app: &mut App, operation: TargetOperation, flags: CommandFlags) -> Action {
    let Some(source) = app.selected_change_id() else {
        return Action::None;
    };
    let source = source.to_string();
    let restore_cursor = app.cursor;
    app.mode = AppMode::TargetSelect {
        prompt: operation.label(),
        source,
        restore_cursor,
        operation,
        flags,
    };
    Action::None
}

fn handle_target_select(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        // Confirm target selection.
        KeyCode::Enter => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::TargetSelect {
                source,
                operation,
                flags,
                ..
            } = mode
            {
                let Some(target) = app.selected_change_id() else {
                    return Action::None;
                };
                let target = target.to_string();
                let builder = operation.build(source, target.clone());
                let prompt = format!("{} {}:", operation.label(), target);
                app.mode = AppMode::MessageChoice {
                    prompt,
                    builder,
                    flags,
                };
            }
            Action::None
        }
        // Cancel.
        KeyCode::Esc => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::TargetSelect { restore_cursor, .. } = mode {
                app.cursor = restore_cursor;
            }
            Action::None
        }
        // Navigation keys pass through normally.
        KeyCode::Char('j') | KeyCode::Down => {
            app.move_down();
            Action::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.move_up();
            Action::None
        }
        KeyCode::Char('J') => {
            app.move_down_section();
            Action::None
        }
        KeyCode::Char('K') => {
            app.move_up_section();
            Action::None
        }
        _ => Action::None,
    }
}

fn handle_message_choice(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        // Default message behavior.
        KeyCode::Char('s') => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::MessageChoice { builder, flags, .. } = mode {
                let cmd = builder.build_default(flags);
                if cmd.is_interactive() {
                    Action::SuspendAndRunJj(cmd)
                } else {
                    Action::RunJj(cmd)
                }
            } else {
                Action::None
            }
        }
        // With inline message.
        KeyCode::Char('m') => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::MessageChoice { builder, flags, .. } = mode {
                app.mode = AppMode::TextInput {
                    prompt: "message: ".to_string(),
                    input: Input::new(String::new()),
                    on_submit: PendingCommand::SquashWithMessage { builder, flags },
                };
            }
            Action::None
        }
        // Use destination message.
        KeyCode::Char('u') => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::MessageChoice { builder, flags, .. } = mode {
                let cmd = builder.build_use_dest_message(flags);
                if cmd.is_interactive() {
                    Action::SuspendAndRunJj(cmd)
                } else {
                    Action::RunJj(cmd)
                }
            } else {
                Action::None
            }
        }
        // Cancel.
        KeyCode::Esc => {
            app.mode = AppMode::Normal;
            Action::None
        }
        _ => Action::None,
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
