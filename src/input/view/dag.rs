use smallvec::smallvec;

use crate::app::{App, AppMode};
use crate::jj_command::{InsertPosition, JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::{
    ArrangeDirection, FollowUpAction, FollowUpOption, MessageMode, PendingCommand,
    PendingSelection, RebaseKind, RebaseSource, RebaseTarget, RemoteCommand, RevisionArg,
    SelectionKind, SmallVec, SplitKind, SquashKind, Str, TargetOperation,
};

use crate::input::Action;
use crate::input::action::{build_change_selection, enter_target_select};
use crate::input::bookmark::enter_bookmark_advance;
use crate::input::target::with_remote;

/// Shown when a per-hunk conflict action fires off a conflict hunk row.
pub(in crate::input) const ERR_NOT_ON_HUNK: &str = "per-hunk only: use on a conflict hunk row";
/// Shown when picking a side that deleted the file (assembly can't express
/// a deletion, only empty content).
pub(in crate::input) const ERR_SIDE_DELETED: &str =
    "that side deleted the file: use C,o / C,t to take it whole-file";

pub(in crate::input) fn dispatch(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
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
        AppAction::ExpandDescendants => {
            let Some(entry_idx) = app.selected_entry_idx() else {
                return Action::None;
            };
            app.expand_descendants(entry_idx);
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
        AppAction::Run => enter_run_input(app, flags),
        AppAction::ResolveOurs | AppAction::ResolveTheirs | AppAction::ResolveMergeTool => {
            // Whole-file resolution via jj's native tools, from any row of
            // the conflicted file (jj handles deletion sides itself).
            let Some((entry_idx, file_idx)) =
                app.rows.get(app.cursor.raw()).and_then(|r| r.dag_file())
            else {
                app.set_error("cursor must be on a conflicted file");
                return Action::None;
            };
            let Some(file) = app.nodes[entry_idx]
                .files
                .files()
                .and_then(|f| f.get(file_idx.raw()))
            else {
                return Action::None;
            };
            if !file.has_conflict {
                app.set_error("no conflict on this file");
                return Action::None;
            }
            let change_id = app.revision(entry_idx);
            let path = Str::from(file.path.as_str());
            let tool = match action {
                AppAction::ResolveOurs => crate::jj_command::ResolveTool::Ours,
                AppAction::ResolveTheirs => crate::jj_command::ResolveTool::Theirs,
                _ => crate::jj_command::ResolveTool::Default,
            };
            Action::run(JJCommand {
                kind: JJCommandKind::Resolve {
                    change_id,
                    path,
                    tool,
                },
                flags,
            })
        }
        AppAction::ConflictPickOurs
        | AppAction::ConflictPickTheirs
        | AppAction::ConflictPickBase => {
            if let Some(hunk) = app
                .rows
                .get(app.cursor.raw())
                .and_then(|r| r.conflict_hunk())
            {
                let pick = match action {
                    AppAction::ConflictPickOurs => crate::conflict::ConflictTermKind::Side(0),
                    AppAction::ConflictPickTheirs => crate::conflict::ConflictTermKind::Side(1),
                    _ => crate::conflict::ConflictTermKind::Base(0),
                };
                if picked_term_is_absent(app, hunk, pick) {
                    app.set_error(ERR_SIDE_DELETED);
                    return Action::None;
                }
                if app.pick_conflict_term(hunk, pick).is_some() {
                    maybe_offer_apply(app, hunk.entry_idx, hunk.file_idx, flags);
                }
            } else {
                app.set_error(ERR_NOT_ON_HUNK);
            }
            Action::None
        }
        AppAction::ConflictUnpick => {
            if let Some(hunk) = app
                .rows
                .get(app.cursor.raw())
                .and_then(|r| r.conflict_hunk())
            {
                app.unpick_conflict(hunk);
            } else {
                app.set_error(ERR_NOT_ON_HUNK);
            }
            Action::None
        }
        AppAction::ConflictEditHunk => {
            // Hand-edit this hunk's resolution in $EDITOR, seeded with the
            // current pick (or the materialized markers if unpicked).
            let Some(hunk) = app
                .rows
                .get(app.cursor.raw())
                .and_then(|r| r.conflict_hunk())
            else {
                app.set_error(ERR_NOT_ON_HUNK);
                return Action::None;
            };
            let Some(path) = app.nodes[hunk.entry_idx]
                .files
                .files()
                .and_then(|f| f.get(hunk.file_idx.raw()))
                .map(|f| f.path.clone())
            else {
                return Action::None;
            };
            let pick = app.hunk_pick(hunk).cloned();
            let seed = app.conflict_hunk(hunk).and_then(|h| match h {
                crate::conflict::ConflictHunkKind::Conflict { terms } => Some(match pick {
                    Some(crate::conflict::ConflictPick::Edited(text)) => text.to_content(),
                    Some(crate::conflict::ConflictPick::Term(kind)) => terms
                        .iter()
                        .find(|t| t.kind == kind)
                        .map(|t| t.text.to_content())
                        .unwrap_or_default(),
                    None => crate::repo::hunk_markers(terms),
                }),
                _ => None,
            });
            let Some(seed) = seed else {
                return Action::None;
            };
            Action::EditConflictHunk {
                commit_id: app.commit_id(hunk.entry_idx).clone(),
                path,
                hunk_idx: hunk.hunk_idx,
                seed,
                flags,
            }
        }
        AppAction::ConflictApplyPicks => {
            let Some((entry_idx, file_idx)) =
                app.rows.get(app.cursor.raw()).and_then(|r| r.dag_file())
            else {
                app.set_error("cursor must be on a conflicted file");
                return Action::None;
            };
            let Some((path, resolution)) = app.conflict_resolution(entry_idx, file_idx) else {
                app.set_error("no picks to apply on this file");
                return Action::None;
            };
            apply_conflict_resolution(app, entry_idx, path, resolution.content, flags)
        }
        AppAction::ConflictEditFile => {
            // Hand-edit the resolution: picked hunks applied, unpicked
            // hunks as markers. Applied via jj resolve on editor exit.
            let Some((entry_idx, file_idx)) =
                app.rows.get(app.cursor.raw()).and_then(|r| r.dag_file())
            else {
                app.set_error("cursor must be on a conflicted file");
                return Action::None;
            };
            let Some(file) = app.nodes[entry_idx]
                .files
                .files()
                .and_then(|f| f.get(file_idx.raw()))
            else {
                return Action::None;
            };
            if !file.has_conflict {
                app.set_error("no conflict on this file");
                return Action::None;
            }
            let Some((path, content)) = app.conflict_file_content(entry_idx, file_idx) else {
                app.set_error("conflict hunks not loaded: unfold the file first (tab)");
                return Action::None;
            };
            Action::EditConflictFile {
                change_id: app.revision(entry_idx),
                path,
                content,
                flags,
            }
        }
        AppAction::FileUntrack => {
            let paths = app.selected_file_paths();
            if paths.is_empty() {
                app.set_error("no files selected");
                return Action::None;
            }
            Action::run(JJCommand {
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
            Action::run(cmd)
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
        AppAction::Diffedit => make_command(app, |id| JJCommand {
            kind: JJCommandKind::Diffedit {
                change_id: id,
                selection: build_change_selection(app),
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
        AppAction::SquashInto => {
            enter_target_select(app, TargetOperation::Squash(SquashKind::Into), flags)
        }
        AppAction::SquashOnto => {
            enter_target_select(app, TargetOperation::Squash(SquashKind::Onto), flags)
        }
        AppAction::SquashAfter => {
            enter_target_select(app, TargetOperation::Squash(SquashKind::After), flags)
        }
        AppAction::SquashBefore => {
            enter_target_select(app, TargetOperation::Squash(SquashKind::Before), flags)
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
            let Some(change_id) = app.selected_change_id() else {
                return Action::None;
            };
            app.mode = AppMode::text_input(
                "create bookmark: ",
                "",
                PendingCommand::BookmarkCreate { change_id, flags },
            );
            Action::None
        }
        AppAction::BookmarkAdvance => enter_bookmark_advance(app, flags),
        AppAction::Undo => Action::run(JJCommand {
            kind: JJCommandKind::Undo,
            flags,
        }),
        AppAction::Redo => Action::run(JJCommand {
            kind: JJCommandKind::Redo,
            flags,
        }),

        AppAction::GitPush => with_remote(
            app,
            "push to remote",
            RemoteCommand::Push { all: false },
            flags,
        ),
        AppAction::GitPushAll => with_remote(
            app,
            "push all to remote",
            RemoteCommand::Push { all: true },
            flags,
        ),
        AppAction::GitPushChange => {
            let Some(change_id) = app.selected_change_id() else {
                return Action::None;
            };
            Action::run(JJCommand {
                kind: JJCommandKind::GitPushChange {
                    change_id,
                    remote: None,
                },
                flags,
            })
        }
        AppAction::GitExport => Action::run(JJCommand {
            kind: JJCommandKind::GitExport,
            flags,
        }),
        AppAction::GitImport => Action::run(JJCommand {
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
        AppAction::ArrangeUp => arrange(app, flags, crate::types::ArrangeDirection::Up),
        AppAction::ArrangeDown => arrange(app, flags, crate::types::ArrangeDirection::Down),
        AppAction::Revert => {
            let sources = app.selected_change_ids();
            enter_target_select(app, TargetOperation::Revert { sources }, flags)
        }

        other => unreachable!("{other:?} is not routed to this view"),
    }
}

fn make_multi_command(app: &App, build: impl FnOnce(SmallVec<RevisionArg>) -> JJCommand) -> Action {
    let ids = app.selected_change_ids();
    if ids.is_empty() {
        return Action::None;
    }
    Action::run(build(ids))
}

fn make_command(app: &App, build: impl FnOnce(RevisionArg) -> JJCommand) -> Action {
    if app.selection_kind() == SelectionKind::Commit && app.selection_active() {
        return Action::None;
    }
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    Action::run(build(change_id))
}

fn enter_run_input(app: &mut App, flags: CommandFlags) -> Action {
    let ids = app.selected_change_ids();
    if ids.is_empty() {
        return Action::None;
    }
    // Without presets or history there is nothing to pick from: go
    // straight to the free-text input.
    if app.config.run.presets.is_empty() && app.run_history.is_empty() {
        app.mode = crate::input::modal::run_command_input(ids, flags, "");
        return Action::None;
    }
    let mut items: Vec<String> = app.config.run.presets.to_vec();
    items.extend(
        app.run_history
            .iter()
            .filter(|c| !app.config.run.presets.contains(c))
            .cloned(),
    );
    app.mode = AppMode::select_from_list_with_custom(
        "run command",
        "enter command\u{2026}",
        crate::input::modal::run_custom_entry(ids.clone(), flags),
        items,
        PendingSelection::RunCommand {
            change_ids: ids,
            flags,
        },
    );
    Action::None
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
        app.set_error(format!(
            "arrange: commit must have exactly one {noun}, not {}",
            neighbors.len()
        ));
        return Action::None;
    }

    let target_id = app.nodes[neighbors[0]].commit.unique_prefix();
    Action::run(JJCommand {
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

fn picked_term_is_absent(
    app: &App,
    hunk: crate::types::ConflictHunkRef,
    pick: crate::conflict::ConflictTermKind,
) -> bool {
    app.conflict_term(hunk, pick).is_some_and(|t| t.absent)
}

/// Complete a hand-edit of one conflict hunk: validate, store it as the
/// hunk's pick, and offer to apply if the file is now fully picked.
/// Addressed by stable IDs so it survives reloads while the editor was
/// open: a vanished commit is reported, never silently dropped.
pub fn complete_hunk_edit(
    app: &mut App,
    commit_id: &crate::types::CommitId,
    path: &crate::types::RepoPath,
    hunk_idx: crate::idx::ConflictHunkIdx,
    edited: &str,
    flags: CommandFlags,
) {
    if crate::repo::has_conflict_markers(edited) {
        app.set_error("markers remain: resolve the hunk fully or cancel");
        return;
    }
    let Some(hunk) = app.resolve_conflict_hunk(commit_id, path, hunk_idx) else {
        app.set_error("commit changed while editing: edit not applied");
        return;
    };
    app.set_conflict_edited(
        hunk,
        crate::conflict::ConflictText::from_bytes(edited.as_bytes()),
    );
    maybe_offer_apply(app, hunk.entry_idx, hunk.file_idx, flags);
}

/// After a pick, offer to apply immediately when every hunk in the file
/// is picked: Enter (or `a`) applies, Esc keeps accumulating. Returns
/// whether the prompt was shown.
pub(in crate::input) fn maybe_offer_apply(
    app: &mut App,
    entry_idx: crate::idx::EntryIdx,
    file_idx: crate::idx::FileIdx,
    flags: CommandFlags,
) -> bool {
    let Some((path, resolution)) = app.conflict_resolution(entry_idx, file_idx) else {
        return false;
    };
    if !resolution.complete {
        return false;
    }
    // Carry the content, not a temp file: declining the prompt must leave
    // nothing to clean up. The file is staged only when the option runs.
    app.mode = AppMode::FollowUp {
        prompt: format!("all conflicts in {path} picked"),
        options: vec![FollowUpOption {
            key: 'a',
            label: "apply resolution",
            action: FollowUpAction::ResolveConflict {
                change_id: app.revision(entry_idx),
                path,
                content: resolution.content,
                flags,
            },
        }],
        origin: None,
    };
    true
}

/// Stage resolution `content` to a temp file and wrap it in the
/// `jj resolve` command that applies it via the `--apply-resolution` merge
/// tool. The sole constructor of `ResolveTool::Content` commands; reports
/// a staging failure on `app` and returns `None`. Content containing
/// markers stays conflicted: jj parses them back.
pub fn staged_resolution(
    app: &mut App,
    change_id: RevisionArg,
    path: &crate::types::RepoPath,
    content: &str,
    flags: CommandFlags,
) -> Option<JJCommand> {
    match persist_resolved_content(content) {
        Ok(content_path) => Some(JJCommand {
            kind: JJCommandKind::Resolve {
                change_id,
                path: Str::from(path.as_str()),
                tool: crate::jj_command::ResolveTool::Content(content_path),
            },
            flags,
        }),
        Err(e) => {
            app.set_error(format!("failed to stage resolution for `{path}`: {e}"));
            None
        }
    }
}

/// Apply accumulated picks by running `jj resolve` with a merge tool that
/// copies the assembled content into place. Going through jj (rather than
/// writing to the working copy) resolves the conflict in the commit it
/// actually lives in, rebases descendants, and records one undoable
/// operation. Partially picked files keep conflict markers, which jj
/// parses back into a conflicted state.
fn apply_conflict_resolution(
    app: &mut App,
    entry_idx: crate::idx::EntryIdx,
    path: crate::types::RepoPath,
    content: String,
    flags: CommandFlags,
) -> Action {
    let change_id = app.revision(entry_idx);
    match staged_resolution(app, change_id, &path, &content, flags) {
        Some(cmd) => Action::run(cmd),
        None => Action::None,
    }
}

/// Persist resolved file content to a temp file for the
/// `--apply-resolution` merge tool, which removes it after applying.
fn persist_resolved_content(content: &str) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write as _;
    let mut tmp = tempfile::NamedTempFile::new()?;
    tmp.write_all(content.as_bytes())?;
    tmp.flush()?;
    let (_, path) = tmp.keep().map_err(|e| e.error)?;
    Ok(path)
}
