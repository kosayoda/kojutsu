use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use tui_input::backend::crossterm::EventHandler;

use crate::app::{App, AppMode};
use crate::dag::DiffLineKind;
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
use crate::jj_command::{ChangeSelection, JJCommand};
use crate::keymap::{
    self, action_label, action_supported_selection_kinds, AppAction, CommandFlags, Keymap,
    LookupResult,
};
use crate::types::{
    ChangeId, DisplayRow, FollowUpAction, FollowUpOption, MessageMode, PendingCommand,
    PendingCommitSelect, PendingSelection, RebaseSource, SelectionKind, SplitKind, TargetOperation,
};

/// Build the appropriate `ChangeSelection` from the current app state.
fn build_change_selection(app: &App) -> ChangeSelection {
    match app.selection_kind() {
        SelectionKind::Commit => ChangeSelection::All,
        SelectionKind::File => {
            let paths = app.selected_file_paths();
            if paths.is_empty() {
                ChangeSelection::All
            } else {
                ChangeSelection::Files(paths)
            }
        }
        SelectionKind::Line => {
            let path = crate::selection::serialize_selections(
                app.explicit_selection()
                    .expect("line selection should be explicit"),
            )
            .expect("failed to serialize selections");
            ChangeSelection::Lines(path)
        }
    }
}

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
    /// Evaluate a new revset and refresh the view.
    UpdateRevset(String),
    /// Suspend TUI and open $EDITOR to edit the revset.
    EditRevsetInEditor,
}

/// Handle a key press, dispatching through the keymap trie and app mode.
pub fn handle_key(app: &mut App, keymap: &'static Keymap, key: KeyEvent) -> Action {
    let Some(node) = keymap::key_event_to_node(&key) else {
        return Action::None;
    };

    match &app.mode {
        AppMode::Normal => handle_normal_key(app, keymap, &node),
        AppMode::Submenu {
            children, flags, ..
        } => {
            let children = *children;
            let flags = *flags;
            handle_submenu_key(app, children, flags, &node)
        }
        AppMode::CommandOutput { .. } => {
            app.mode = AppMode::Normal;
            if node.key == keymap_parser::Key::Esc {
                Action::None
            } else {
                handle_normal_key(app, keymap, &node)
            }
        }
        AppMode::Help => {
            app.mode = app.pre_overlay_mode.take().unwrap_or(AppMode::Normal);
            if node.key == keymap_parser::Key::Esc {
                Action::None
            } else {
                match &app.mode {
                    AppMode::Normal => handle_normal_key(app, keymap, &node),
                    _ => Action::None, // in select mode, swallow the dismissal key
                }
            }
        }
        AppMode::TextInput { .. } => handle_text_input(app, key),
        AppMode::SearchInput => handle_search_input(app, key),
        AppMode::TargetSelect { .. } => handle_target_select(app, key),
        AppMode::CommitSelect { .. } => handle_commit_select(app, key),
        AppMode::FollowUp { .. } => handle_follow_up(app, key),
        AppMode::SelectFromList { .. } => handle_select_from_list(app, key),
    }
}

fn handle_normal_key(app: &mut App, keymap: &'static Keymap, node: &keymap_parser::Node) -> Action {
    if node.key == keymap_parser::Key::Esc && app.search.is_some() {
        app.clear_search();
        return Action::None;
    }

    match keymap.lookup(node) {
        LookupResult::Action(action) => {
            app.status_message = None;
            dispatch_action(app, action, CommandFlags::empty())
        }
        LookupResult::Prefix { label, children } => {
            app.status_message = None;
            if app.selection_active() {
                let kind = app.selection_kind();
                let has_supported_action = children.iter().any(|(_, node)| match node {
                    keymap::KeymapNode::Action { action, .. } => {
                        action_supported_selection_kinds(*action).contains(&kind)
                    }
                    _ => false,
                });
                if !has_supported_action {
                    return Action::None;
                }
            }
            app.mode = AppMode::Submenu {
                key: keymap::display_key(node),
                label,
                children,
                flags: CommandFlags::empty(),
                error: None,
            };
            Action::None
        }
        LookupResult::Toggle(_) => Action::None, // toggles only work inside submenus
        LookupResult::Unbound => {
            app.status_message = Some(format!("unknown key: {}", keymap::display_key(node)));
            Action::None
        }
    }
}

fn handle_submenu_key(
    app: &mut App,
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
            dispatch_action(app, action, flags)
        }
        LookupResult::Toggle(flag) => {
            if let AppMode::Submenu { flags, error, .. } = &mut app.mode {
                flags.toggle(flag);
                *error = None;
            }
            Action::None
        }
        LookupResult::Prefix { label, children } => {
            app.mode = AppMode::Submenu {
                key: keymap::display_key(node),
                label,
                children,
                flags,
                error: None,
            };
            Action::None
        }
        LookupResult::Unbound => {
            if let AppMode::Submenu { error, .. } = &mut app.mode {
                *error = Some(format!("unknown key: {}", keymap::display_key(node)));
            }
            Action::None
        }
    }
}

