use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use tui_input::backend::crossterm::EventHandler;

use std::collections::HashSet;

use crate::app::{App, AppMode, TargetMode};
use crate::dag::{BookmarkRef, DiffLineKind};
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
use crate::jj_command::{ChangeSelection, InsertPosition, JJCommand};
use crate::keymap::{
    self, action_label, action_supported_selection_kinds, AppAction, CommandFlags, Keymap, Keymaps,
    LookupResult,
};
use smallvec::smallvec;

use crate::types::{
    BookmarkName, ChangeId, CommitId, DisplayRow, FollowUpAction, FollowUpOption, MessageMode,
    PendingCommand, PendingCommitSelect, PendingSelection, RebaseSource, SelectionKind, SmallVec,
    SplitKind, SquashKind, Str, TargetOperation,
};

/// Number of rows to jump for page-up/page-down style navigation.
const PAGE_SIZE: usize = 15;

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
pub fn handle_key(app: &mut App, keymaps: &'static Keymaps, key: KeyEvent) -> Action {
    let Some(node) = keymap::key_event_to_node(&key) else {
        return Action::None;
    };
    let keymap = keymaps.for_view(app.active_view);

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
                    app.mode = app.pre_overlay_mode.take().unwrap_or(AppMode::Normal);
                    Action::None
                }
                // Dismiss and forward printable keys to the underlying mode.
                Key::Char(_) => {
                    app.mode = app.pre_overlay_mode.take().unwrap_or(AppMode::Normal);
                    match &app.mode {
                        AppMode::Normal => handle_normal_key(app, keymap, &node),
                        AppMode::TargetSelect { .. } => handle_target_select(app, key),
                        AppMode::CommitSelect { .. } => handle_commit_select(app, key),
                        _ => Action::None,
                    }
                }
                // Ignore everything else (function keys, modifier combos,
                // spurious escape sequences from terminal resize, etc.).
                _ => Action::None,
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
            };
            Action::None
        }
        LookupResult::Toggle(_) => Action::None, // toggles only work inside submenus
        LookupResult::Unbound => {
            app.set_error(format!("unknown key: {}", keymap::display_key(node)));
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
            if let AppMode::Submenu { flags, .. } = &mut app.mode {
                flags.toggle(flag);
                app.status_message = None;
            }
            Action::None
        }
        LookupResult::Prefix { label, children } => {
            app.mode = AppMode::Submenu {
                key: keymap::display_key(node),
                label,
                children,
                flags,
            };
            Action::None
        }
        LookupResult::Unbound => {
            app.set_error(format!("unknown key: {}", keymap::display_key(node)));
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
        AppAction::ScrollLeft => {
            app.h_scroll = app.h_scroll.saturating_sub(4);
            Action::None
        }
        AppAction::ScrollRight => {
            app.h_scroll = app.h_scroll.saturating_add(4);
            Action::None
        }
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
            app.page_down(PAGE_SIZE);
            Action::None
        }
        AppAction::PageUp => {
            app.page_up(PAGE_SIZE);
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
        AppAction::SelectPreset => {
            if app.revset.presets.is_empty() {
                app.set_status("no presets configured");
                return Action::None;
            }
            let items: Vec<String> = app.revset.presets.iter().map(|p| p.name.clone()).collect();
            app.mode = AppMode::select_from_list(
                "switch preset",
                items,
                false,
                PendingSelection::PresetSelect,
                false,
            );
            Action::None
        }
        AppAction::EditRevset => {
            let prefill = app.revset_input_text().to_string();
            app.mode = AppMode::text_input("revset: ", prefill, PendingCommand::Revset);
            Action::None
        }
        AppAction::EditRevsetInEditor => Action::EditRevsetInEditor,
        AppAction::ResetRevset => {
            app.revset.active_preset = None;
            app.request_revset_load(None);
            Action::None
        }
        AppAction::SwitchPreset(slot) => {
            if let Some(preset) = app.revset.presets.get(slot) {
                app.revset.active_preset = Some(slot);
                Action::UpdateRevset(preset.revset.clone())
            } else {
                // No preset at this slot — use jj's default revset.
                app.revset.active_preset = None;
                app.request_revset_load(None);
                Action::None
            }
        }
        AppAction::WorkspaceAdd => {
            app.mode = AppMode::text_input(
                "workspace path: ",
                "",
                PendingCommand::WorkspaceAddPath { flags },
            );
            Action::None
        }
        AppAction::WorkspaceForget => {
            let entry_idx = app.selected_entry_idx();
            let workspaces: Vec<String> = entry_idx
                .map(|idx| {
                    app.nodes[idx]
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
                    names: workspaces.into(),
                    flags,
                })
            } else if workspaces.len() > 1 {
                app.mode = AppMode::select_from_list(
                    "forget workspace",
                    workspaces,
                    true,
                    PendingSelection::WorkspaceForget { flags },
                    false,
                );
                Action::None
            } else {
                app.set_status("no other workspace on this commit");
                Action::None
            }
        }
        AppAction::WorkspaceList => Action::RunJj(JJCommand::WorkspaceList { flags }),
        AppAction::WorkspaceRename => {
            app.mode = AppMode::text_input(
                "rename workspace to: ",
                "",
                PendingCommand::WorkspaceRename { flags },
            );
            Action::None
        }
        AppAction::ShowHelp => {
            app.mode = AppMode::Help { scroll: 0 };
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
        AppAction::ExpandAncestors => {
            let Some(entry_idx) = app.selected_entry_idx() else {
                return Action::None;
            };
            app.expand_ancestors(entry_idx);
            Action::None
        }
        AppAction::Fix => make_multi_command(app, |ids| JJCommand::Fix {
            change_ids: ids,
            flags,
        }),
        AppAction::ResolveOurs | AppAction::ResolveTheirs | AppAction::ResolveMergeTool => {
            // Check if cursor is on a conflict hunk row (per-hunk resolution).
            if let Some(DisplayRow::ConflictHeader {
                entry_idx,
                file_idx,
                hunk_idx,
            })
            | Some(DisplayRow::ConflictSide {
                entry_idx,
                file_idx,
                hunk_idx,
                ..
            }) = app.rows.get(app.cursor)
            {
                let side = match action {
                    AppAction::ResolveOurs => 0,
                    AppAction::ResolveTheirs => 1,
                    _ => {
                        // Merge tool doesn't apply per-hunk.
                        app.set_status("merge tool is for whole-file only");
                        return Action::None;
                    }
                };
                app.pick_conflict_side(*entry_idx, *file_idx, *hunk_idx, side);
                return Action::None;
            }

            // Whole-file resolution from FileChange/DiffLine rows.
            let (entry_idx, file_idx) = match app.rows.get(app.cursor) {
                Some(DisplayRow::FileChange {
                    entry_idx,
                    file_idx,
                })
                | Some(DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    ..
                }) => (*entry_idx, *file_idx),
                _ => {
                    app.set_status("cursor must be on a file or conflict hunk");
                    return Action::None;
                }
            };
            let Some(file) = app
                .files_for_entry(entry_idx)
                .and_then(|f| f.get(file_idx.raw()))
            else {
                return Action::None;
            };
            if !file.has_conflict {
                app.set_status("no conflict on this file");
                return Action::None;
            }
            let change_id = app.change_id(entry_idx);
            let path = Str::from(file.path.as_str());
            let tool = match action {
                AppAction::ResolveOurs => crate::jj_command::ResolveTool::Ours,
                AppAction::ResolveTheirs => crate::jj_command::ResolveTool::Theirs,
                _ => crate::jj_command::ResolveTool::Default,
            };
            let cmd = JJCommand::Resolve {
                change_id,
                path,
                tool: tool.clone(),
                flags,
            };
            if matches!(tool, crate::jj_command::ResolveTool::Default) {
                Action::SuspendAndRunJj(cmd)
            } else {
                Action::RunJj(cmd)
            }
        }
        AppAction::ConflictPickOurs
        | AppAction::ConflictPickTheirs
        | AppAction::ConflictPickBase => {
            // Per-hunk conflict picking (also accessible via c→o/t on conflict rows).
            if let Some(DisplayRow::ConflictHeader {
                entry_idx,
                file_idx,
                hunk_idx,
            })
            | Some(DisplayRow::ConflictSide {
                entry_idx,
                file_idx,
                hunk_idx,
                ..
            }) = app.rows.get(app.cursor)
            {
                let side = match action {
                    AppAction::ConflictPickOurs => 0,
                    AppAction::ConflictPickTheirs => 1,
                    _ => 2, // base
                };
                app.pick_conflict_side(*entry_idx, *file_idx, *hunk_idx, side);
            }
            Action::None
        }
        AppAction::FileUntrack => {
            let paths = app.selected_file_paths();
            if paths.is_empty() {
                app.set_status("no files selected");
                return Action::None;
            }
            Action::RunJj(JJCommand::FileUntrack {
                paths: paths.into(),
                flags,
            })
        }
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
        AppAction::New => make_multi_command(app, |ids| JJCommand::New {
            change_ids: ids,
            insert: None,
            flags,
        }),
        AppAction::NewInsertAfter => make_command(app, |id| JJCommand::New {
            change_ids: smallvec![id],
            insert: Some(InsertPosition::After),
            flags,
        }),
        AppAction::NewInsertBefore => make_command(app, |id| JJCommand::New {
            change_ids: smallvec![id],
            insert: Some(InsertPosition::Before),
            flags,
        }),
        AppAction::Squash => {
            if app.selected_is_merge() {
                // Merge commits can't squash into parent without specifying which one.
                // Redirect to target selection (squash into).
                enter_target_select(app, TargetOperation::Squash(SquashKind::Into), flags)
            } else {
                let selection = build_change_selection(app);
                make_command(app, |id| JJCommand::Squash {
                    change_id: id,
                    target: None,
                    message: MessageMode::Default,
                    selection: selection.clone(),
                    flags,
                })
            }
        }
        AppAction::SquashSelect(kind) => {
            enter_target_select(app, TargetOperation::Squash(kind), flags)
        }
        // Rebase -- target selection (supports multi-commit via selection)
        AppAction::RebaseRevision => {
            let sources = app.selected_change_ids();
            if sources.is_empty() {
                return Action::None;
            }
            enter_target_select(
                app,
                TargetOperation::Rebase {
                    source_mode: RebaseSource::Revision,
                    sources,
                },
                flags,
            )
        }
        AppAction::RebaseSource => {
            let sources = app.selected_change_ids();
            if sources.is_empty() {
                return Action::None;
            }
            enter_target_select(
                app,
                TargetOperation::Rebase {
                    source_mode: RebaseSource::Source,
                    sources,
                },
                flags,
            )
        }
        AppAction::RebaseBranch => {
            let sources = app.selected_change_ids();
            if sources.is_empty() {
                return Action::None;
            }
            enter_target_select(
                app,
                TargetOperation::Rebase {
                    source_mode: RebaseSource::Branch,
                    sources,
                },
                flags,
            )
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
        AppAction::BookmarkTrack => {
            let bookmarks: Vec<String> = app
                .views
                .untracked_bookmarks
                .iter()
                .map(|s| s.to_string())
                .collect();
            enter_remote_bookmark_select(
                app,
                bookmarks,
                "no untracked remote bookmarks",
                "track bookmark",
                PendingSelection::BookmarkTrack { flags },
            )
        }
        AppAction::BookmarkUntrack => {
            let bookmarks: Vec<String> = app
                .views
                .tracked_bookmarks
                .iter()
                .map(|s| s.to_string())
                .collect();
            enter_remote_bookmark_select(
                app,
                bookmarks,
                "no tracked remote bookmarks",
                "untrack bookmark",
                PendingSelection::BookmarkUntrack { flags },
            )
        }

        AppAction::TagSet => {
            let Some(change_id) = app.selected_change_id() else {
                return Action::None;
            };
            app.mode =
                AppMode::text_input("set tag: ", "", PendingCommand::TagSet { change_id, flags });
            Action::None
        }
        AppAction::TagDelete => enter_tag_delete(app, flags),

        AppAction::Undo => Action::RunJj(JJCommand::Undo { flags }),
        AppAction::Redo => Action::RunJj(JJCommand::Redo { flags }),

        // Git commands (network ops suspend TUI for SSH auth / progress)
        AppAction::GitFetch => {
            if app.views.remotes.len() > 1 {
                let items = app.views.remotes.iter().map(|r| r.to_string()).collect();
                app.mode = AppMode::select_from_list(
                    "fetch from remote",
                    items,
                    false,
                    PendingSelection::GitRemoteForFetch {
                        all_remotes: false,
                        flags,
                    },
                    false,
                );
                Action::None
            } else {
                Action::SuspendAndRunJj(JJCommand::GitFetch {
                    all_remotes: false,
                    remote: None,
                    flags,
                })
            }
        }
        AppAction::GitFetchAllRemotes => Action::SuspendAndRunJj(JJCommand::GitFetch {
            all_remotes: true,
            remote: None,
            flags,
        }),
        AppAction::GitPush => {
            if app.views.remotes.len() > 1 {
                let items = app.views.remotes.iter().map(|r| r.to_string()).collect();
                app.mode = AppMode::select_from_list(
                    "push to remote",
                    items,
                    false,
                    PendingSelection::GitRemoteForPush { all: false, flags },
                    false,
                );
                Action::None
            } else {
                Action::SuspendAndRunJj(JJCommand::GitPush {
                    all: false,
                    remote: None,
                    flags,
                })
            }
        }
        AppAction::GitPushAll => {
            if app.views.remotes.len() > 1 {
                let items = app.views.remotes.iter().map(|r| r.to_string()).collect();
                app.mode = AppMode::select_from_list(
                    "push all to remote",
                    items,
                    false,
                    PendingSelection::GitRemoteForPush { all: true, flags },
                    false,
                );
                Action::None
            } else {
                Action::SuspendAndRunJj(JJCommand::GitPush {
                    all: true,
                    remote: None,
                    flags,
                })
            }
        }
        AppAction::GitPushChange => {
            let Some(change_id) = app.selected_change_id() else {
                return Action::None;
            };
            Action::SuspendAndRunJj(JJCommand::GitPushChange {
                change_id,
                remote: None,
                flags,
            })
        }
        AppAction::GitPushBookmark => {
            let bookmarks = app.selected_bookmarks().unwrap_or(&[]);
            if bookmarks.is_empty() {
                app.set_status("no bookmarks on this commit");
                return Action::None;
            }
            let items: Vec<String> = bookmarks.iter().map(|b| b.name.to_string()).collect();
            if items.len() == 1 {
                let bookmark_names: SmallVec<BookmarkName> =
                    bookmarks.iter().map(|b| b.name.clone()).collect();
                if app.views.remotes.len() > 1 {
                    let remote_items = app.views.remotes.iter().map(|r| r.to_string()).collect();
                    app.mode = AppMode::select_from_list(
                        "push bookmark to remote",
                        remote_items,
                        true,
                        PendingSelection::GitRemoteForPushBookmark {
                            bookmarks: bookmark_names,
                            flags,
                        },
                        false,
                    );
                    return Action::None;
                }
                return Action::SuspendAndRunJj(JJCommand::GitPushBookmark {
                    bookmarks: bookmark_names,
                    remote: None,
                    flags,
                });
            }
            app.mode = AppMode::select_from_list(
                "push bookmark",
                items,
                true,
                PendingSelection::GitPushBookmark { flags },
                false,
            );
            Action::None
        }
        AppAction::GitExport => Action::RunJj(JJCommand::GitExport { flags }),
        AppAction::GitImport => Action::RunJj(JJCommand::GitImport { flags }),

        // Duplicate
        AppAction::Duplicate => make_multi_command(app, |ids| JJCommand::Duplicate {
            change_ids: ids,
            onto: None,
            flags,
        }),
        AppAction::DuplicateOnto => enter_target_select(app, TargetOperation::DuplicateOnto, flags),

        // Parallelize / Simplify parents / Revert
        AppAction::Parallelize => make_multi_command(app, |ids| JJCommand::Parallelize {
            change_ids: ids,
            flags,
        }),
        AppAction::SimplifyParents => make_multi_command(app, |ids| JJCommand::SimplifyParents {
            change_ids: ids,
            flags,
        }),
        AppAction::Revert => {
            let sources = app.selected_change_ids();
            enter_target_select(app, TargetOperation::Revert { sources }, flags)
        }
        AppAction::SwitchToDagView => {
            app.switch_view(crate::app::ActiveView::Dag);
            Action::None
        }
        AppAction::SwitchToBookmarkView => {
            app.switch_view(crate::app::ActiveView::Bookmarks);
            Action::None
        }
        // Bookmark view actions
        AppAction::BmViewDelete => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            Action::RunJj(JJCommand::BookmarkDelete {
                names: smallvec![name],
                flags,
            })
        }
        AppAction::BmViewTrack => {
            let Some(br) = app.selected_bookmark_ref() else {
                app.set_status("bookmark is already local");
                return Action::None;
            };
            Action::RunJj(JJCommand::BookmarkTrack {
                bookmarks: smallvec![br],
                flags,
            })
        }
        AppAction::BmViewUntrack => {
            let Some(br) = app.selected_bookmark_ref() else {
                app.set_status("bookmark has no remote to untrack");
                return Action::None;
            };
            Action::RunJj(JJCommand::BookmarkUntrack {
                bookmarks: smallvec![br],
                flags,
            })
        }
        AppAction::BmViewPush => {
            // On a remote target row: push to that specific remote.
            if let Some((entry, target)) = app.selected_remote_target() {
                let name = entry.name.clone();
                let remote = Str::from(target.remote.as_str());
                return Action::SuspendAndRunJj(JJCommand::GitPushBookmark {
                    bookmarks: smallvec![name],
                    remote: Some(remote),
                    flags,
                });
            }
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            Action::SuspendAndRunJj(JJCommand::GitPushBookmark {
                bookmarks: smallvec![name],
                remote: None,
                flags,
            })
        }
        AppAction::BmViewJumpToCommit => {
            // On a conflict target row: resolve conflict by picking this side.
            if let Some((entry, target)) = app.selected_conflict_target() {
                let name = entry.name.clone();
                let prefix = &target.change_id.display[..target
                    .change_id
                    .prefix_len
                    .min(target.change_id.display.len())];
                let change_id = match target.change_id_suffix {
                    Some(suffix) => ChangeId::new(format!("{prefix}/{suffix}")),
                    None => ChangeId::new(prefix),
                };
                return Action::RunJj(JJCommand::BookmarkSet {
                    name,
                    change_id,
                    flags: flags | CommandFlags::ALLOW_BACKWARDS,
                });
            }
            // On a remote target row: jump to that specific commit in DAG.
            if let Some(ids) = app
                .selected_remote_target()
                .map(|(_, t)| (t.commit_id.clone(), t.change_id.clone()))
            {
                jump_to_commit_in_dag(app, Some(&ids.0), Some(&ids.1), "");
                return Action::None;
            }
            // On a bookmark row: jump to the bookmark's commit in DAG.
            let Some(ids) = app
                .selected_bookmark_entry()
                .map(|e| (e.commit_id.clone(), e.change_id.clone()))
            else {
                return Action::None;
            };
            jump_to_commit_in_dag(
                app,
                ids.0.as_ref(),
                ids.1.as_ref(),
                "bookmark has no associated commit",
            );
            Action::None
        }
        AppAction::BmViewEdit => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let Some(change_id) = entry.change_id.as_ref().map(|s| ChangeId::new(&s.display))
            else {
                app.set_error("bookmark has no associated commit");
                return Action::None;
            };
            Action::RunJj(JJCommand::Edit { change_id, flags })
        }
        AppAction::BmViewRename => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let old_name = entry.name.clone();
            app.mode = AppMode::text_input(
                "rename to: ",
                entry.name.as_str(),
                PendingCommand::BookmarkRename { old_name, flags },
            );
            Action::None
        }
        AppAction::BmViewMove => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let bookmark_name = entry.name.clone();
            app.switch_view(crate::app::ActiveView::Dag);
            enter_target_select(app, TargetOperation::BookmarkMove { bookmark_name }, flags)
        }
        AppAction::BmViewForget => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            Action::RunJj(JJCommand::BookmarkForget {
                names: smallvec![name],
                flags,
            })
        }
        AppAction::BmViewSet => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            app.mode = AppMode::text_input(
                format!("set {} to (change id): ", name),
                "",
                PendingCommand::BookmarkSetByName { name, flags },
            );
            Action::None
        }
        AppAction::BmViewFetch => {
            // On a remote target row: fetch that specific bookmark+remote.
            if let Some((entry, target)) = app.selected_remote_target() {
                return Action::SuspendAndRunJj(JJCommand::GitFetchBookmark {
                    bookmark: entry.name.clone(),
                    remote: Str::from(target.remote.as_str()),
                    flags,
                });
            }
            // On a bookmark row: fetch from all remotes.
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            if let Some(remote) = &entry.remote {
                // Remote bookmark row — fetch from that remote.
                return Action::SuspendAndRunJj(JJCommand::GitFetchBookmark {
                    bookmark: entry.name.clone(),
                    remote: Str::from(remote.as_str()),
                    flags,
                });
            }
            // Local bookmark — fetch from all remotes for this bookmark.
            Action::SuspendAndRunJj(JJCommand::GitFetch {
                all_remotes: false,
                remote: None,
                flags,
            })
        }
        // Tag view actions
        AppAction::SwitchToTagView => {
            app.switch_view(crate::app::ActiveView::Tags);
            Action::None
        }
        AppAction::TgViewDelete => {
            let Some(entry) = app.selected_tag_entry() else {
                return Action::None;
            };
            let name = entry.name.to_string();
            Action::RunJj(JJCommand::TagDelete {
                names: smallvec![name],
                flags,
            })
        }
        AppAction::TgViewSet => {
            let Some(entry) = app.selected_tag_entry() else {
                return Action::None;
            };
            let name = entry.name.to_string();
            app.mode = AppMode::text_input(
                format!("set {name} to (change id): "),
                "",
                PendingCommand::TagSetByName { name, flags },
            );
            Action::None
        }
        AppAction::TgViewJumpToCommit => {
            let Some(ids) = app
                .selected_tag_entry()
                .map(|e| (e.commit_id.clone(), e.change_id.clone()))
            else {
                return Action::None;
            };
            jump_to_commit_in_dag(
                app,
                ids.0.as_ref(),
                ids.1.as_ref(),
                "tag has no associated commit",
            );
            Action::None
        }
        // Operations view actions
        AppAction::SwitchToOpLogView => {
            app.switch_view(crate::app::ActiveView::Operations);
            Action::None
        }
        AppAction::OpLogFilterWorkspace => {
            // Collect unique workspace names from op log entries.
            let mut workspaces: Vec<String> = app
                .op_log
                .entries
                .iter()
                .filter_map(|e| e.workspace.as_ref().map(|w| w.to_string()))
                .collect();
            workspaces.sort();
            workspaces.dedup();
            if workspaces.is_empty() {
                app.set_status("no workspace info in operation log");
                return Action::None;
            }
            app.mode = AppMode::select_from_list(
                "filter by workspace",
                workspaces,
                true,
                PendingSelection::OpLogWorkspaceFilter,
                false,
            );
            Action::None
        }
        AppAction::OpLogRestore => {
            let Some(entry) = app.selected_op_log_entry() else {
                return Action::None;
            };
            let op_id = entry.id.to_string();
            Action::RunJj(JJCommand::OpRestore { op_id, flags })
        }
        AppAction::OpLogRevert => {
            let Some(entry) = app.selected_op_log_entry() else {
                return Action::None;
            };
            let op_id = entry.id.to_string();
            Action::RunJj(JJCommand::OpRevert { op_id, flags })
        }
        AppAction::OpLogAbandon => {
            let Some(entry) = app.selected_op_log_entry() else {
                return Action::None;
            };
            let op_id = entry.id.to_string();
            Action::RunJj(JJCommand::OpAbandon { op_id, flags })
        }
        // Workspace view actions
        AppAction::SwitchToWorkspaceView => {
            app.switch_view(crate::app::ActiveView::Workspaces);
            Action::None
        }
        AppAction::SwitchToEvoLogView => {
            app.switch_view(crate::app::ActiveView::Evolog);
            Action::None
        }
        AppAction::EvoLogEdit => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            let change_id = ChangeId::new(entry.commit_id.as_str());
            Action::RunJj(JJCommand::Edit { change_id, flags })
        }
        AppAction::EvoLogNew => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            let change_id = ChangeId::new(entry.commit_id.as_str());
            Action::RunJj(JJCommand::New {
                change_ids: smallvec![change_id],
                insert: None,
                flags,
            })
        }
        AppAction::EvoLogRestore => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            if entry.is_current {
                app.set_status("already on current version");
                return Action::None;
            }
            let from = ChangeId::new(entry.commit_id.as_str());
            let into = app
                .evolog
                .entries
                .iter()
                .find(|e| e.is_current)
                .map(|e| ChangeId::new(e.commit_id.as_str()));
            Action::RunJj(JJCommand::Restore {
                from: Some(from),
                into,
                changes_in: None,
                selection: build_change_selection(app),
                flags,
            })
        }
        AppAction::WsViewForget => {
            let Some(entry) = app.selected_workspace_entry() else {
                return Action::None;
            };
            let name = entry.name.to_string();
            Action::RunJj(JJCommand::WorkspaceForget {
                names: smallvec![name],
                flags,
            })
        }
        AppAction::WsViewJumpToCommit => {
            let Some(entry) = app.selected_workspace_entry() else {
                return Action::None;
            };
            let commit_id = entry.commit_id.clone();
            let change_id = entry.change_id.clone();
            jump_to_commit_in_dag(
                app,
                commit_id.as_ref(),
                change_id.as_ref(),
                "workspace has no associated commit",
            );
            Action::None
        }
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
/// Try to jump to a commit in the DAG view. If the commit is in the current
/// revset, switches to DAG and moves the cursor. Otherwise offers to widen
/// the revset. If there's no commit at all, shows an error.
fn jump_to_commit_in_dag(
    app: &mut App,
    commit_id: Option<&CommitId>,
    change_id: Option<&crate::dag::ShortId>,
    missing_msg: &str,
) {
    let Some(cid) = commit_id else {
        app.set_error(missing_msg);
        return;
    };
    if let Some(idx) = app.entry_by_commit_id(cid) {
        app.switch_view(crate::app::ActiveView::Dag);
        if let Some(row) = app.row_of_commit(idx) {
            app.cursor = row;
        }
    } else {
        offer_widen_revset(app, change_id);
    }
}

