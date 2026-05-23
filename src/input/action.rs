use std::collections::HashSet;

use smallvec::smallvec;

use crate::app::{App, AppMode, TargetMode};
use crate::dag::DiffLineKind;
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
use crate::jj_command::{InsertPosition, JJCommand, JJCommandKind};
use crate::keymap::{
    self, action_label, ActionId, ActionRegistry, AppAction, CommandFlags, Keymap, LookupResult,
    TrieNode,
};
use crate::types::ChangeSelection;
use crate::types::{
    BookmarkName, ChangeId, CommitId, DisplayRow, FollowUpAction, FollowUpOption, MessageMode,
    PendingCommand, PendingSelection, RebaseSource, RemoteName, SelectionKind, SmallVec, SplitKind,
    SquashKind, Str, TargetOperation, WorkspaceName,
};

use super::bookmark::{
    enter_bookmark_advance, enter_bookmark_select, enter_bookmark_text_input,
    enter_remote_bookmark_select, enter_tag_delete, BookmarkTextAction, PendingSelectionKind,
};
use super::Action;

/// Number of rows to jump for page-up/page-down style navigation.
use super::PAGE_SIZE;

/// Build the appropriate `ChangeSelection` from the current app state.
pub(super) fn build_change_selection(app: &App) -> ChangeSelection {
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

pub(super) fn handle_normal_key(
    app: &mut App,
    registry: &ActionRegistry,
    lua: &crate::lua::LuaEngine,
    keymap: &Keymap,
    node: &keymap_parser::Node,
) -> Action {
    if node.key == keymap_parser::Key::Esc && app.search.is_some() {
        app.clear_search();
        return Action::None;
    }

    match keymap.lookup(node) {
        LookupResult::Action(ActionId::Builtin(action)) => {
            app.status_message = None;
            dispatch_action(app, registry, action, CommandFlags::empty())
        }
        LookupResult::Action(ActionId::Lua(id)) => {
            app.status_message = None;
            lua.execute_command(id, app, CommandFlags::empty())
        }
        LookupResult::Prefix { label, children } => {
            app.status_message = None;
            if app.selection_active() {
                let kind = app.selection_kind();
                let has_supported_action = children.iter().any(|(_, n)| match n {
                    TrieNode::Action { id, .. } => {
                        registry.selection_support(*id).contains(kind.as_bitset())
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
        LookupResult::Toggle(_) => Action::None,
        LookupResult::Unbound => {
            app.set_error(format!("unknown key: {}", keymap::display_key(node)));
            Action::None
        }
    }
}

pub(super) fn handle_submenu_key(
    app: &mut App,
    registry: &ActionRegistry,
    lua: &crate::lua::LuaEngine,
    children: &[(keymap_parser::Node, TrieNode)],
    flags: CommandFlags,
    node: &keymap_parser::Node,
) -> Action {
    if node.key == keymap_parser::Key::Esc {
        app.mode = AppMode::Normal;
        return Action::None;
    }

    let result = Keymap::lookup_in(children, node);

    match result {
        LookupResult::Action(ActionId::Builtin(action)) => {
            app.mode = AppMode::Normal;
            dispatch_action(app, registry, action, flags)
        }
        LookupResult::Action(ActionId::Lua(id)) => {
            app.mode = AppMode::Normal;
            lua.execute_command(id, app, flags)
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

fn dispatch_action(
    app: &mut App,
    registry: &ActionRegistry,
    action: AppAction,
    flags: CommandFlags,
) -> Action {
    // Merge global toggles into the command flags.
    let flags = flags | app.toggles;

    if app.selection_active()
        && !registry
            .selection_support(ActionId::Builtin(action))
            .contains(app.selection_kind().as_bitset())
    {
        let kind = app.selection_kind();
        let kind_label = match kind {
            SelectionKind::Commit => "commit",
            SelectionKind::File => "file",
            SelectionKind::Line => "line",
        };
        app.set_error(format!(
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
        AppAction::MoveToScreenTop => {
            app.move_to_screen_top();
            Action::None
        }
        AppAction::MoveToScreenMiddle => {
            app.move_to_screen_middle();
            Action::None
        }
        AppAction::MoveToScreenBottom => {
            app.move_to_screen_bottom();
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
        AppAction::ToggleGitDiff => {
            app.diff_format = match app.diff_format {
                crate::app::DiffFormat::Git => crate::app::DiffFormat::ColorWords,
                crate::app::DiffFormat::ColorWords => crate::app::DiffFormat::Git,
            };
            app.rebuild_rows();
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
            let target = match app.rows.get(app.cursor.raw()) {
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
                app.set_error("no presets configured");
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
            let workspaces: Vec<WorkspaceName> = entry_idx
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
                Action::RunJj(JJCommand {
                    kind: JJCommandKind::WorkspaceForget {
                        names: workspaces.into(),
                    },
                    flags,
                })
            } else if workspaces.len() > 1 {
                let items: Vec<String> = workspaces.iter().map(|w| w.to_string()).collect();
                app.mode = AppMode::select_from_list(
                    "forget workspace",
                    items,
                    true,
                    PendingSelection::WorkspaceForget { flags },
                    false,
                );
                Action::None
            } else {
                app.set_error("no other workspace on this commit");
                Action::None
            }
        }
        AppAction::WorkspaceList => Action::RunJj(JJCommand {
            kind: JJCommandKind::WorkspaceList,
            flags,
        }),
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
        AppAction::Abandon => make_multi_command(app, |ids| JJCommand {
            kind: JJCommandKind::Abandon { change_ids: ids },
            flags,
        }),
        AppAction::Absorb => make_command(app, |id| JJCommand {
            kind: JJCommandKind::Absorb {
                from: Some(id),
                selection: build_change_selection(app),
            },
            flags,
        }),
        AppAction::ExpandAncestors => {
            let Some(entry_idx) = app.selected_entry_idx() else {
                return Action::None;
            };
            app.expand_ancestors(entry_idx);
            Action::None
        }
        AppAction::Fix => {
            let selection = build_change_selection(app);
            make_multi_command(app, |ids| JJCommand {
                kind: JJCommandKind::Fix {
                    change_ids: ids,
                    selection: selection.clone(),
                },
                flags,
            })
        }
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
            }) = app.rows.get(app.cursor.raw())
            {
                let side = match action {
                    AppAction::ResolveOurs => 0,
                    AppAction::ResolveTheirs => 1,
                    _ => {
                        // Merge tool doesn't apply per-hunk.
                        app.set_error("merge tool is for whole-file only");
                        return Action::None;
                    }
                };
                let result = app.pick_conflict_side(*entry_idx, *file_idx, *hunk_idx, side);
                return write_conflict_resolution(app, result);
            }

            // Whole-file resolution from FileChange/DiffLine rows.
            let (entry_idx, file_idx) = match app.rows.get(app.cursor.raw()) {
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
                    app.set_error("cursor must be on a file or conflict hunk");
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
                app.set_error("no conflict on this file");
                return Action::None;
            }
            let change_id = app.change_id(entry_idx);
            let path = Str::from(file.path.as_str());
            let tool = match action {
                AppAction::ResolveOurs => crate::jj_command::ResolveTool::Ours,
                AppAction::ResolveTheirs => crate::jj_command::ResolveTool::Theirs,
                _ => crate::jj_command::ResolveTool::Default,
            };
            let cmd = JJCommand {
                kind: JJCommandKind::Resolve {
                    change_id,
                    path,
                    tool: tool.clone(),
                },
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
            }) = app.rows.get(app.cursor.raw())
            {
                let side = match action {
                    AppAction::ConflictPickOurs => 0,
                    AppAction::ConflictPickTheirs => 1,
                    _ => 2, // base
                };
                let result = app.pick_conflict_side(*entry_idx, *file_idx, *hunk_idx, side);
                let action = write_conflict_resolution(app, result);
                if matches!(action, Action::Refresh) {
                    return action;
                }
            } else {
                app.set_error("per-hunk only — use on a conflict hunk row");
            }
            Action::None
        }
        AppAction::FileUntrack => {
            let paths = app.selected_file_paths();
            if paths.is_empty() {
                app.set_error("no files selected");
                return Action::None;
            }
            Action::RunJj(JJCommand {
                kind: JJCommandKind::FileUntrack {
                    paths: paths.into(),
                },
                flags,
            })
        }
        AppAction::Commit => {
            let cmd = JJCommand {
                kind: JJCommandKind::Commit {
                    message: None,
                    selection: build_change_selection(app),
                },
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
        AppAction::DescribeInEditor => make_command(app, |id| JJCommand {
            kind: JJCommandKind::DescribeInEditor { change_id: id },
            flags,
        }),
        AppAction::Edit => make_command(app, |id| JJCommand {
            kind: JJCommandKind::Edit { change_id: id },
            flags,
        }),
        AppAction::New => make_multi_command(app, |ids| JJCommand {
            kind: JJCommandKind::New {
                change_ids: ids,
                insert: None,
            },
            flags,
        }),
        AppAction::NewInsertAfter => make_command(app, |id| JJCommand {
            kind: JJCommandKind::New {
                change_ids: smallvec![id],
                insert: Some(InsertPosition::After),
            },
            flags,
        }),
        AppAction::NewInsertBefore => make_command(app, |id| JJCommand {
            kind: JJCommandKind::New {
                change_ids: smallvec![id],
                insert: Some(InsertPosition::Before),
            },
            flags,
        }),
        AppAction::Squash => {
            if app.selected_is_merge() {
                // Merge commits can't squash into parent without specifying which one.
                // Redirect to target selection (squash into).
                enter_target_select(app, TargetOperation::Squash(SquashKind::Into), flags)
            } else {
                let selection = build_change_selection(app);
                make_command(app, |id| JJCommand {
                    kind: JJCommandKind::Squash {
                        change_id: id,
                        target: None,
                        message: MessageMode::Default,
                        selection: selection.clone(),
                    },
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
        AppAction::Restore => make_command(app, |id| JJCommand {
            kind: JJCommandKind::Restore {
                from: None,
                into: None,
                changes_in: Some(id),
                selection: build_change_selection(app),
            },
            flags,
        }),
        AppAction::RestoreFrom => enter_target_select(app, TargetOperation::RestoreFrom, flags),
        AppAction::RestoreInto => enter_target_select(app, TargetOperation::RestoreInto, flags),
        AppAction::Split => make_command(app, |id| JJCommand {
            kind: JJCommandKind::Split {
                change_id: id,
                target: None,
                selection: build_change_selection(app),
            },
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
                .remote_bookmarks
                .iter()
                .filter(|rb| !rb.is_tracked)
                .map(|rb| format!("{}@{}", rb.name, rb.remote))
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
                .remote_bookmarks
                .iter()
                .filter(|rb| rb.is_tracked)
                .map(|rb| format!("{}@{}", rb.name, rb.remote))
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

        AppAction::Undo => Action::RunJj(JJCommand {
            kind: JJCommandKind::Undo,
            flags,
        }),
        AppAction::Redo => Action::RunJj(JJCommand {
            kind: JJCommandKind::Redo,
            flags,
        }),

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
                Action::SuspendAndRunJj(JJCommand {
                    kind: JJCommandKind::GitFetch {
                        all_remotes: false,
                        remote: None,
                    },
                    flags,
                })
            }
        }
        AppAction::GitFetchAllRemotes => Action::SuspendAndRunJj(JJCommand {
            kind: JJCommandKind::GitFetch {
                all_remotes: true,
                remote: None,
            },
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
                Action::SuspendAndRunJj(JJCommand {
                    kind: JJCommandKind::GitPush {
                        all: false,
                        remote: None,
                    },
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
                Action::SuspendAndRunJj(JJCommand {
                    kind: JJCommandKind::GitPush {
                        all: true,
                        remote: None,
                    },
                    flags,
                })
            }
        }
        AppAction::GitPushChange => {
            let Some(change_id) = app.selected_change_id() else {
                return Action::None;
            };
            Action::SuspendAndRunJj(JJCommand {
                kind: JJCommandKind::GitPushChange {
                    change_id,
                    remote: None,
                },
                flags,
            })
        }
        AppAction::GitPushBookmark => {
            let bookmarks = app.selected_bookmarks().unwrap_or(&[]);
            if bookmarks.is_empty() {
                app.set_error("no bookmarks on this commit");
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
                return Action::SuspendAndRunJj(JJCommand {
                    kind: JJCommandKind::GitPushBookmark {
                        bookmarks: bookmark_names,
                        remote: None,
                    },
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
        AppAction::GitExport => Action::RunJj(JJCommand {
            kind: JJCommandKind::GitExport,
            flags,
        }),
        AppAction::GitImport => Action::RunJj(JJCommand {
            kind: JJCommandKind::GitImport,
            flags,
        }),

        // Duplicate
        AppAction::Duplicate => make_multi_command(app, |ids| JJCommand {
            kind: JJCommandKind::Duplicate {
                change_ids: ids,
                onto: None,
            },
            flags,
        }),
        AppAction::DuplicateOnto => enter_target_select(app, TargetOperation::DuplicateOnto, flags),

        // Parallelize / Simplify parents / Revert
        AppAction::Parallelize => make_multi_command(app, |ids| JJCommand {
            kind: JJCommandKind::Parallelize { change_ids: ids },
            flags,
        }),
        AppAction::SimplifyParents => make_multi_command(app, |ids| JJCommand {
            kind: JJCommandKind::SimplifyParents { change_ids: ids },
            flags,
        }),
        AppAction::Interdiff => enter_target_select(app, TargetOperation::Interdiff, flags),
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
            Action::RunJj(JJCommand {
                kind: JJCommandKind::BookmarkDelete {
                    names: smallvec![name],
                },
                flags,
            })
        }
        AppAction::BmViewTrack => {
            let Some(br) = app.selected_bookmark_ref() else {
                app.set_error("bookmark is already local");
                return Action::None;
            };
            Action::RunJj(JJCommand {
                kind: JJCommandKind::BookmarkTrack {
                    bookmarks: smallvec![br],
                },
                flags,
            })
        }
        AppAction::BmViewUntrack => {
            let Some(br) = app.selected_bookmark_ref() else {
                app.set_error("bookmark has no remote to untrack");
                return Action::None;
            };
            Action::RunJj(JJCommand {
                kind: JJCommandKind::BookmarkUntrack {
                    bookmarks: smallvec![br],
                },
                flags,
            })
        }
        AppAction::BmViewPush => {
            // On a remote target row: push to that specific remote.
            if let Some((entry, target)) = app.selected_remote_target() {
                let name = entry.name.clone();
                let remote = RemoteName::new(target.remote.as_str());
                return Action::SuspendAndRunJj(JJCommand {
                    kind: JJCommandKind::GitPushBookmark {
                        bookmarks: smallvec![name],
                        remote: Some(remote),
                    },
                    flags,
                });
            }
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            Action::SuspendAndRunJj(JJCommand {
                kind: JJCommandKind::GitPushBookmark {
                    bookmarks: smallvec![name],
                    remote: None,
                },
                flags,
            })
        }
        AppAction::BmViewJumpToCommit => {
            // On a conflict target row: resolve conflict by picking this side.
            if let Some((entry, target)) = app.selected_conflict_target() {
                let name = entry.name.clone();
                let prefix = &target.summary.change_id.display[..target
                    .summary
                    .change_id
                    .prefix_len
                    .min(target.summary.change_id.display.len())];
                let change_id = match target.change_id_suffix {
                    Some(suffix) => ChangeId::new(format!("{prefix}/{suffix}")),
                    None => ChangeId::new(prefix),
                };
                return Action::RunJj(JJCommand {
                    kind: JJCommandKind::BookmarkSet { name, change_id },
                    flags: flags | CommandFlags::ALLOW_BACKWARDS,
                });
            }
            // On a remote target row: jump to that specific commit in DAG.
            if let Some(ids) = app
                .selected_remote_target()
                .map(|(_, t)| (t.summary.commit_id.clone(), t.summary.change_id.clone()))
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
            Action::RunJj(JJCommand {
                kind: JJCommandKind::Edit { change_id },
                flags,
            })
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
            Action::RunJj(JJCommand {
                kind: JJCommandKind::BookmarkForget {
                    names: smallvec![name],
                },
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
                return Action::SuspendAndRunJj(JJCommand {
                    kind: JJCommandKind::GitFetchBookmark {
                        bookmark: entry.name.clone(),
                        remote: RemoteName::new(target.remote.as_str()),
                    },
                    flags,
                });
            }
            // On a bookmark row: fetch from all remotes.
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            if let Some(remote) = entry.kind.remote() {
                // Remote bookmark row — fetch from that remote.
                return Action::SuspendAndRunJj(JJCommand {
                    kind: JJCommandKind::GitFetchBookmark {
                        bookmark: entry.name.clone(),
                        remote: RemoteName::new(remote.as_str()),
                    },
                    flags,
                });
            }
            // Local bookmark — fetch from all remotes for this bookmark.
            Action::SuspendAndRunJj(JJCommand {
                kind: JJCommandKind::GitFetch {
                    all_remotes: false,
                    remote: None,
                },
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
            let name = entry.name.clone();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::TagDelete {
                    names: smallvec![name],
                },
                flags,
            })
        }
        AppAction::TgViewSet => {
            let Some(entry) = app.selected_tag_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
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
                app.set_error("no workspace info in operation log");
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
            let op_id = entry.id.clone();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::OpRestore { op_id },
                flags,
            })
        }
        AppAction::OpLogRevert => {
            let Some(entry) = app.selected_op_log_entry() else {
                return Action::None;
            };
            let op_id = entry.id.clone();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::OpRevert { op_id },
                flags,
            })
        }
        AppAction::OpLogAbandon => {
            let Some(entry) = app.selected_op_log_entry() else {
                return Action::None;
            };
            let op_id = entry.id.clone();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::OpAbandon { op_id },
                flags,
            })
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
        AppAction::SwitchToCommandLogView => {
            app.switch_view(crate::app::ActiveView::CommandLog);
            Action::None
        }
        AppAction::Jump => {
            app.enter_jump();
            Action::None
        }
        AppAction::EvoLogEdit => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            let change_id = ChangeId::new(entry.commit_id.as_str());
            Action::RunJj(JJCommand {
                kind: JJCommandKind::Edit { change_id },
                flags,
            })
        }
        AppAction::EvoLogNew => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            let change_id = ChangeId::new(entry.commit_id.as_str());
            Action::RunJj(JJCommand {
                kind: JJCommandKind::New {
                    change_ids: smallvec![change_id],
                    insert: None,
                },
                flags,
            })
        }
        AppAction::EvoLogInterdiff => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            if entry.is_current {
                app.set_error("already on current version");
                return Action::None;
            }
            let from_id = CommitId::new(entry.commit_id.as_str());
            let from_label = Str::from(entry.change_id.display.as_str());
            let Some(current) = app.evolog.entries.iter().find(|e| e.is_current) else {
                app.set_error("no current version found");
                return Action::None;
            };
            let to_id = CommitId::new(current.commit_id.as_str());
            let to_label = Str::from(current.change_id.display.as_str());
            app.enter_interdiff_view(from_id, to_id, from_label, to_label);
            Action::None
        }
        AppAction::EvoLogRestore => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            if entry.is_current {
                app.set_error("already on current version");
                return Action::None;
            }
            let from = ChangeId::new(entry.commit_id.as_str());
            let into = app
                .evolog
                .entries
                .iter()
                .find(|e| e.is_current)
                .map(|e| ChangeId::new(e.commit_id.as_str()));
            Action::RunJj(JJCommand {
                kind: JJCommandKind::Restore {
                    from: Some(from),
                    into,
                    changes_in: None,
                    selection: build_change_selection(app),
                },
                flags,
            })
        }
        AppAction::WsViewForget => {
            let Some(entry) = app.selected_workspace_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::WorkspaceForget {
                    names: smallvec![name],
                },
                flags,
            })
        }
        AppAction::FileAnnotate => {
            if let Some((path, _)) = extract_file_and_line(app) {
                if let Some(cid) = extract_commit_id(app) {
                    app.enter_annotate_view(cid, path);
                } else {
                    app.set_error("select a file to annotate");
                }
            } else {
                app.set_error("select a file to annotate");
            }
            Action::None
        }
        AppAction::AnnotateTimeTravel => {
            if let Some(line) = app.selected_annotate_line() {
                let new_commit = line.commit_id.clone();
                let line_number = line.line_number;
                let same_commit = app
                    .annotate
                    .commit_id
                    .as_ref()
                    .is_some_and(|c| *c == new_commit);
                if !same_commit {
                    if let Some(current_commit) = app.annotate.commit_id.clone() {
                        app.annotate.history.push((current_commit, line_number));
                    }
                    app.annotate_navigate(new_commit, line_number);
                }
            }
            Action::None
        }
        AppAction::ToggleAnnotateSeparator => {
            if app.active_view == crate::app::ActiveView::Annotate {
                app.annotate.show_commit_separators = !app.annotate.show_commit_separators;
                let label = if app.annotate.show_commit_separators {
                    "on"
                } else {
                    "off"
                };
                app.set_status(format!("commit separators: {label}"));
            }
            Action::None
        }
        AppAction::AnnotateForward => {
            if let Some((prev_commit, prev_line)) = app.annotate.history.pop() {
                app.annotate_navigate(prev_commit, prev_line);
            }
            Action::None
        }
        AppAction::EditFileWorkingCopy => {
            if let Some((path, line)) = extract_file_and_line(app) {
                return Action::EditWorkingCopyFile {
                    path: path.as_str().to_string(),
                    line,
                };
            }
            app.set_error("select a file to edit");
            Action::None
        }
        AppAction::EditFileAtRevision => {
            if let Some((path, line)) = extract_file_and_line(app) {
                if let Some(cid) = extract_commit_id(app) {
                    return Action::EditFileAtRevision {
                        commit_id: cid,
                        path,
                        line,
                    };
                }
            }
            app.set_error("select a file to edit");
            Action::None
        }
        AppAction::CheckoutAndEditFile => {
            if let Some((path, line)) = extract_file_and_line(app) {
                if let Some(cid) = extract_commit_id(app) {
                    return Action::CheckoutAndEdit {
                        commit_id: cid,
                        path: path.as_str().to_string(),
                        line,
                    };
                }
            }
            app.set_error("select a file to edit");
            Action::None
        }
        AppAction::AnnotateGoToCommit => {
            if let Some(line) = app.selected_annotate_line() {
                let commit_id = line.commit_id.clone();
                let change_id = line.change_id.clone();
                jump_to_commit_in_dag(
                    app,
                    Some(&commit_id),
                    Some(&change_id),
                    "annotate line has no commit",
                );
            }
            Action::None
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
        AppAction::CommandMode => {
            app.mode = AppMode::text_input(":", "", PendingCommand::RawCommand);
            Action::None
        }
        AppAction::FileList => {
            if let Some(cid) = extract_commit_id(app) {
                app.request_file_list(cid);
            } else {
                app.set_error("select a commit to list files");
            }
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
            app.set_cursor(row);
        }
    } else {
        // Use change ID if available, otherwise fall back to commit ID.
        let id_for_revset = change_id
            .map(|c| c.display.clone())
            .unwrap_or_else(|| cid.as_str().into());
        offer_widen_revset(app, &id_for_revset);
    }
}

/// Show a FollowUp prompt offering to widen the revset to include a commit.
/// Whether the current cursor row has a file context (for help panel greying).
pub fn has_file_context(app: &App) -> bool {
    extract_file_and_line(app).is_some()
}

pub fn has_conflict_context(app: &App) -> bool {
    matches!(
        app.rows.get(app.cursor.raw()),
        Some(
            DisplayRow::ConflictHeader { .. }
                | DisplayRow::ConflictSide { .. }
                | DisplayRow::FileChange { .. }
                | DisplayRow::DiffLine { .. }
        )
    )
}

/// Extract the file path and line number from the current cursor position.
/// Works across DAG, evolog, interdiff, and annotate views.
fn extract_file_and_line(app: &App) -> Option<(crate::types::RepoPath, usize)> {
    match app.rows.get(app.cursor.raw())? {
        // DAG view
        DisplayRow::FileChange {
            entry_idx,
            file_idx,
        } => {
            let file = app.files_for_entry(*entry_idx)?.get(file_idx.raw())?;
            Some((file.path.clone(), 1))
        }
        DisplayRow::DiffLine {
            entry_idx,
            file_idx,
            line_idx,
        } => {
            let file = app.files_for_entry(*entry_idx)?.get(file_idx.raw())?;
            let line = app
                .diff_lines(*entry_idx, *file_idx)
                .and_then(|lines| lines.get(line_idx.raw()))
                .and_then(|dl| dl.new_line)
                .unwrap_or(1) as usize;
            Some((file.path.clone(), line))
        }
        // Evolog view
        DisplayRow::EvoLogFileChange {
            evolog_idx,
            file_idx,
        }
        | DisplayRow::EvoLogFileDiffLine {
            evolog_idx,
            file_idx,
            ..
        } => {
            let entry = app.evolog.entries.get(evolog_idx.raw())?;
            let file = app
                .evolog
                .files
                .get(&entry.commit_id)?
                .loaded()?
                .get(file_idx.raw())?;
            Some((file.path.clone(), 1))
        }
        // Interdiff view
        DisplayRow::InterdiffFileChange { file_idx }
        | DisplayRow::InterdiffDiffLine { file_idx, .. } => {
            let file = app.interdiff.files.loaded()?.get(file_idx.raw())?;
            Some((file.path.clone(), 1))
        }
        // Annotate view
        DisplayRow::AnnotateLine { line_idx } => {
            let path = app.annotate.path.clone()?;
            let line = app
                .annotate
                .lines
                .loaded()
                .and_then(|l| l.get(line_idx.raw()))
                .map(|l| l.line_number)
                .unwrap_or(1);
            Some((path, line))
        }
        _ => None,
    }
}

/// Extract the commit ID for the current cursor row.
fn extract_commit_id(app: &App) -> Option<CommitId> {
    match app.rows.get(app.cursor.raw())? {
        DisplayRow::CommitNode { entry_idx }
        | DisplayRow::FileChange { entry_idx, .. }
        | DisplayRow::DiffLine { entry_idx, .. } => {
            Some(app.nodes[*entry_idx].commit.graph_id.clone())
        }
        DisplayRow::EvoLogFileChange { evolog_idx, .. }
        | DisplayRow::EvoLogFileDiffLine { evolog_idx, .. } => app
            .evolog
            .entries
            .get(evolog_idx.raw())
            .map(|e| e.commit_id.clone()),
        DisplayRow::InterdiffFileChange { .. } | DisplayRow::InterdiffDiffLine { .. } => {
            app.interdiff.to_commit_id.clone()
        }
        DisplayRow::AnnotateLine { line_idx } => app
            .annotate
            .lines
            .loaded()
            .and_then(|l| l.get(line_idx.raw()))
            .map(|l| l.commit_id.clone()),
        _ => None,
    }
}

fn offer_widen_revset(app: &mut App, id: &str) {
    app.mode = AppMode::FollowUp {
        prompt: "commit not in current revset".into(),
        options: vec![FollowUpOption {
            key: 'w',
            label: "widen revset",
            action: FollowUpAction::WidenRevset {
                change_id: id.into(),
            },
        }],
    };
}

pub(super) fn execute_follow_up(app: &mut App, action: FollowUpAction) -> Action {
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
        FollowUpAction::EnterInterdiff {
            from,
            to,
            from_label,
            to_label,
        } => {
            app.enter_interdiff_view(from, to, from_label, to_label);
            Action::None
        }
    }
}

/// Write resolved conflict content to disk and return the appropriate action.
fn write_conflict_resolution(app: &mut App, result: crate::app::ConflictPickResult) -> Action {
    match result {
        crate::app::ConflictPickResult::FileResolved { path, content } => {
            let full_path = std::path::Path::new(&app.repo_root).join(path.as_str());
            if std::fs::write(&full_path, &content).is_ok() {
                app.set_status(format!("resolved {}", path));
                Action::Refresh
            } else {
                app.set_error(format!("failed to write {}", path));
                Action::None
            }
        }
        crate::app::ConflictPickResult::Pending => Action::None,
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

pub(super) fn enter_target_select(
    app: &mut App,
    operation: TargetOperation,
    flags: CommandFlags,
) -> Action {
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