fn dispatch_action(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
    // Merge global toggles into the command flags.
    let flags = flags | app.toggles;

    if app.selection_active()
        && !action_supported_selection_kinds(action).contains(&app.selection_kind())
    {
        let kind = app.selection_kind();
        let kind_label = match kind {
            SelectionKind::Commit => "commit",
            SelectionKind::File => "file",
            SelectionKind::Line => "line",
        };
        app.last_command = Some(format!(
            "{} does not support {} selection",
            action_label(action),
            kind_label
        ));
        return Action::None;
    }

    // Visual mode intercepts (line or commit): constrain movement, handle v/space/esc.
    if app.in_visual_mode() {
        match action {
            AppAction::MoveDown => {
                app.visual_move_down();
                return Action::None;
            }
            AppAction::MoveUp => {
                app.visual_move_up();
                return Action::None;
            }
            AppAction::ToggleSelect => {
                app.persist_visual_selection();
                return Action::None;
            }
            AppAction::EnterVisualMode => {
                app.exit_visual_mode();
                return Action::None;
            }
            AppAction::Quit => {
                app.cancel_visual_mode();
                return Action::Quit;
            }
            _ => {
                app.cancel_visual_mode();
            }
        }
    }

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
        AppAction::MoveToTop => {
            app.move_to_top();
            Action::None
        }
        AppAction::MoveToBottom => {
            app.move_to_bottom();
            Action::None
        }
        AppAction::ToggleFold => {
            app.toggle_fold();
            Action::None
        }
        AppAction::ToggleIgnoreImmutable => {
            app.toggles ^= CommandFlags::IGNORE_IMMUTABLE;
            Action::None
        }
        AppAction::ToggleIgnoreWorkingCopy => {
            app.toggles ^= CommandFlags::IGNORE_WORKING_COPY;
            Action::None
        }
        AppAction::ToggleDebug => {
            app.toggles ^= CommandFlags::DEBUG;
            Action::None
        }
        AppAction::ToggleLineNumbers => {
            app.show_line_numbers = !app.show_line_numbers;
            Action::None
        }
        AppAction::ToggleSelect => {
            // If cursor is in a persistent visual range, toggle it into selections.
            if app.cursor_in_persistent_visual_range() {
                app.toggle_persistent_visual_selection();
                return Action::None;
            }

            // Extract row info before mutating app (borrow rules).
            enum SelectTarget {
                Commit(EntryIdx),
                File(EntryIdx, FileIdx),
                DiffLine(EntryIdx, FileIdx, DiffLineIdx, DiffLineKind),
            }
            let target = match app.rows.get(app.cursor) {
                Some(DisplayRow::CommitNode { entry_idx }) => {
                    Some(SelectTarget::Commit(*entry_idx))
                }
                Some(DisplayRow::FileChange {
                    entry_idx,
                    file_idx,
                }) => Some(SelectTarget::File(*entry_idx, *file_idx)),
                Some(DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    line_idx,
                }) => {
                    let Some(diff_lines) = app.diff_lines(*entry_idx, *file_idx) else {
                        return Action::None;
                    };
                    let kind = diff_lines[line_idx.raw()].kind;
                    Some(SelectTarget::DiffLine(
                        *entry_idx, *file_idx, *line_idx, kind,
                    ))
                }
                _ => None,
            };
            match target {
                Some(SelectTarget::Commit(entry_idx)) => {
                    app.toggle_commit_selection(entry_idx);
                }
                Some(SelectTarget::File(entry_idx, file_idx)) => {
                    app.toggle_file_selection(entry_idx, file_idx);
                }
                Some(SelectTarget::DiffLine(entry_idx, file_idx, line_idx, kind)) => {
                    match kind {
                        DiffLineKind::Added | DiffLineKind::Removed => {
                            app.toggle_line_selection(entry_idx, file_idx, line_idx);
                        }
                        DiffLineKind::Header => {
                            app.toggle_hunk_selection(entry_idx, file_idx, line_idx);
                        }
                        DiffLineKind::Context => {} // no-op
                    }
                }
                None => {}
            }
            Action::None
        }
        AppAction::EnterVisualMode => {
            app.enter_visual_mode();
            Action::None
        }
        AppAction::StartSearch => {
            app.begin_search();
            Action::None
        }
        AppAction::NextMatch => {
            app.search_next();
            Action::None
        }
        AppAction::PrevMatch => {
            app.search_prev();
            Action::None
        }
        AppAction::Refresh => Action::Refresh,
        AppAction::EditRevset => {
            let prefill = app.revset_input_text().to_string();
            app.mode = AppMode::text_input("revset: ", prefill, PendingCommand::Revset);
            Action::None
        }
        AppAction::EditRevsetInEditor => Action::EditRevsetInEditor,
        AppAction::ResetRevset => {
            app.request_revset_load(None);
            Action::None
        }
        AppAction::SwitchPreset(slot) => {
            // Save current revset to current slot before switching.
            app.revset_presets[app.active_preset] = Some(app.revset.clone());
            app.active_preset = slot;
            if let Some(revset) = &app.revset_presets[slot] {
                Action::UpdateRevset(revset.clone())
            } else {
                app.request_revset_load(None);
                Action::None
            }
        }
        AppAction::WorkspaceAdd => {
            app.mode = AppMode::text_input("workspace path: ", "", PendingCommand::WorkspaceAddPath { flags });
            Action::None
        }
        AppAction::WorkspaceForget => {
            let entry_idx = app.selected_entry_idx();
            let workspaces: Vec<String> = entry_idx
                .map(|idx| {
                    app.entries[idx]
                        .commit
                        .workspaces
                        .iter()
                        .filter(|ws| !ws.is_current)
                        .map(|ws| ws.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            if workspaces.len() == 1 {
                Action::RunJj(JJCommand::WorkspaceForget {
                    names: workspaces,
                    flags,
                })
            } else if workspaces.len() > 1 {
                app.mode = AppMode::select_from_list(
                    "forget workspace",
                    workspaces,
                    true,
                    PendingSelection::WorkspaceForget { flags },
                );
                Action::None
            } else {
                app.status_message = Some("no other workspace on this commit".to_string());
                Action::None
            }
        }
        AppAction::WorkspaceList => Action::RunJj(JJCommand::WorkspaceList { flags }),
        AppAction::ShowHelp => {
            app.mode = AppMode::Help;
            Action::None
        }
        AppAction::Abandon => make_multi_command(app, |ids| JJCommand::Abandon {
            change_ids: ids,
            flags,
        }),
        AppAction::Absorb => make_command(app, |id| JJCommand::Absorb {
            from: Some(id),
            selection: build_change_selection(app),
            flags,
        }),
        AppAction::Commit => {
            let cmd = JJCommand::Commit {
                message: None,
                selection: build_change_selection(app),
                flags,
            };
            Action::SuspendAndRunJj(cmd)
        }
        AppAction::CommitWithMessage => {
            app.mode = AppMode::text_input(
                "commit message: ",
                "",
                PendingCommand::Commit {
                    flags,
                    selection: build_change_selection(app),
                },
            );
            Action::None
        }
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
        AppAction::Squash => {
            let selection = build_change_selection(app);
            make_command(app, |id| JJCommand::Squash {
                change_id: id,
                target: None,
                message: MessageMode::Default,
                selection: selection.clone(),
                flags,
            })
        }
        AppAction::SquashSelect(kind) => {
            enter_target_select(app, TargetOperation::Squash(kind), flags)
        }
        // Rebase -- target selection
        AppAction::RebaseRevision => {
            enter_target_select(app, TargetOperation::Rebase(RebaseSource::Revision), flags)
        }
        AppAction::RebaseSource => {
            enter_target_select(app, TargetOperation::Rebase(RebaseSource::Source), flags)
        }
        AppAction::RebaseBranch => {
            enter_target_select(app, TargetOperation::Rebase(RebaseSource::Branch), flags)
        }
        AppAction::Restore => make_command(app, |id| JJCommand::Restore {
            from: None,
            into: None,
            changes_in: Some(id),
            selection: build_change_selection(app),
            flags,
        }),
        AppAction::RestoreFrom => enter_target_select(app, TargetOperation::RestoreFrom, flags),
        AppAction::RestoreInto => enter_target_select(app, TargetOperation::RestoreInto, flags),
        AppAction::Split => make_command(app, |id| JJCommand::Split {
            change_id: id,
            target: None,
            selection: build_change_selection(app),
            flags,
        }),
        AppAction::SplitOnto => {
            enter_target_select(app, TargetOperation::Split(SplitKind::Onto), flags)
        }
        AppAction::SplitAfter => {
            enter_target_select(app, TargetOperation::Split(SplitKind::After), flags)
        }
        AppAction::SplitBefore => {
            enter_target_select(app, TargetOperation::Split(SplitKind::Before), flags)
        }

        // Bookmark commands
        AppAction::BookmarkCreate => {
            enter_bookmark_text_input(app, flags, "create bookmark: ", BookmarkTextAction::Create)
        }
        AppAction::BookmarkSet => {
            enter_bookmark_text_input(app, flags, "set bookmark: ", BookmarkTextAction::Set)
        }
        AppAction::BookmarkDelete => {
            enter_bookmark_select(app, flags, PendingSelectionKind::Delete)
        }
        AppAction::BookmarkForget => {
            enter_bookmark_select(app, flags, PendingSelectionKind::Forget)
        }
        AppAction::BookmarkMove => enter_bookmark_select(app, flags, PendingSelectionKind::Move),
        AppAction::BookmarkRename => {
            enter_bookmark_select(app, flags, PendingSelectionKind::Rename)
        }
        AppAction::BookmarkAdvance => enter_bookmark_advance(app, flags),
        AppAction::BookmarkTrack => enter_remote_bookmark_select(
            app,
            &app.untracked_bookmarks.clone(),
            "no untracked remote bookmarks",
            "track bookmark",
            PendingSelection::BookmarkTrack { flags },
        ),
        AppAction::BookmarkUntrack => enter_remote_bookmark_select(
            app,
            &app.tracked_bookmarks.clone(),
            "no tracked remote bookmarks",
            "untrack bookmark",
            PendingSelection::BookmarkUntrack { flags },
        ),

        AppAction::Undo => make_command(app, |_| JJCommand::Undo { flags }),
        AppAction::Redo => make_command(app, |_| JJCommand::Redo { flags }),

        // Git commands (network ops suspend TUI for SSH auth / progress)
        AppAction::GitFetch => Action::SuspendAndRunJj(JJCommand::GitFetch {
            all_remotes: false,
            flags,
        }),
        AppAction::GitFetchAllRemotes => Action::SuspendAndRunJj(JJCommand::GitFetch {
            all_remotes: true,
            flags,
        }),
        AppAction::GitPush => Action::SuspendAndRunJj(JJCommand::GitPush { all: false, flags }),
        AppAction::GitPushAll => Action::SuspendAndRunJj(JJCommand::GitPush { all: true, flags }),
        AppAction::GitPushChange => {
            let Some(change_id) = app.selected_change_id() else {
                return Action::None;
            };
            Action::SuspendAndRunJj(JJCommand::GitPushChange { change_id, flags })
        }
        AppAction::GitPushBookmark => {
            let bookmarks = app.selected_bookmarks().unwrap_or(&[]);
            if bookmarks.is_empty() {
                app.status_message = Some("no bookmarks on this commit".to_string());
                return Action::None;
            }
            let items: Vec<String> = bookmarks.iter().map(|b| b.name.clone()).collect();
            if items.len() == 1 {
                return Action::SuspendAndRunJj(JJCommand::GitPushBookmark {
                    bookmarks: items,
                    flags,
                });
            }
            app.mode = AppMode::select_from_list(
                "push bookmark",
                items,
                true,
                PendingSelection::GitPushBookmark { flags },
            );
            Action::None
        }
        AppAction::GitExport => Action::RunJj(JJCommand::GitExport { flags }),
        AppAction::GitImport => Action::RunJj(JJCommand::GitImport { flags }),

        // Duplicate
        AppAction::Duplicate => make_command(app, |id| JJCommand::Duplicate {
            change_id: id,
            onto: None,
            flags,
        }),
        AppAction::DuplicateOnto => enter_target_select(app, TargetOperation::DuplicateOnto, flags),
    }
}

/// Turn a command into the appropriate run/suspend action.
fn run_cmd(cmd: JJCommand) -> Action {
    if cmd.is_interactive() {
        Action::SuspendAndRunJj(cmd)
    } else {
        Action::RunJj(cmd)
    }
}

/// Dispatch a follow-up action (execute command or enter text input).
fn execute_follow_up(app: &mut App, action: FollowUpAction) -> Action {
    match action {
        FollowUpAction::Execute(cmd) => run_cmd(cmd),
        FollowUpAction::TextInput { prompt, pending } => {
            app.mode = AppMode::text_input(prompt, "", pending);
            Action::None
        }
    }
}

/// Helper: build a command from multiple selected change IDs (or cursor fallback).
fn make_multi_command(app: &App, build: impl FnOnce(Vec<ChangeId>) -> JJCommand) -> Action {
    let ids = app.selected_change_ids();
    if ids.is_empty() {
        return Action::None;
    }
    run_cmd(build(ids))
}

/// Helper: build a single-commit command from the cursor change ID.
///
/// Returns `Action::None` if multi-commit selection is active (single-commit
/// commands should not silently act on just the cursor).
fn make_command(app: &App, build: impl FnOnce(ChangeId) -> JJCommand) -> Action {
    if app.selection_kind() == SelectionKind::Commit && app.selection_active() {
        return Action::None;
    }
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    run_cmd(build(change_id))
}

fn enter_describe_input(app: &mut App, flags: CommandFlags) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    let current_desc = app.selected_description().unwrap_or("").to_string();
    app.mode = AppMode::text_input("describe: ", current_desc, PendingCommand::Describe { change_id, flags });
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
                let text = input.to_string();
                match on_submit {
                    PendingCommand::Revset => Action::UpdateRevset(text),
                    PendingCommand::WorkspaceAddPath { flags } => {
                        app.mode = AppMode::text_input(
                            "workspace name (enter for default): ",
                            "",
                            PendingCommand::WorkspaceAddName { path: text, flags },
                        );
                        Action::None
                    }
                    PendingCommand::WorkspaceAddName { path, flags } => {
                        let name = if text.is_empty() { None } else { Some(text) };
                        let restore_cursor = app.cursor;
                        app.mode = AppMode::CommitSelect {
                            restore_cursor,
                            pending: PendingCommitSelect::WorkspaceAdd { path, name },
                            flags,
                        };
                        Action::None
                    }
                    cmd => Action::RunJj(cmd.into_jj_command(text)),
                }
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

fn handle_search_input(app: &mut App, key: KeyEvent) -> Action {
    use crate::types::SearchFocus;

    match key.code {
        KeyCode::Esc => {
            app.cancel_search();
            Action::None
        }
        KeyCode::Enter => {
            app.confirm_search();
            Action::None
        }
        KeyCode::Tab => {
            app.toggle_search_focus();
            Action::None
        }
        _ => {
            let focus = app.search.as_ref().map(|s| s.focus);
            match focus {
                Some(SearchFocus::Query) => match key.code {
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.search_next();
                    }
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.search_prev();
                    }
                    _ => {
                        if let Some(search) = &mut app.search {
                            let mut input = search.input.clone();
                            input.handle_event(&Event::Key(key));
                            app.update_search_input(input);
                        }
                    }
                },
                Some(SearchFocus::Scopes) => match key.code {
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.search_next();
                    }
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.search_prev();
                    }
                    KeyCode::Char('0') => app.reset_search_scopes(),
                    KeyCode::Char('*') => app.enable_all_search_scopes(),
                    KeyCode::Char('c') => {
                        app.toggle_search_scope(crate::types::SearchScopes::CHANGE_ID)
                    }
                    KeyCode::Char('i') => {
                        app.toggle_search_scope(crate::types::SearchScopes::COMMIT_ID)
                    }
                    KeyCode::Char('d') => {
                        app.toggle_search_scope(crate::types::SearchScopes::DESCRIPTION)
                    }
                    KeyCode::Char('b') => {
                        app.toggle_search_scope(crate::types::SearchScopes::BOOKMARK)
                    }
                    KeyCode::Char('a') => {
                        app.toggle_search_scope(crate::types::SearchScopes::AUTHOR)
                    }
                    KeyCode::Char('p') => app.toggle_search_scope(crate::types::SearchScopes::PATH),
                    KeyCode::Char('l') => app.toggle_search_scope(crate::types::SearchScopes::LINE),
                    _ => {}
                },
                None => {}
            }
            Action::None
        }
    }
}