/// Show a FollowUp prompt offering to widen the revset to include a change.
fn offer_widen_revset(app: &mut App, change_id: Option<&crate::dag::ShortId>) {
    if let Some(cid) = change_id {
        app.mode = AppMode::FollowUp {
            prompt: "commit not in current revset".into(),
            options: vec![FollowUpOption {
                key: 'w',
                label: "widen revset",
                action: FollowUpAction::WidenRevset {
                    change_id: cid.display.clone(),
                },
            }],
        };
    } else {
        app.set_status("commit not in current revset");
    }
}

fn execute_follow_up(app: &mut App, action: FollowUpAction) -> Action {
    match action {
        FollowUpAction::Execute(cmd) => run_cmd(cmd),
        FollowUpAction::TextInput { prompt, pending } => {
            app.mode = AppMode::text_input(prompt, "", pending);
            Action::None
        }
        FollowUpAction::WidenRevset { change_id } => {
            let new_revset = format!("({}) | {}", app.revset.current, change_id);
            app.jump_after_refresh = Some(crate::app::JumpTarget::ChangeId(change_id));
            app.switch_view(crate::app::ActiveView::Dag);
            app.revset.active_preset = None;
            Action::UpdateRevset(new_revset)
        }
    }
}

/// Helper: build a command from multiple selected change IDs (or cursor fallback).
fn make_multi_command(app: &App, build: impl FnOnce(SmallVec<ChangeId>) -> JJCommand) -> Action {
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
    let ids = app.selected_change_ids();
    if ids.is_empty() {
        return Action::None;
    }
    // Prefill with existing description only for single-commit describe.
    let prefill = if ids.len() == 1 {
        app.selected_description().unwrap_or("").to_string()
    } else {
        String::new()
    };
    app.mode = AppMode::text_input(
        "describe: ",
        prefill,
        PendingCommand::Describe {
            change_ids: ids,
            flags,
        },
    );
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
                    PendingCommand::Revset => {
                        app.revset.active_preset = None;
                        Action::UpdateRevset(text)
                    }
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
                        // CommitSelect needs the DAG view to navigate commits.
                        if app.active_view != crate::app::ActiveView::Dag {
                            app.switch_view(crate::app::ActiveView::Dag);
                        }
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
                    KeyCode::Char(ch) => {
                        let specs = crate::types::scope_specs_for_view(app.active_view);
                        if let Some(spec) = specs.iter().find(|s| s.hint.starts_with(ch)) {
                            app.toggle_search_scope(spec.flag);
                        }
                    }
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
    let target_mode = if operation.multi_target() {
        TargetMode::Multi {
            targets: HashSet::new(),
        }
    } else {
        TargetMode::Single
    };
    app.mode = AppMode::TargetSelect {
        prompt: operation.label(),
        source,
        restore_cursor,
        operation,
        flags,
        target_mode,
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
            app.page_down(PAGE_SIZE);
            Some(Action::None)
        }
        (Key::Char('u'), _, true) => {
            app.page_up(PAGE_SIZE);
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
            app.page_down(PAGE_SIZE);
            Some(Action::None)
        }
        (Key::PageUp, _, _) => {
            app.page_up(PAGE_SIZE);
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
            let old_mode = std::mem::replace(&mut app.mode, AppMode::Help { scroll: 0 });
            app.pre_overlay_mode = Some(old_mode);
            Some(Action::None)
        }
        _ => None,
    }
}

