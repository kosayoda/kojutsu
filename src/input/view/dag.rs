use smallvec::smallvec;

use crate::app::{App, AppMode};
use crate::jj_command::{InsertPosition, JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::{
    BookmarkName, ChangeId, DisplayRow, MessageMode, PendingCommand, PendingSelection, RebaseKind,
    RebaseSource, RebaseTarget, SelectionKind, SmallVec, SplitKind, SquashKind, Str,
    TargetOperation,
};

use crate::input::action::{build_change_selection, enter_target_select, run_cmd};
use crate::input::bookmark::{
    enter_bookmark_advance, enter_bookmark_select, enter_bookmark_text_input,
    enter_remote_bookmark_select, enter_tag_delete, BookmarkTextAction, PendingSelectionKind,
};
use crate::input::Action;

pub(in crate::input) fn dispatch(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    action: AppAction,
    flags: CommandFlags,
) -> Action {
    match action {
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
                        app.set_error("merge tool is for whole-file only");
                        return Action::None;
                    }
                };
                let result = app.pick_conflict_side(*entry_idx, *file_idx, *hunk_idx, side);
                return write_conflict_resolution(app, result);
            }

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

        AppAction::BookmarkCreate => {
            enter_bookmark_text_input(app, flags, "create bookmark: ", BookmarkTextAction::Create)
        }
        AppAction::BookmarkSet => {
            enter_bookmark_text_input(app, flags, "set bookmark: ", BookmarkTextAction::Set)
        }
        AppAction::BookmarkDelete => {
            enter_bookmark_select(app, lua, flags, PendingSelectionKind::Delete)
        }
        AppAction::BookmarkForget => {
            enter_bookmark_select(app, lua, flags, PendingSelectionKind::Forget)
        }
        AppAction::BookmarkMove => {
            enter_bookmark_select(app, lua, flags, PendingSelectionKind::Move)
        }
        AppAction::BookmarkRename => {
            enter_bookmark_select(app, lua, flags, PendingSelectionKind::Rename)
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

        AppAction::Duplicate => make_multi_command(app, |ids| JJCommand {
            kind: JJCommandKind::Duplicate {
                change_ids: ids,
                onto: None,
            },
            flags,
        }),
        AppAction::DuplicateOnto => {
            let sources = app.selected_change_ids();
            if sources.is_empty() {
                return Action::None;
            }
            enter_target_select(app, TargetOperation::DuplicateOnto { sources }, flags)
        }

        AppAction::Parallelize => make_multi_command(app, |ids| JJCommand {
            kind: JJCommandKind::Parallelize { change_ids: ids },
            flags,
        }),
        AppAction::SimplifyParents => make_multi_command(app, |ids| JJCommand {
            kind: JJCommandKind::SimplifyParents { change_ids: ids },
            flags,
        }),
        AppAction::ArrangeUp => arrange(app, flags, ArrangeDirection::Up),
        AppAction::ArrangeDown => arrange(app, flags, ArrangeDirection::Down),
        AppAction::Interdiff => enter_target_select(app, TargetOperation::Interdiff, flags),
        AppAction::Revert => {
            let sources = app.selected_change_ids();
            enter_target_select(app, TargetOperation::Revert { sources }, flags)
        }

        _ => Action::None,
    }
}

fn make_multi_command(app: &App, build: impl FnOnce(SmallVec<ChangeId>) -> JJCommand) -> Action {
    let ids = app.selected_change_ids();
    if ids.is_empty() {
        return Action::None;
    }
    run_cmd(build(ids))
}

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

enum ArrangeDirection {
    Up,
    Down,
}

fn arrange(app: &mut App, flags: CommandFlags, direction: ArrangeDirection) -> Action {
    let Some(entry_idx) = app.selected_entry_idx() else {
        return Action::None;
    };
    let node = &app.nodes[entry_idx];
    let change_id = node.commit.unique_prefix();

    let (neighbors, noun, kind) = match direction {
        ArrangeDirection::Up => (&node.children, "child", RebaseKind::After),
        ArrangeDirection::Down => (&node.parents, "parent", RebaseKind::Before),
    };

    if neighbors.len() != 1 {
        app.set_error(format!("arrange: commit must have exactly one {noun}"));
        return Action::None;
    }

    let target_id = app.nodes[neighbors[0]].commit.unique_prefix();
    run_cmd(JJCommand {
        kind: JJCommandKind::Rebase {
            change_ids: smallvec![change_id],
            source_mode: RebaseSource::Revision,
            dest: RebaseTarget {
                targets: smallvec![target_id],
                kind,
            },
        },
        flags,
    })
}

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