fn enter_target_select(app: &mut App, operation: TargetOperation, flags: CommandFlags) -> Action {
    let Some(source) = app.selected_change_id() else {
        return Action::None;
    };
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

/// Shared navigation for TargetSelect and CommitSelect modes.
/// Returns `Some(Action)` if the key was handled, `None` if not recognized.
///
/// Uses `key_event_to_node` for consistent key matching with the keymap system.
fn handle_select_navigation(app: &mut App, key: &KeyEvent) -> Option<Action> {
    use keymap_parser::Key;

    let node = keymap::key_event_to_node(key)?;
    let shift = (node.modifiers & keymap_parser::Modifier::Shift as u8) != 0;
    let ctrl = (node.modifiers & keymap_parser::Modifier::Ctrl as u8) != 0;

    match (node.key, shift, ctrl) {
        (Key::Char('j'), true, _) => {
            app.move_down_section();
            Some(Action::None)
        }
        (Key::Char('k'), true, _) => {
            app.move_up_section();
            Some(Action::None)
        }
        (Key::Char('d'), _, true) => {
            app.page_down(15);
            Some(Action::None)
        }
        (Key::Char('u'), _, true) => {
            app.page_up(15);
            Some(Action::None)
        }
        (Key::Char('n'), _, true) => {
            app.search_next();
            Some(Action::None)
        }
        (Key::Char('p'), _, true) => {
            app.search_prev();
            Some(Action::None)
        }
        (Key::Char('j'), _, _) | (Key::Down, _, _) => {
            app.move_down();
            Some(Action::None)
        }
        (Key::Char('k'), _, _) | (Key::Up, _, _) => {
            app.move_up();
            Some(Action::None)
        }
        (Key::PageDown, _, _) => {
            app.page_down(15);
            Some(Action::None)
        }
        (Key::PageUp, _, _) => {
            app.page_up(15);
            Some(Action::None)
        }
        (Key::Char('@'), _, _) => {
            app.jump_to_working_copy();
            Some(Action::None)
        }
        (Key::Char('0'), _, _) => {
            app.move_to_top();
            Some(Action::None)
        }
        (Key::Char('$'), _, _) => {
            app.move_to_bottom();
            Some(Action::None)
        }
        (Key::Tab, _, _) => {
            app.toggle_fold();
            Some(Action::None)
        }
        (Key::Char('/'), _, _) => {
            app.begin_search();
            Some(Action::None)
        }
        (Key::Char('?'), _, _) => {
            let old_mode = std::mem::replace(&mut app.mode, AppMode::Help);
            app.pre_overlay_mode = Some(old_mode);
            Some(Action::None)
        }
        _ => None,
    }
}

fn handle_target_select(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
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
                let label = operation.label();
                let selection = build_change_selection(app);
                let mut options = operation.follow_up(source, target.clone(), flags, selection);
                if options.len() == 1 {
                    let opt = options.remove(0);
                    return execute_follow_up(app, opt.action);
                }
                let prompt = format!("{label} {target}:");
                app.mode = AppMode::FollowUp { prompt, options };
            }
            Action::None
        }
        KeyCode::Esc => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::TargetSelect { restore_cursor, .. } = mode {
                app.cursor = restore_cursor;
            }
            Action::None
        }
        _ => handle_select_navigation(app, &key).unwrap_or(Action::None),
    }
}

