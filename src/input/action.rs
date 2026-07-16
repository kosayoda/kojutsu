use std::collections::HashSet;

use crate::app::{App, AppMode, TargetMode};
use crate::dag::DiffLineKind;
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{
    self, ActionId, ActionRegistry, AppAction, CommandFlags, Keymap, LookupResult, TrieNode,
    action_label,
};
use crate::types::ChangeSelection;
use crate::types::{
    CommitId, DisplayRow, FollowUpAction, FollowUpOption, PendingCommand, PendingSelection,
    SelectionKind, TargetOperation, WorkspaceName,
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
            let Some(selections) = app.explicit_selection() else {
                return ChangeSelection::All;
            };
            match crate::selection::serialize_selections(selections) {
                Ok(path) => ChangeSelection::Lines(path),
                Err(e) => {
                    tracing::warn!("failed to serialize line selections: {e}");
                    ChangeSelection::All
                }
            }
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
            dispatch_action(app, registry, lua, action, CommandFlags::empty())
        }
        LookupResult::Action(ActionId::Lua(id)) => {
            app.status_message = None;
            run_lua_command(lua, id, app, CommandFlags::empty())
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
            app.pending_toggles = children
                .iter()
                .filter_map(|(key_node, child)| match child {
                    TrieNode::Toggle { flag, description } => Some(crate::app::SubmenuToggle {
                        node: key_node.clone(),
                        flag: *flag,
                        description: description.clone(),
                    }),
                    _ => None,
                })
                .collect();
            app.mode = AppMode::Normal;
            dispatch_action(app, registry, lua, action, flags)
        }
        LookupResult::Action(ActionId::Lua(id)) => {
            app.mode = AppMode::Normal;
            run_lua_command(lua, id, app, flags)
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

fn run_lua_command(
    lua: &crate::lua::LuaEngine,
    id: u16,
    app: &mut App,
    flags: CommandFlags,
) -> Action {
    let result = lua.execute_command(id, app, flags);
    if matches!(result, Action::RunJj(_) | Action::SuspendAndRunJj(_)) {
        app.last_repeatable = None;
    }
    result
}

fn dispatch_action(
    app: &mut App,
    registry: &ActionRegistry,
    lua: &crate::lua::LuaEngine,
    action: AppAction,
    flags: CommandFlags,
) -> Action {
    if action == AppAction::RepeatLast {
        return if let Some((prev_action, prev_flags)) = app.last_repeatable {
            dispatch_action(app, registry, lua, prev_action, prev_flags)
        } else {
            app.set_error("no action to repeat");
            Action::None
        };
    }

    let id_name = crate::keymap::action_id_name(action);

    match lua.run_pre_hooks(id_name, action, flags, app) {
        crate::lua::HookOutcome::Cancel => {
            return Action::None;
        }
        // The hook yielded (e.g. waiting on a prompt or a jj command); the
        // carried action starts whatever the suspended thread requested.
        crate::lua::HookOutcome::Suspended(yield_action) => {
            return yield_action;
        }
        crate::lua::HookOutcome::Proceed => {}
    }

    let result = dispatch_action_after_hooks(app, registry, lua, action, flags);

    if action.is_repeatable() && matches!(result, Action::RunJj(_) | Action::SuspendAndRunJj(_)) {
        app.last_repeatable = Some((action, flags));
    } else if action.is_mutation() {
        app.last_repeatable = None;
    }

    result
}

pub fn dispatch_action_after_hooks(
    app: &mut App,
    registry: &ActionRegistry,
    lua: &crate::lua::LuaEngine,
    action: AppAction,
    flags: CommandFlags,
) -> Action {
    let id_name = crate::keymap::action_id_name(action);
    app.last_action_label = Some(id_name);

    // Merge global toggles into the command flags.
    let flags = flags | app.toggles;

    if app.selection_active()
        && !registry
            .selection_support(ActionId::Builtin(action))
            .contains(app.selection_kind().as_bitset())
    {
        app.set_error(format!(
            "{} does not support {} selection",
            action_label(action),
            app.selection_kind()
        ));
        return Action::None;
    }

    // Visual mode intercepts (line or commit): constrain movement, handle v/space/esc.
    if app.in_visual_mode() {
        match action {
            AppAction::MoveDown
            | AppAction::MoveUp
            | AppAction::MoveDownSection
            | AppAction::MoveUpSection
            | AppAction::PageDown
            | AppAction::PageUp
            | AppAction::MoveToTop
            | AppAction::MoveToBottom
            | AppAction::MoveToScreenTop
            | AppAction::MoveToScreenMiddle
            | AppAction::MoveToScreenBottom
            | AppAction::JumpToWorkingCopy => {
                app.visual_move(action, PAGE_SIZE);
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
            if !app.jump_to_working_copy() {
                app.set_error("working copy not in current revset");
            }
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
        AppAction::ToggleDiffUnderline => {
            app.diff_underline = !app.diff_underline;
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
            app.request_revset_load_no_snapshot(None);
            Action::None
        }
        AppAction::SwitchPreset1
        | AppAction::SwitchPreset2
        | AppAction::SwitchPreset3
        | AppAction::SwitchPreset4
        | AppAction::SwitchPreset5 => {
            let slot = action.preset_slot().expect("switch-preset action");
            if let Some(preset) = app.revset.presets.get(slot) {
                app.revset.active_preset = Some(slot);
                Action::UpdateRevset(preset.revset.clone())
            } else {
                // No preset at this slot — use jj's default revset.
                app.revset.active_preset = None;
                app.request_revset_load_no_snapshot(None);
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
        // DAG-view commit mutation actions
        AppAction::Abandon
        | AppAction::Absorb
        | AppAction::ExpandAncestors
        | AppAction::ExpandDescendants
        | AppAction::Fix
        | AppAction::ResolveOurs
        | AppAction::ResolveTheirs
        | AppAction::ResolveMergeTool
        | AppAction::ConflictPickOurs
        | AppAction::ConflictPickTheirs
        | AppAction::ConflictPickBase
        | AppAction::FileUntrack
        | AppAction::Commit
        | AppAction::CommitWithMessage
        | AppAction::Describe
        | AppAction::DescribeInEditor
        | AppAction::Diffedit
        | AppAction::Edit
        | AppAction::New
        | AppAction::NewInsertAfter
        | AppAction::NewInsertBefore
        | AppAction::Squash
        | AppAction::SquashInto
        | AppAction::SquashOnto
        | AppAction::SquashAfter
        | AppAction::SquashBefore
        | AppAction::RebaseRevision
        | AppAction::RebaseSource
        | AppAction::RebaseBranch
        | AppAction::Restore
        | AppAction::RestoreFrom
        | AppAction::RestoreInto
        | AppAction::Split
        | AppAction::SplitOnto
        | AppAction::SplitAfter
        | AppAction::SplitBefore
        | AppAction::BookmarkCreate
        | AppAction::BookmarkSet
        | AppAction::BookmarkDelete
        | AppAction::BookmarkForget
        | AppAction::BookmarkMove
        | AppAction::BookmarkRename
        | AppAction::BookmarkAdvance
        | AppAction::BookmarkTrack
        | AppAction::BookmarkUntrack
        | AppAction::TagSet
        | AppAction::TagDelete
        | AppAction::Undo
        | AppAction::Redo
        | AppAction::GitFetch
        | AppAction::GitFetchAllRemotes
        | AppAction::GitPush
        | AppAction::GitPushAll
        | AppAction::GitPushChange
        | AppAction::GitPushBookmark
        | AppAction::GitExport
        | AppAction::GitImport
        | AppAction::Duplicate
        | AppAction::DuplicateOnto
        | AppAction::Parallelize
        | AppAction::SimplifyParents
        | AppAction::Interdiff
        | AppAction::Revert
        | AppAction::Run
        | AppAction::ArrangeUp
        | AppAction::ArrangeDown => super::view::dag::dispatch(app, lua, action, flags),
        AppAction::SwitchToDagView => {
            app.switch_view(crate::app::ActiveView::Dag);
            Action::None
        }
        AppAction::SwitchToBookmarkView => {
            app.switch_view(crate::app::ActiveView::Bookmarks);
            Action::None
        }
        // Bookmark view actions
        AppAction::BookmarkViewDelete
        | AppAction::BookmarkViewTrack
        | AppAction::BookmarkViewUntrack
        | AppAction::BookmarkViewPush
        | AppAction::BookmarkViewJumpToCommit
        | AppAction::BookmarkViewEdit
        | AppAction::BookmarkViewRename
        | AppAction::BookmarkViewMove
        | AppAction::BookmarkViewForget
        | AppAction::BookmarkViewSet
        | AppAction::BookmarkViewFetchDefault
        | AppAction::BookmarkViewFetchBookmark
        | AppAction::BookmarkViewFetchAllRemotes
        | AppAction::BookmarkViewInterdiff => super::view::bookmark::dispatch(app, action, flags),
        // Tag view actions
        AppAction::SwitchToTagView => {
            app.switch_view(crate::app::ActiveView::Tags);
            Action::None
        }
        AppAction::TagViewDelete
        | AppAction::TagViewSet
        | AppAction::TagViewJumpToCommit
        | AppAction::TagViewEdit => super::view::tag::dispatch(app, action, flags),
        // Operations view actions
        AppAction::SwitchToOpLogView => {
            app.switch_view(crate::app::ActiveView::Operations);
            Action::None
        }
        AppAction::OpLogFilterWorkspace
        | AppAction::OpLogRestore
        | AppAction::OpLogRevert
        | AppAction::OpLogAbandon => super::view::oplog::dispatch(app, action, flags),
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
        AppAction::EvoLogEdit
        | AppAction::EvoLogNew
        | AppAction::EvoLogInterdiff
        | AppAction::EvoLogRestore => super::view::evolog::dispatch(app, action, flags),
        AppAction::WorkspaceViewForget => super::view::workspace::dispatch(app, action, flags),
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
        AppAction::AnnotateTimeTravel
        | AppAction::ToggleAnnotateSeparator
        | AppAction::AnnotateForward => super::view::annotate::dispatch(app, action, flags),
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
            if let Some((path, line)) = extract_file_and_line(app)
                && let Some(cid) = extract_commit_id(app)
            {
                return Action::EditFileAtRevision {
                    commit_id: cid,
                    path,
                    line,
                };
            }
            app.set_error("select a file to edit");
            Action::None
        }
        AppAction::CheckoutAndEditFile => {
            if let Some((path, line)) = extract_file_and_line(app)
                && let Some(cid) = extract_commit_id(app)
            {
                return Action::CheckoutAndEdit {
                    commit_id: cid,
                    path: path.as_str().to_string(),
                    line,
                };
            }
            app.set_error("select a file to edit");
            Action::None
        }
        AppAction::AnnotateGoToCommit => super::view::annotate::dispatch(app, action, flags),
        AppAction::WorkspaceViewJumpToCommit => {
            super::view::workspace::dispatch(app, action, flags)
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
        AppAction::RepeatLast => unreachable!("handled in dispatch_action"),
    }
}

/// Turn a command into the appropriate run/suspend action.
pub(super) fn run_cmd(cmd: JJCommand) -> Action {
    if cmd.is_interactive() {
        Action::SuspendAndRunJj(cmd)
    } else {
        Action::RunJj(cmd)
    }
}

/// Try to jump to a commit in the DAG view. If the commit is in the current
/// revset, switches to DAG and moves the cursor. Otherwise offers to widen
/// the revset. If there's no commit at all, shows an error.
pub(super) fn jump_to_commit_in_dag(
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
                | DisplayRow::ConflictTerm { .. }
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
            let path = app.annotate.target.as_ref()?.path.clone();
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
        DisplayRow::InterdiffFileChange { .. } | DisplayRow::InterdiffDiffLine { .. } => app
            .interdiff
            .target
            .as_ref()
            .map(|t| t.to_commit_id.clone()),
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
            app.jump_after_refresh = Some(crate::types::JumpTarget::Prefix(change_id));
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
    let toggles = std::mem::take(&mut app.pending_toggles);
    app.mode = AppMode::TargetSelect {
        prompt: operation.label(),
        source,
        restore_cursor,
        operation,
        flags,
        target_mode,
        toggles,
    };
    Action::None
}