fn handle_target_select(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char(' ') => {
            // Toggle target in multi-select mode.
            let id = app.selected_change_id();
            if let (
                Some(id),
                AppMode::TargetSelect {
                    target_mode: TargetMode::Multi { targets },
                    ..
                },
            ) = (id, &mut app.mode)
            {
                if !targets.remove(&id) {
                    targets.insert(id);
                }
            }
            Action::None
        }
        KeyCode::Enter => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::TargetSelect {
                source,
                operation,
                flags,
                target_mode,
                ..
            } = mode
            {
                let targets = match target_mode {
                    TargetMode::Multi { targets } if !targets.is_empty() => {
                        targets.into_iter().collect()
                    }
                    _ => {
                        let Some(target) = app.selected_change_id() else {
                            return Action::None;
                        };
                        smallvec![target]
                    }
                };
                let label = operation.label();
                let selection = build_change_selection(app);
                let mut options = operation.follow_up(source, targets.clone(), flags, selection);
                if options.len() == 1 {
                    let opt = options.remove(0);
                    return execute_follow_up(app, opt.action);
                }
                let target_str: String = targets
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let prompt = format!("{label} {target_str}:");
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
    bookmarks: Vec<String>,
    empty_msg: &str,
    title: &str,
    on_select: PendingSelection,
) -> Action {
    if bookmarks.is_empty() {
        app.set_status(empty_msg);
        return Action::None;
    }
    app.mode = AppMode::select_from_list(title, bookmarks, true, on_select, true);
    Action::None
}

enum BookmarkTextAction {
    Create,
    Set,
}

#[derive(Clone, Copy)]
enum PendingSelectionKind {
    Delete,
    Forget,
    Move,
    Rename,
}

impl PendingSelectionKind {
    fn title(self) -> &'static str {
        match self {
            Self::Delete => "delete bookmark",
            Self::Forget => "forget bookmark",
            Self::Move => "move bookmark",
            Self::Rename => "rename bookmark",
        }
    }

    fn to_pending(self, change_id: ChangeId, flags: CommandFlags) -> PendingSelection {
        match self {
            Self::Delete => PendingSelection::BookmarkDelete { change_id, flags },
            Self::Forget => PendingSelection::BookmarkForget { change_id, flags },
            Self::Move => PendingSelection::BookmarkMove { change_id, flags },
            Self::Rename => PendingSelection::BookmarkRename { change_id, flags },
        }
    }

    fn is_multi(self) -> bool {
        matches!(self, Self::Delete | Self::Forget)
    }
}