fn handle_commit_select(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Enter => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::CommitSelect { pending, flags, .. } = mode {
                let Some(target) = app.selected_change_id() else {
                    return Action::None;
                };
                let cmd = pending.into_jj_command(target, flags);
                Action::RunJj(cmd)
            } else {
                Action::None
            }
        }
        KeyCode::Esc => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::CommitSelect { restore_cursor, .. } = mode {
                app.cursor = restore_cursor;
            }
            Action::None
        }
        _ => handle_select_navigation(app, &key).unwrap_or(Action::None),
    }
}

fn handle_follow_up(app: &mut App, key: KeyEvent) -> Action {
    if key.code == KeyCode::Esc {
        app.mode = AppMode::Normal;
        return Action::None;
    }

    let c = match key.code {
        KeyCode::Char(c) => c,
        _ => return Action::None,
    };

    let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
    let AppMode::FollowUp { options, .. } = mode else {
        return Action::None;
    };

    // Find the matching option.
    let Some(option) = options.into_iter().find(|o| o.key == c) else {
        return Action::None;
    };

    execute_follow_up(app, option.action)
}

// ---------------------------------------------------------------------------
// Bookmark helpers
// ---------------------------------------------------------------------------

/// Show a select-from-list for remote bookmarks, or a status message if empty.
fn enter_remote_bookmark_select(
    app: &mut App,
    bookmarks: &[String],
    empty_msg: &str,
    title: &str,
    on_select: PendingSelection,
) -> Action {
    if bookmarks.is_empty() {
        app.status_message = Some(empty_msg.to_string());
        return Action::None;
    }
    let items = bookmarks.to_vec();
    app.mode = AppMode::select_from_list(title, items, true, on_select);
    Action::None
}

