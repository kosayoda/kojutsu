use std::collections::HashSet;

use crate::app::{App, AppMode, TargetMode};
use crate::dag::DiffLineKind;
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{
    self, ActionId, ActionRegistry, AppAction, CommandFlags, Keymap, LookupResult, TrieNode,
    action_label,
};
use crate::repo_service::RevsetLoadKind;
use crate::types::ChangeSelection;
use crate::types::FileOwner;
use crate::types::{
    CommitId, DisplayRow, FollowUpAction, FollowUpOption, PendingCommand, PendingSelection,
    SelectionKind, TargetOperation, WorkspaceName,
};

use super::Action;

/// Number of rows to jump for page-up/page-down style navigation.
use super::PAGE_SIZE;

/// Revset the conflicted-filter toggle switches to (and recognizes to
/// switch back from).
const CONFLICTED_REVSET: &str = "conflicted()";

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
            dispatch_action(app, registry, lua, keymap, action, CommandFlags::empty())
        }
        LookupResult::Action(ActionId::Lua(id)) => {
            app.status_message = None;
            run_lua_command(lua, registry, id, app, CommandFlags::empty())
        }
        LookupResult::Prefix { label, children } => {
            app.status_message = None;
            // Opening a submenu whose every entry the selection rules out
            // would be a dead end. Only gate when something is selected:
            // otherwise a submenu of nothing but sub-prefixes never opens.
            let kinds = app.selection_kinds();
            if !kinds.is_empty() {
                let any_usable = children.iter().any(|(_, n)| match n {
                    TrieNode::Action { id, .. } => {
                        !kinds.blocked_by(registry.selection_support(*id))
                    }
                    _ => false,
                });
                if !any_usable {
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

/// Fallback for the select modes: a key they don't own resolves through the
/// keymap like any other, but only cursor movement is admitted: anything else
/// would mutate the repo or move the selection out from under a pending pick.
///
/// Sequence bindings resolve to `Prefix`, which would need the submenu state
/// these modes don't have, so they stay unavailable here.
pub(super) fn handle_select_navigation(
    app: &mut App,
    registry: &ActionRegistry,
    lua: &crate::lua::LuaEngine,
    keymap: &Keymap,
    node: &keymap_parser::Node,
) -> Action {
    match keymap.lookup(node) {
        LookupResult::Action(ActionId::Builtin(action)) if action.is_cursor_navigation() => {
            app.status_message = None;
            dispatch_action(app, registry, lua, keymap, action, CommandFlags::empty())
        }
        // Unlike normal mode, an unrecognised key is ignored rather than
        // reported: most of the keymap is simply out of scope mid-pick.
        _ => Action::None,
    }
}

pub(super) fn handle_submenu_key(
    app: &mut App,
    registry: &ActionRegistry,
    lua: &crate::lua::LuaEngine,
    keymap: &Keymap,
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
            dispatch_action(app, registry, lua, keymap, action, flags)
        }
        LookupResult::Action(ActionId::Lua(id)) => {
            app.mode = AppMode::Normal;
            run_lua_command(lua, registry, id, app, flags)
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

/// Whether the active selection is outside what `id` declares it can act on,
/// reporting it if so. Shared by the builtin and plugin paths: a Lua command's
/// `selection` option is filtered on in the submenu and greyed out in help, so
/// it has to be refused on the keypress too.
fn rejects_selection(app: &mut App, registry: &ActionRegistry, id: ActionId, label: &str) -> bool {
    if !app
        .selection_kinds()
        .blocked_by(registry.selection_support(id))
    {
        return false;
    }
    // Describe what is selected, not the kind that won the precedence: a
    // mixed selection is rejected for the part the action can't take.
    let selected = app
        .selection_summary()
        .describe()
        .unwrap_or_else(|| format!("a {} selection", app.selection_kind()));
    app.set_error(format!("{label} does not support {selected}"));
    true
}

fn run_lua_command(
    lua: &crate::lua::LuaEngine,
    registry: &ActionRegistry,
    id: u16,
    app: &mut App,
    flags: CommandFlags,
) -> Action {
    if rejects_selection(app, registry, ActionId::Lua(id), lua.command_name(id)) {
        return Action::None;
    }
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
    keymap: &Keymap,
    action: AppAction,
    flags: CommandFlags,
) -> Action {
    if action == AppAction::RepeatLast {
        return if let Some((prev_action, prev_flags)) = app.last_repeatable {
            dispatch_action(app, registry, lua, keymap, prev_action, prev_flags)
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

    let result = dispatch_action_after_hooks(app, registry, lua, keymap, action, flags);

    if action.is_repeatable() {
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
    keymap: &Keymap,
    action: AppAction,
    flags: CommandFlags,
) -> Action {
    let id_name = crate::keymap::action_id_name(action);
    app.last_action_label = Some(id_name);

    // Merge global toggles into the command flags.
    let flags = flags | app.toggles;

    if rejects_selection(
        app,
        registry,
        ActionId::Builtin(action),
        action_label(action),
    ) {
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
                ConflictTerm {
                    hunk: crate::types::ConflictHunkRef,
                    kind: crate::conflict::ConflictTermKind,
                    absent: bool,
                },
            }
            let target = match app.rows.get(app.cursor.raw()) {
                Some(DisplayRow::CommitNode { entry_idx }) => {
                    Some(SelectTarget::Commit(*entry_idx))
                }
                Some(DisplayRow::FileChange {
                    owner: FileOwner::Dag(entry_idx),
                    file_idx,
                }) => Some(SelectTarget::File(*entry_idx, *file_idx)),
                Some(DisplayRow::DiffLine {
                    owner: FileOwner::Dag(entry_idx),
                    file_idx,
                    line_idx,
                }) => {
                    let Some(diff_lines) = app.diff_lines(FileOwner::Dag(*entry_idx), *file_idx)
                    else {
                        return Action::None;
                    };
                    let dl = &diff_lines[line_idx.raw()];
                    if dl.conflict_region {
                        app.set_error(
                            "conflict region: resolve the conflict or select the whole file",
                        );
                        return Action::None;
                    }
                    let kind = dl.kind;
                    Some(SelectTarget::DiffLine(
                        *entry_idx, *file_idx, *line_idx, kind,
                    ))
                }
                // On a conflict term row, space toggles the pick for the
                // side under the cursor (the only way to pick sides beyond
                // ours/theirs/base in n-way merges).
                Some(row @ DisplayRow::ConflictTerm { term_idx, .. }) => {
                    let hunk = row.conflict_hunk().expect("ConflictTerm row has a hunk");
                    app.nodes
                        .get(hunk.entry_idx)
                        .and_then(|n| n.conflict_hunks(hunk.file_idx))
                        .and_then(|l| l.loaded())
                        .and_then(|hunks| hunks.get(hunk.hunk_idx.raw()))
                        .and_then(|h| match h {
                            crate::conflict::ConflictHunkKind::Conflict { terms, .. } => {
                                terms.get(term_idx.raw())
                            }
                            _ => None,
                        })
                        .map(|term| SelectTarget::ConflictTerm {
                            hunk,
                            kind: term.kind,
                            absent: term.absent,
                        })
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
                Some(SelectTarget::ConflictTerm { hunk, kind, absent }) => {
                    if absent {
                        app.set_error(super::view::dag::ERR_SIDE_DELETED);
                    } else if app.pick_conflict_term(hunk, kind).is_some() {
                        // Picked (not unpicked): offer to apply if that was
                        // the last hunk, otherwise move to the next conflict.
                        if !super::view::dag::maybe_offer_apply(
                            app,
                            hunk.entry_idx,
                            hunk.file_idx,
                            flags,
                        ) {
                            app.jump_to_conflict(crate::types::NavDirection::Forward);
                        }
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
        AppAction::NextConflict => {
            if !app.jump_to_conflict(crate::types::NavDirection::Forward) {
                app.set_error("no conflicts in current view");
            }
            Action::None
        }
        AppAction::PrevConflict => {
            if !app.jump_to_conflict(crate::types::NavDirection::Backward) {
                app.set_error("no conflicts in current view");
            }
            Action::None
        }
        AppAction::Refresh => Action::Refresh,
        AppAction::ReloadConfig => Action::ReloadConfig,
        AppAction::SelectPreset => {
            if app.config.revsets.presets.is_empty() {
                app.set_error("no presets configured");
                return Action::None;
            }
            let items: Vec<String> = app
                .config
                .revsets
                .presets
                .iter()
                .map(|p| p.name.clone())
                .collect();
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
            app.revset.conflicted_prev = None;
            app.request_revset_load(None, RevsetLoadKind::NoSnapshot);
            Action::None
        }
        AppAction::ToggleConflictedRevset => {
            app.revset.active_preset = None;
            if let Some(prev) = app.revset.conflicted_prev.take() {
                // Toggle off: return to the revset we were viewing before.
                app.request_revset_load(Some(prev.to_string()), RevsetLoadKind::NoSnapshot);
            } else {
                // Toggle on: remember where we were so we can come back.
                app.revset.conflicted_prev = Some(app.revset.current.clone());
                app.request_revset_load(
                    Some(CONFLICTED_REVSET.to_string()),
                    RevsetLoadKind::NoSnapshot,
                );
            }
            Action::None
        }
        AppAction::SwitchPreset1
        | AppAction::SwitchPreset2
        | AppAction::SwitchPreset3
        | AppAction::SwitchPreset4
        | AppAction::SwitchPreset5 => {
            let slot = action.preset_slot().expect("switch-preset action");
            if let Some(preset) = app.config.revsets.presets.get(slot) {
                app.revset.active_preset = Some(slot);
                Action::UpdateRevset(preset.revset.clone())
            } else {
                // No preset at this slot: use jj's default revset.
                app.revset.active_preset = None;
                app.revset.conflicted_prev = None;
                app.request_revset_load(None, RevsetLoadKind::NoSnapshot);
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
        // As an overlay, so dismissing help returns to a target- or
        // commit-select that was in progress rather than dropping it.
        AppAction::ShowHelp => {
            app.enter_overlay(AppMode::Help { scroll: 0 });
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
        | AppAction::ConflictUnpick
        | AppAction::ConflictApplyPicks
        | AppAction::ConflictEditFile
        | AppAction::ConflictEditHunk
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
            app.enter_jump(keymap);
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
                app.request_file_view(cid, path, line);
                return Action::None;
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
            .map(|c| c.display().to_string())
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
    app.rows.get(app.cursor.raw()).is_some_and(|row| {
        row.conflict_hunk().is_some()
            || matches!(
                row,
                DisplayRow::FileChange {
                    owner: FileOwner::Dag(_),
                    ..
                } | DisplayRow::DiffLine {
                    owner: FileOwner::Dag(_),
                    ..
                }
            )
    })
}

/// Extract the file path and line number from the current cursor position.
/// Works across DAG, evolog, interdiff, and annotate views.
fn extract_file_and_line(app: &App) -> Option<(crate::types::RepoPath, usize)> {
    match app.rows.get(app.cursor.raw())? {
        row @ (DisplayRow::FileChange { owner, file_idx }
        | DisplayRow::DiffLine {
            owner, file_idx, ..
        }) => {
            let file = app.file(*owner, *file_idx)?;
            let line = app
                .row_diff_line(*row)
                .and_then(|dl| dl.new_line)
                .unwrap_or(1) as usize;
            Some((file.path.clone(), line))
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
        DisplayRow::CommitNode { entry_idx } => Some(app.nodes[*entry_idx].commit.graph_id.clone()),
        DisplayRow::FileChange { owner, .. } | DisplayRow::DiffLine { owner, .. } => app
            .file_tree(*owner)
            .map(|tree| tree.target().after().clone()),
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
        FollowUpAction::ResolveConflict {
            change_id,
            path,
            content,
            flags,
        } => match super::view::dag::staged_resolution(app, change_id, &path, &content, flags) {
            Some(cmd) => run_cmd(cmd),
            None => Action::None,
        },
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

#[cfg(test)]
mod selection_gate_tests {
    use super::*;
    use crate::keymap::SelectionKindSet;
    use crate::types::{ChangeId, FileRef, RepoPath, Selection};

    fn select_file(app: &mut App) {
        app.selection.insert(Selection::File(FileRef {
            change_id: ChangeId::new("qpvuntsm"),
            path: RepoPath::new("a.rs"),
        }));
    }

    fn select_line(app: &mut App) {
        app.selection.insert(Selection::Line {
            file_ref: FileRef {
                change_id: ChangeId::new("qpvuntsm"),
                path: RepoPath::new("b.rs"),
            },
            old_line: None,
            new_line: Some(3),
        });
    }

    /// A plugin's `selection` option is filtered on in the submenu and greyed
    /// out in help; it has to hold on the keypress too, or the UI is lying.
    #[test]
    fn a_lua_command_is_refused_outside_its_declared_selection() {
        let mut registry = ActionRegistry::new();
        let file_only = registry.register_lua(SelectionKindSet::FILE);
        let mut app = App::for_test();

        select_file(&mut app);
        assert!(!rejects_selection(&mut app, &registry, file_only, "plug"));

        select_line(&mut app);
        assert!(rejects_selection(&mut app, &registry, file_only, "plug"));
    }

    /// The message names what is selected rather than the kind that won the
    /// precedence, which for a mixed selection is only half the story.
    #[test]
    fn the_refusal_describes_the_whole_selection() {
        let mut registry = ActionRegistry::new();
        let file_only = registry.register_lua(SelectionKindSet::FILE);
        let mut app = App::for_test();
        select_file(&mut app);
        select_line(&mut app);

        assert!(rejects_selection(&mut app, &registry, file_only, "plug"));
        let message = app.status_message.as_ref().expect("an error").0.clone();
        assert_eq!(message, "plug does not support 1 file + 1 line");
    }

    #[test]
    fn a_command_declaring_all_takes_anything() {
        let mut registry = ActionRegistry::new();
        let anything = registry.register_lua(SelectionKindSet::ALL);
        let mut app = App::for_test();
        select_file(&mut app);
        select_line(&mut app);
        assert!(!rejects_selection(&mut app, &registry, anything, "plug"));
    }

    #[test]
    fn no_selection_gates_nothing() {
        let mut registry = ActionRegistry::new();
        let commit_only = registry.register_lua(SelectionKindSet::COMMIT);
        let mut app = App::for_test();
        assert!(!rejects_selection(&mut app, &registry, commit_only, "plug"));
    }
}