fn enter_bookmark_advance(app: &mut App, flags: CommandFlags) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };

    // Check if the selected commit is the working copy.
    let is_wc = app.selected_bookmarks().is_some_and(|_| {
        // Check via the entries
        let entry_idx = match app.rows.get(app.cursor) {
            Some(DisplayRow::CommitNode { entry_idx })
            | Some(DisplayRow::DescriptionLine { entry_idx, .. })
            | Some(DisplayRow::GraphLink { entry_idx, .. })
            | Some(DisplayRow::FileChange { entry_idx, .. })
            | Some(DisplayRow::DiffLine { entry_idx, .. }) => Some(*entry_idx),
            Some(DisplayRow::BookmarkItem { .. })
            | Some(DisplayRow::BookmarkConflictTarget { .. })
            | Some(DisplayRow::BookmarkRemoteTarget { .. })
            | Some(DisplayRow::TagItem { .. })
            | Some(DisplayRow::TagRemoteTarget { .. })
            | Some(DisplayRow::OpLogItem { .. })
            | Some(DisplayRow::OpLogDetailLine { .. })
            | Some(DisplayRow::OpLogGraphLink { .. })
            | Some(DisplayRow::OpLogLoadMore)
            | Some(DisplayRow::EvoLogItem { .. })
            | Some(DisplayRow::EvoLogFileChange { .. })
            | Some(DisplayRow::EvoLogFileDiffLine { .. })
            | Some(DisplayRow::EvoLogGraphLink { .. })
            | Some(DisplayRow::WorkspaceItem { .. })
            | Some(DisplayRow::ConflictHeader { .. })
            | Some(DisplayRow::ConflictSide { .. })
            | Some(DisplayRow::ConflictContext { .. })
            | None => None,
        };
        entry_idx.is_some_and(|idx| app.nodes[idx].commit.is_working_copy())
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
        app.set_status("no bookmarks on this commit");
        return Action::None;
    }

    let on_select = kind.to_pending(change_id, flags);
    let items: Vec<String> = bookmarks.iter().map(|b| b.name.to_string()).collect();

    if items.len() == 1 {
        return resolve_selection(app, on_select, items.into());
    }

    app.mode = AppMode::select_from_list(kind.title(), items, kind.is_multi(), on_select, false);
    Action::None
}