enum BookmarkTextAction {
    Create,
    Set,
}

enum PendingSelectionKind {
    Delete,
    Forget,
    Move,
    Rename,
}

fn enter_bookmark_advance(app: &mut App, flags: CommandFlags) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };

    // Check if the selected commit is the working copy.
    let is_wc = app.selected_bookmarks().is_some_and(|_| {
        // Check via the entries
        let entry_idx = match app.rows.get(app.cursor) {
            Some(DisplayRow::CommitNode { entry_idx }) => Some(*entry_idx),
            Some(DisplayRow::GraphLink { entry_idx, .. }) => Some(*entry_idx),
            Some(DisplayRow::FileChange { entry_idx, .. }) => Some(*entry_idx),
            Some(DisplayRow::DiffLine { entry_idx, .. }) => Some(*entry_idx),
            None => None,
        };
        entry_idx.is_some_and(|idx| app.entries[idx].commit.is_working_copy())
    });

    if is_wc {
        // On working copy: advance immediately (jj default = advance to @).
        Action::RunJj(JJCommand::BookmarkAdvance {
            change_id: None,
            flags,
        })
    } else {
        // Not on working copy: show follow-up to choose between selected and @.
        app.mode = AppMode::FollowUp {
            prompt: "advance bookmarks to:".to_string(),
            options: vec![
                FollowUpOption {
                    key: 's',
                    label: "selected",
                    action: FollowUpAction::Execute(JJCommand::BookmarkAdvance {
                        change_id: Some(change_id),
                        flags,
                    }),
                },
                FollowUpOption {
                    key: '@',
                    label: "working copy",
                    action: FollowUpAction::Execute(JJCommand::BookmarkAdvance {
                        change_id: None,
                        flags,
                    }),
                },
            ],
        };
        Action::None
    }
}

fn enter_bookmark_text_input(
    app: &mut App,
    flags: CommandFlags,
    prompt: &str,
    action: BookmarkTextAction,
) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };

    let on_submit = match action {
        BookmarkTextAction::Create => PendingCommand::BookmarkCreate { change_id, flags },
        BookmarkTextAction::Set => PendingCommand::BookmarkSet { change_id, flags },
    };

    app.mode = AppMode::text_input(prompt, "", on_submit);
    Action::None
}

fn enter_bookmark_select(app: &mut App, flags: CommandFlags, kind: PendingSelectionKind) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    let bookmarks = app.selected_bookmarks().unwrap_or(&[]);
    if bookmarks.is_empty() {
        return Action::None;
    }

    let on_select = match kind {
        PendingSelectionKind::Delete => PendingSelection::BookmarkDelete { change_id, flags },
        PendingSelectionKind::Forget => PendingSelection::BookmarkForget { change_id, flags },
        PendingSelectionKind::Move => PendingSelection::BookmarkMove { change_id, flags },
        PendingSelectionKind::Rename => PendingSelection::BookmarkRename { change_id, flags },
    };

    let title = match kind {
        PendingSelectionKind::Delete => "delete bookmark",
        PendingSelectionKind::Forget => "forget bookmark",
        PendingSelectionKind::Move => "move bookmark",
        PendingSelectionKind::Rename => "rename bookmark",
    };

    let items: Vec<String> = bookmarks.iter().map(|b| b.name.clone()).collect();

    // Skip selection if only one bookmark.
    if items.len() == 1 {
        return resolve_bookmark_selection(app, on_select, items.into_iter().next().unwrap());
    }

    let multi = matches!(
        kind,
        PendingSelectionKind::Delete | PendingSelectionKind::Forget
    );

    app.mode = AppMode::select_from_list(title, items, multi, on_select);
    Action::None
}