fn enter_tag_delete(app: &mut App, flags: CommandFlags) -> Action {
    let tags = app.selected_tags().unwrap_or(&[]);
    if tags.is_empty() {
        app.set_status("no tags on this commit");
        return Action::None;
    }
    let items: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
    if items.len() == 1 {
        return Action::RunJj(JJCommand::TagDelete {
            names: items.into(),
            flags,
        });
    }
    let on_select = PendingSelection::TagDelete { flags };
    app.mode = AppMode::select_from_list("delete tag", items, true, on_select, false);
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

/// Move the list cursor by `delta` rows (positive = down, negative = up).
fn list_move(app: &mut App, delta: isize) {
    if let AppMode::SelectFromList {
        cursor,
        filtered_indices,
        ..
    } = &mut app.mode
    {
        let max = filtered_indices.len().saturating_sub(1);
        *cursor = (*cursor as isize + delta).clamp(0, max as isize) as usize;
    }
}

/// Jump the list cursor to start (false) or end (true).
fn list_jump(app: &mut App, to_end: bool) {
    if let AppMode::SelectFromList {
        cursor,
        filtered_indices,
        ..
    } = &mut app.mode
    {
        *cursor = if to_end {
            filtered_indices.len().saturating_sub(1)
        } else {
            0
        };
    }
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
            // Tab, Esc, Enter, arrows, page keys, and ctrl-n/p fall through to the
            // main match below so list navigation works while filtering.
            _ if matches!(
                node_key,
                Some(Key::Tab)
                    | Some(Key::Esc)
                    | Some(Key::Enter)
                    | Some(Key::Up)
                    | Some(Key::Down)
                    | Some(Key::PageUp)
                    | Some(Key::PageDown)
            ) => {}
            _ if matches!(node_key, Some(Key::Char('n') | Key::Char('p'))) && ctrl => {}
            // All other keys are swallowed while filtering.
            _ => return Action::None,
        }
    }

    match node_key {
        Some(Key::Char('n')) if ctrl => {
            list_move(app, 1);
            Action::None
        }
        Some(Key::Char('p')) if ctrl => {
            list_move(app, -1);
            Action::None
        }
        Some(Key::Char('j')) | Some(Key::Down) => {
            list_move(app, 1);
            Action::None
        }
        Some(Key::Char('k')) | Some(Key::Up) => {
            list_move(app, -1);
            Action::None
        }
        Some(Key::Char('d')) if ctrl => {
            list_move(app, PAGE_SIZE as isize);
            Action::None
        }
        Some(Key::Char('u')) if ctrl => {
            list_move(app, -(PAGE_SIZE as isize));
            Action::None
        }
        Some(Key::PageDown) => {
            list_move(app, PAGE_SIZE as isize);
            Action::None
        }
        Some(Key::PageUp) => {
            list_move(app, -(PAGE_SIZE as isize));
            Action::None
        }
        Some(Key::Char('0')) => {
            list_jump(app, false);
            Action::None
        }
        Some(Key::Char('$')) => {
            list_jump(app, true);
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
                let names: Vec<String> = if multi && !marked.is_empty() {
                    let mut indices: Vec<usize> = marked.into_iter().collect();
                    indices.sort();
                    indices
                        .into_iter()
                        .filter_map(|i| items.get(i).cloned())
                        .collect()
                } else {
                    let orig_idx = filtered_indices.get(cursor).copied().unwrap_or(0);
                    vec![items.into_iter().nth(orig_idx).unwrap_or_default()]
                };
                resolve_selection(app, on_select, names.into())
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

/// After item(s) have been selected from a list, decide what to do next.
fn resolve_selection(
    app: &mut App,
    on_select: PendingSelection,
    names: SmallVec<String>,
) -> Action {
    match on_select {
        PendingSelection::BookmarkDelete { flags, .. } => {
            let names = names.into_iter().map(BookmarkName::new).collect();
            Action::RunJj(JJCommand::BookmarkDelete { names, flags })
        }
        PendingSelection::BookmarkForget { flags, .. } => {
            let names = names.into_iter().map(BookmarkName::new).collect();
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
            let bookmarks: SmallVec<BookmarkName> =
                names.into_iter().map(BookmarkName::new).collect();
            if app.views.remotes.len() > 1 {
                let items = app.views.remotes.iter().map(|r| r.to_string()).collect();
                app.mode = AppMode::select_from_list(
                    "push bookmark to remote",
                    items,
                    false,
                    PendingSelection::GitRemoteForPushBookmark { bookmarks, flags },
                    false,
                );
                Action::None
            } else {
                Action::SuspendAndRunJj(JJCommand::GitPushBookmark {
                    bookmarks,
                    remote: None,
                    flags,
                })
            }
        }
        PendingSelection::GitRemoteForFetch { all_remotes, flags } => {
            let remote = names.into_iter().next().map(Str::from);
            Action::SuspendAndRunJj(JJCommand::GitFetch {
                all_remotes,
                remote,
                flags,
            })
        }
        PendingSelection::GitRemoteForPush { all, flags } => {
            let remote = names.into_iter().next().map(Str::from);
            Action::SuspendAndRunJj(JJCommand::GitPush { all, remote, flags })
        }
        PendingSelection::GitRemoteForPushBookmark { bookmarks, flags } => {
            let remote = names.into_iter().next().map(Str::from);
            Action::SuspendAndRunJj(JJCommand::GitPushBookmark {
                bookmarks,
                remote,
                flags,
            })
        }
        PendingSelection::TagDelete { flags } => {
            Action::RunJj(JJCommand::TagDelete { names, flags })
        }
        // Single-item operations: take the first name.
        PendingSelection::BookmarkMove {
            change_id, flags, ..
        } => {
            let name = BookmarkName::new(names.into_iter().next().unwrap_or_default());
            app.mode = AppMode::TargetSelect {
                prompt: "move bookmark",
                source: change_id,
                restore_cursor: app.cursor,
                operation: TargetOperation::BookmarkMove {
                    bookmark_name: name,
                },
                flags,
                target_mode: TargetMode::Single,
            };
            Action::None
        }
        PendingSelection::BookmarkRename { flags, .. } => {
            let name = names.into_iter().next().unwrap_or_default();
            app.mode = AppMode::text_input(
                "rename to: ",
                name.clone(),
                PendingCommand::BookmarkRename {
                    old_name: BookmarkName::new(name),
                    flags,
                },
            );
            Action::None
        }
        PendingSelection::PresetSelect => {
            let name = names.into_iter().next().unwrap_or_default();
            let idx = app.revset.presets.iter().position(|p| p.name == name);
            if let Some(i) = idx {
                app.revset.active_preset = Some(i);
                Action::UpdateRevset(app.revset.presets[i].revset.clone())
            } else {
                Action::None
            }
        }
        PendingSelection::OpLogWorkspaceFilter => {
            app.op_log.workspace_filter = names.into_iter().map(Str::from).collect();
            app.rebuild_rows();
            Action::None
        }
    }
}

/// Parse `"name@remote"` display strings into `BookmarkRef` values.
fn parse_remote_bookmarks(names: SmallVec<String>) -> SmallVec<BookmarkRef> {
    names
        .into_iter()
        .filter_map(|s| {
            let (name, remote) = s.rsplit_once('@')?;
            Some(BookmarkRef {
                name: BookmarkName::new(name),
                remote: crate::types::RemoteName::new(remote),
            })
        })
        .collect()
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
                        AppMode::TargetSelect { .. } => handle_target_select(app, enter),
                        AppMode::CommitSelect { .. } => handle_commit_select(app, enter),
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
        | AppMode::SelectFromList { .. } => Action::None,
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