/// Recompute which items match the filter.
fn recompute_list_filter(items: &[String], filter: &str) -> Vec<usize> {
    if filter.is_empty() {
        return (0..items.len()).collect();
    }
    let lower = filter.to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.to_lowercase().contains(&lower))
        .map(|(i, _)| i)
        .collect()
}

fn handle_select_from_list(app: &mut App, key: KeyEvent) -> Action {
    use keymap_parser::Key;

    let node = keymap::key_event_to_node(&key);
    let ctrl = node
        .as_ref()
        .is_some_and(|n| (n.modifiers & keymap_parser::Modifier::Ctrl as u8) != 0);
    let node_key = node.map(|n| n.key);

    // While filtering, intercept all keys except Tab/Esc/Enter.
    let is_filtering = matches!(
        &app.mode,
        AppMode::SelectFromList {
            filtering: true,
            ..
        }
    );
    if is_filtering {
        match key.code {
            KeyCode::Char(c) if !ctrl => {
                if let AppMode::SelectFromList {
                    filter,
                    items,
                    filtered_indices,
                    cursor,
                    scroll_offset,
                    ..
                } = &mut app.mode
                {
                    filter.push(c);
                    *filtered_indices = recompute_list_filter(items, filter);
                    *cursor = 0;
                    *scroll_offset = 0;
                }
                return Action::None;
            }
            KeyCode::Backspace => {
                if let AppMode::SelectFromList {
                    filter,
                    items,
                    filtered_indices,
                    cursor,
                    scroll_offset,
                    ..
                } = &mut app.mode
                {
                    filter.pop();
                    *filtered_indices = recompute_list_filter(items, filter);
                    *cursor = (*cursor).min(filtered_indices.len().saturating_sub(1));
                    *scroll_offset = 0;
                }
                return Action::None;
            }
            // Tab, Esc, Enter fall through to the main match below.
            _ if matches!(node_key, Some(Key::Tab) | Some(Key::Esc) | Some(Key::Enter)) => {}
            // All other keys (arrows, etc.) are swallowed while filtering.
            _ => return Action::None,
        }
    }

    match node_key {
        Some(Key::Char('j')) | Some(Key::Down) => {
            if let AppMode::SelectFromList {
                cursor,
                filtered_indices,
                ..
            } = &mut app.mode
            {
                if *cursor + 1 < filtered_indices.len() {
                    *cursor += 1;
                }
            }
            Action::None
        }
        Some(Key::Char('k')) | Some(Key::Up) => {
            if let AppMode::SelectFromList { cursor, .. } = &mut app.mode {
                *cursor = cursor.saturating_sub(1);
            }
            Action::None
        }
        Some(Key::Char('d')) if ctrl => {
            if let AppMode::SelectFromList {
                cursor,
                filtered_indices,
                ..
            } = &mut app.mode
            {
                *cursor = (*cursor + 10).min(filtered_indices.len().saturating_sub(1));
            }
            Action::None
        }
        Some(Key::Char('u')) if ctrl => {
            if let AppMode::SelectFromList { cursor, .. } = &mut app.mode {
                *cursor = cursor.saturating_sub(10);
            }
            Action::None
        }
        Some(Key::PageDown) => {
            if let AppMode::SelectFromList {
                cursor,
                filtered_indices,
                ..
            } = &mut app.mode
            {
                *cursor = (*cursor + 10).min(filtered_indices.len().saturating_sub(1));
            }
            Action::None
        }
        Some(Key::PageUp) => {
            if let AppMode::SelectFromList { cursor, .. } = &mut app.mode {
                *cursor = cursor.saturating_sub(10);
            }
            Action::None
        }
        Some(Key::Char('0')) => {
            if let AppMode::SelectFromList { cursor, .. } = &mut app.mode {
                *cursor = 0;
            }
            Action::None
        }
        Some(Key::Char('$')) => {
            if let AppMode::SelectFromList {
                cursor,
                filtered_indices,
                ..
            } = &mut app.mode
            {
                *cursor = filtered_indices.len().saturating_sub(1);
            }
            Action::None
        }
        Some(Key::Tab) => {
            if let AppMode::SelectFromList { filtering, .. } = &mut app.mode {
                *filtering = !*filtering;
            }
            Action::None
        }
        Some(Key::Space) => {
            if let AppMode::SelectFromList {
                cursor,
                filtered_indices,
                marked,
                multi,
                ..
            } = &mut app.mode
            {
                if *multi {
                    if let Some(&orig_idx) = filtered_indices.get(*cursor) {
                        if marked.contains(&orig_idx) {
                            marked.remove(&orig_idx);
                        } else {
                            marked.insert(orig_idx);
                        }
                    }
                }
            }
            Action::None
        }
        Some(Key::Enter) => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::SelectFromList {
                items,
                filtered_indices,
                cursor,
                marked,
                multi,
                on_select,
                ..
            } = mode
            {
                if multi {
                    let names: Vec<String> = if marked.is_empty() {
                        // Nothing marked — use cursor item (translated through filter).
                        let orig_idx = filtered_indices.get(cursor).copied().unwrap_or(0);
                        vec![items.into_iter().nth(orig_idx).unwrap_or_default()]
                    } else {
                        let mut indices: Vec<usize> = marked.into_iter().collect();
                        indices.sort();
                        indices
                            .into_iter()
                            .filter_map(|i| items.get(i).cloned())
                            .collect()
                    };
                    resolve_multi_selection(app, on_select, names)
                } else {
                    let orig_idx = filtered_indices.get(cursor).copied().unwrap_or(0);
                    let name = items.into_iter().nth(orig_idx).unwrap_or_default();
                    resolve_bookmark_selection(app, on_select, name)
                }
            } else {
                Action::None
            }
        }
        Some(Key::Esc) => {
            // If filtering, just exit filter focus — keep the filter text.
            if let AppMode::SelectFromList { filtering, .. } = &mut app.mode {
                if *filtering {
                    *filtering = false;
                    return Action::None;
                }
            }
            app.mode = AppMode::Normal;
            Action::None
        }
        _ => Action::None,
    }
}

/// After a bookmark has been selected (from list or auto-selected), decide
/// what to do next based on the `PendingSelection`.
fn resolve_bookmark_selection(app: &mut App, on_select: PendingSelection, name: String) -> Action {
    match on_select {
        PendingSelection::BookmarkDelete { flags, .. } => {
            Action::RunJj(JJCommand::BookmarkDelete {
                names: vec![name],
                flags,
            })
        }
        PendingSelection::BookmarkForget { flags, .. } => {
            Action::RunJj(JJCommand::BookmarkForget {
                names: vec![name],
                flags,
            })
        }
        PendingSelection::BookmarkMove {
            change_id, flags, ..
        } => {
            app.mode = AppMode::TargetSelect {
                prompt: "move bookmark",
                source: change_id,
                restore_cursor: app.cursor,
                operation: TargetOperation::BookmarkMove {
                    bookmark_name: name,
                },
                flags,
            };
            Action::None
        }
        PendingSelection::BookmarkRename { flags, .. } => {
            app.mode = AppMode::text_input(
                "rename to: ",
                name.clone(),
                PendingCommand::BookmarkRename { old_name: name, flags },
            );
            Action::None
        }
        PendingSelection::WorkspaceForget { flags } => Action::RunJj(JJCommand::WorkspaceForget {
            names: vec![name],
            flags,
        }),
        PendingSelection::BookmarkTrack { flags } => Action::RunJj(JJCommand::BookmarkTrack {
            bookmarks: parse_remote_bookmarks(vec![name]),
            flags,
        }),
        PendingSelection::BookmarkUntrack { flags } => Action::RunJj(JJCommand::BookmarkUntrack {
            bookmarks: parse_remote_bookmarks(vec![name]),
            flags,
        }),
        PendingSelection::GitPushBookmark { flags } => {
            Action::SuspendAndRunJj(JJCommand::GitPushBookmark {
                bookmarks: vec![name],
                flags,
            })
        }
    }
}

/// Parse `"name@remote"` display strings into `(name, remote)` tuples.
fn parse_remote_bookmarks(names: Vec<String>) -> Vec<(String, String)> {
    names
        .into_iter()
        .filter_map(|s| {
            let (name, remote) = s.rsplit_once('@')?;
            Some((name.to_string(), remote.to_string()))
        })
        .collect()
}

fn resolve_multi_selection(
    app: &mut App,
    on_select: PendingSelection,
    names: Vec<String>,
) -> Action {
    match on_select {
        PendingSelection::BookmarkDelete { flags, .. } => {
            Action::RunJj(JJCommand::BookmarkDelete { names, flags })
        }
        PendingSelection::BookmarkForget { flags, .. } => {
            Action::RunJj(JJCommand::BookmarkForget { names, flags })
        }
        PendingSelection::WorkspaceForget { flags } => {
            Action::RunJj(JJCommand::WorkspaceForget { names, flags })
        }
        PendingSelection::BookmarkTrack { flags } => Action::RunJj(JJCommand::BookmarkTrack {
            bookmarks: parse_remote_bookmarks(names),
            flags,
        }),
        PendingSelection::BookmarkUntrack { flags } => Action::RunJj(JJCommand::BookmarkUntrack {
            bookmarks: parse_remote_bookmarks(names),
            flags,
        }),
        PendingSelection::GitPushBookmark { flags } => {
            Action::SuspendAndRunJj(JJCommand::GitPushBookmark {
                bookmarks: names,
                flags,
            })
        }
        // Move/Rename should never reach here (multi: false), but handle gracefully.
        other => {
            let name = names.into_iter().next().unwrap_or_default();
            resolve_bookmark_selection(app, other, name)
        }
    }
}

/// Handle a mouse event.
pub fn handle_mouse(app: &mut App, mouse: MouseEvent, list_offset: u16) -> Action {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.mode = AppMode::Normal;
            let row = (mouse.row.saturating_sub(list_offset)) as usize + app.scroll_offset();
            app.select_row(row);
            Action::None
        }
        MouseEventKind::Down(MouseButton::Right) => {
            app.mode = AppMode::Normal;
            let row = (mouse.row.saturating_sub(list_offset)) as usize + app.scroll_offset();
            app.select_row(row);
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
        _ => Action::None, // Mouse move, drag, etc. -- don't dismiss overlays.
    }
}
