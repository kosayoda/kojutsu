use smallvec::smallvec;

use crate::app::{App, AppMode};
use crate::jj_command::{InsertPosition, JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::{
    ArrangeDirection, ChangeSelection, CommandPrompt, FollowUpAction, FollowUpOption, MessageMode,
    PendingSelection, RebaseKind, RebaseSource, RebaseTarget, RemoteCommand, RevisionArg,
    SelectionKind, SmallVec, SplitKind, SquashKind, Str, TargetOperation,
};

use crate::input::Action;
use crate::input::action::{
    enter_target_select, selection_scope, selection_targets, working_copy_selection,
};
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
        AppAction::Absorb => selection_command(app, |id, selection| JJCommand {
            kind: JJCommandKind::Absorb {
                from: Some(id),
                selection,
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
        AppAction::Fix => match selection_targets(app) {
            Some((ids, selection)) => Action::run(JJCommand {
                kind: JJCommandKind::Fix {
                    change_ids: ids,
                    selection,
                },
                flags,
            }),
            None => Action::None,
        },
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
            let Some(file) = app.dag.nodes[entry_idx]
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
            let Some(path) = app.dag.nodes[hunk.entry_idx]
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
            let Some(file) = app.dag.nodes[entry_idx]
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
            let Some(selection) = working_copy_selection(app) else {
                return Action::None;
            };
            Action::run(JJCommand {
                kind: JJCommandKind::Commit {
                    message: None,
                    selection,
                },
                flags,
            })
        }
        AppAction::CommitWithMessage => {
            let Some(selection) = working_copy_selection(app) else {
                return Action::None;
            };
            app.mode = AppMode::text_input(
                "commit message: ",
                "",
                CommandPrompt::Commit { flags, selection },
            );
            Action::None
        }
        AppAction::Describe => enter_describe_input(app, flags),
        AppAction::DescribeInEditor => make_command(app, |id| JJCommand {
            kind: JJCommandKind::DescribeInEditor { change_id: id },
            flags,
        }),
        AppAction::Diffedit => selection_command(app, |id, selection| JJCommand {
            kind: JJCommandKind::Diffedit {
                change_id: id,
                selection,
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
            // A merge has no one parent to squash into, so ask for the target.
            let source = match app.selection_owner() {
                Some(owner) => app.entry_by_commit_id(owner),
                None => app.selected_entry_idx(),
            };
            if source.is_some_and(|entry| app.dag.nodes[entry].commit.is_merge) {
                enter_target_select(app, TargetOperation::Squash(SquashKind::Into), flags)
            } else {
                selection_command(app, |id, selection| JJCommand {
                    kind: JJCommandKind::Squash {
                        change_id: id,
                        target: None,
                        message: MessageMode::Default,
                        selection,
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
        AppAction::Converge => converge(app, flags),
        AppAction::RebaseRevision => enter_target_select(
            app,
            TargetOperation::Rebase {
                source_mode: RebaseSource::Revision,
            },
            flags,
        ),
        AppAction::RebaseSource => enter_target_select(
            app,
            TargetOperation::Rebase {
                source_mode: RebaseSource::Source,
            },
            flags,
        ),
        AppAction::RebaseBranch => enter_target_select(
            app,
            TargetOperation::Rebase {
                source_mode: RebaseSource::Branch,
            },
            flags,
        ),
        AppAction::Restore => selection_command(app, |id, selection| JJCommand {
            kind: JJCommandKind::Restore {
                from: None,
                into: None,
                changes_in: Some(id),
                selection,
            },
            flags,
        }),
        AppAction::RestoreFrom => enter_target_select(app, TargetOperation::RestoreFrom, flags),
        AppAction::RestoreInto => enter_target_select(app, TargetOperation::RestoreInto, flags),
        AppAction::Split => selection_command(app, |id, selection| JJCommand {
            kind: JJCommandKind::Split {
                change_id: id,
                target: None,
                selection,
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
                CommandPrompt::BookmarkCreate { change_id, flags },
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
        AppAction::DuplicateOnto => enter_target_select(app, TargetOperation::DuplicateOnto, flags),

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
        AppAction::Revert => enter_target_select(app, TargetOperation::Revert, flags),

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

/// A one-commit command taking the selection, built for the commit the
/// selection is in (see [`selection_scope`]).
fn selection_command(
    app: &mut App,
    build: impl FnOnce(RevisionArg, ChangeSelection) -> JJCommand,
) -> Action {
    match selection_scope(app) {
        Some((id, selection)) => Action::run(build(id, selection)),
        None => Action::None,
    }
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

/// Converge the divergent changes in view: those of the selected commits, or
/// else the cursor's. When none of them is divergent, jj's own `converge`
/// revset decides, asking which change if it finds several.
fn converge(app: &mut App, flags: CommandFlags) -> Action {
    let mut changes: SmallVec<crate::types::ChangeId> = SmallVec::new();
    for entry in app.target_entries() {
        let commit = &app.dag.nodes[entry].commit;
        let change = commit.change_id.change_id();
        if commit.is_divergent() && !changes.contains(&change) {
            changes.push(change);
        }
    }
    Action::run(JJCommand {
        kind: JJCommandKind::Converge { changes },
        flags,
    })
}

fn enter_run_input(app: &mut App, flags: CommandFlags) -> Action {
    // jj refuses the two together: one discards the changes the other keeps.
    if flags.contains(CommandFlags::IGNORE_CHANGES | CommandFlags::RESTORE_DESCENDANTS) {
        app.set_error("run: ignore changes and restore descendants can't be combined");
        return Action::None;
    }
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
        CommandPrompt::Describe {
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
    let node = &app.dag.nodes[entry_idx];
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

    let target_id = app.dag.nodes[neighbors[0]].commit.unique_prefix();
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
        prompt: crate::app::FollowUpPrompt::Text(format!("all conflicts in {path} picked")),
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

#[cfg(test)]
mod selection_owner_tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{App, AppMode};
    use crate::dag::{CommitInfo, WorkspaceAnnotation};
    use crate::input::{Action, handle_key};
    use crate::jj_command::JJCommandKind;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
    use crate::lua::LuaEngine;
    use crate::types::{
        ChangeSelection, CommitId, FileRef, RepoPath, RevisionArg, Selection, WorkspaceName,
    };

    fn press(app: &mut App, key: char) -> Action {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let lua = LuaEngine::for_test();
        handle_key(
            app,
            &keymaps,
            &lua,
            KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
        )
    }

    /// `@` (change `ww…`, commit `w1`) above `b` (change `bb…`, commit
    /// `b1`), with file `f` of `b` selected and the cursor moved on to `@`.
    fn files_selected_in_b() -> App {
        let mut app = App::for_test();
        let mut working_copy = CommitInfo::for_test("wwnnomkxrqvlypszwlwkvvqnstvzoxrs", "w1");
        working_copy.workspaces.push(WorkspaceAnnotation {
            name: WorkspaceName::new("default"),
            is_current: true,
        });
        let at = app.push_test_commit(working_copy);
        app.push_test_commit(CommitInfo::for_test(
            "bbnnomkxrqvlypszwlwkvvqnstvzoxrs",
            "b1",
        ));
        for node in app.dag.nodes.iter_mut() {
            node.commit.change_id.set_prefix_len(2);
        }
        app.rebuild_rows();
        app.selection.insert(Selection::File(FileRef {
            commit_id: CommitId::new("b1"),
            path: RepoPath::new("f"),
        }));
        app.cursor = app.row_of_commit(at).unwrap();
        app
    }

    /// The selected paths are `b`'s, so squashing them squashes `b`, not
    /// the commit the cursor happens to be on now.
    #[test]
    fn squashing_a_file_selection_acts_on_its_own_commit() {
        let mut app = files_selected_in_b();
        press(&mut app, 's');
        let Action::RunJj { cmd, .. } = press(&mut app, 's') else {
            panic!("expected a command");
        };
        let JJCommandKind::Squash {
            change_id,
            selection: ChangeSelection::Files(paths),
            ..
        } = cmd.kind
        else {
            panic!("expected a squash of files");
        };
        assert_eq!(change_id.as_str(), "bb");
        assert_eq!(paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(), ["f"]);
    }

    /// Choosing a target first still starts from the selection's commit.
    #[test]
    fn a_squash_into_a_target_starts_from_the_selections_commit() {
        let mut app = files_selected_in_b();
        press(&mut app, 's');
        press(&mut app, 't');
        let AppMode::TargetSelect { picks, .. } = &app.mode else {
            panic!("expected target selection");
        };
        assert_eq!(picks.sources.as_slice(), [RevisionArg::new("bb")]);
    }

    /// `jj commit` only takes the working copy's changes, so paths chosen
    /// in another commit are refused rather than applied to `@`.
    #[test]
    fn committing_refuses_a_selection_from_another_commit() {
        let mut app = files_selected_in_b();
        press(&mut app, 'c');
        assert!(matches!(press(&mut app, 'c'), Action::None));
        assert!(app.status_message.is_some());
    }
}

#[cfg(test)]
mod multi_source_tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{App, AppMode};
    use crate::dag::CommitInfo;
    use crate::idx::EntryIdx;
    use crate::input::{Action, handle_key};
    use crate::jj_command::JJCommandKind;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
    use crate::lua::LuaEngine;
    use crate::types::{FollowUpAction, RevisionArg};

    fn press(app: &mut App, code: KeyCode) -> Action {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let lua = LuaEngine::for_test();
        handle_key(app, &keymaps, &lua, KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn keys(app: &mut App, keys: &str) {
        for c in keys.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// Commits `aa`..`dd`, with `aa`, `bb` and `cc` selected, rebasing by
    /// revision, the cursor back on `aa`.
    fn rebasing_three() -> App {
        let mut app = App::for_test();
        for tag in ['a', 'b', 'c', 'd'] {
            let mut commit = CommitInfo::for_test(
                &format!("{tag}{tag}nnomkxrqvlypszwlwkvvqnstvzoxrs"),
                &format!("{tag}1"),
            );
            commit.change_id.set_prefix_len(2);
            app.push_test_commit(commit);
        }
        app.rebuild_rows();
        for i in 0..3 {
            app.toggle_commit_selection(EntryIdx::new(i));
        }
        keys(&mut app, "rr");
        app
    }

    fn sources(app: &App) -> Vec<&str> {
        let AppMode::TargetSelect { picks, .. } = &app.mode else {
            panic!("expected target selection");
        };
        picks.sources.iter().map(RevisionArg::as_str).collect()
    }

    /// The prompt and the highlight read the commits the rebase will move,
    /// every selected one, not only the one under the cursor.
    #[test]
    fn a_multi_commit_rebase_selects_from_every_selected_commit() {
        let app = rebasing_three();
        assert_eq!(sources(&app), ["aa", "bb", "cc"]);
    }

    /// Rebasing a commit onto itself only fails in jj; it's refused here,
    /// with Space and with Enter, and target selection carries on.
    #[test]
    fn a_source_is_refused_as_a_target() {
        let mut app = rebasing_three();
        app.cursor = app.row_of_commit(EntryIdx::new(1)).unwrap();

        press(&mut app, KeyCode::Char(' '));
        assert!(app.status_message.is_some());
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::None));
        assert!(matches!(app.mode, AppMode::TargetSelect { .. }));
    }

    /// Confirming on another commit rebases all three onto it.
    #[test]
    fn confirming_rebases_every_source() {
        let mut app = rebasing_three();
        app.cursor = app.row_of_commit(EntryIdx::new(3)).unwrap();

        let action = press(&mut app, KeyCode::Enter);
        let options = match (&action, &app.mode) {
            (_, AppMode::FollowUp { options, .. }) => options,
            _ => panic!("expected the rebase follow-up"),
        };
        let FollowUpAction::Execute(cmd) = &options[0].action else {
            panic!("expected a command");
        };
        let JJCommandKind::Rebase { change_ids, .. } = &cmd.kind else {
            panic!("expected a rebase");
        };
        let ids: Vec<&str> = change_ids.iter().map(RevisionArg::as_str).collect();
        assert_eq!(ids, ["aa", "bb", "cc"]);
    }

    /// The follow-up asking how to rebase keeps what was picked, so the
    /// DAG can go on marking it: the three sources, and the targets in the
    /// order they were picked.
    #[test]
    fn the_follow_up_keeps_what_was_picked() {
        let mut app = rebasing_three();
        app.cursor = app.row_of_commit(EntryIdx::new(3)).unwrap();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Enter);

        assert!(matches!(app.mode, AppMode::FollowUp { .. }));
        let picks = app.mode.picks().expect("the follow-up keeps the picks");
        let sources: Vec<&str> = picks.sources.iter().map(RevisionArg::as_str).collect();
        let targets: Vec<&str> = picks.targets.iter().map(RevisionArg::as_str).collect();
        assert_eq!(sources, ["aa", "bb", "cc"]);
        assert_eq!(targets, ["dd"]);
    }

    /// Picking a target is a detour: confirming puts the cursor back on
    /// what the command acts on, for the reload to follow it from there.
    #[test]
    fn confirming_returns_the_cursor_to_where_it_started() {
        let mut app = rebasing_three();
        app.cursor = app.row_of_commit(EntryIdx::new(3)).unwrap();

        press(&mut app, KeyCode::Enter);

        assert!(matches!(app.mode, AppMode::FollowUp { .. }));
        assert_eq!(app.selected_entry_idx(), Some(EntryIdx::new(0)));
    }

    /// Rows can shift while targets are picked (a diff arriving above the
    /// start, say): Esc goes back to the row the cursor was on, not to the
    /// number it had.
    #[test]
    fn esc_returns_to_the_starting_commit_after_rows_shift() {
        use crate::dag::{DiffSummary, FileChange, LineStats};
        use crate::types::CommitId;

        let mut app = rebasing_three();
        let a = EntryIdx::new(0);
        app.dag.nodes[a].files.set_summary(Ok(DiffSummary {
            files: vec![FileChange::for_test("f")],
            stats: LineStats::default(),
        }));
        app.dag.unfolded_commits.insert(CommitId::new("a1"));
        // Start on `cc`, then open `aa` above it.
        press(&mut app, KeyCode::Esc);
        app.cursor = app.row_of_commit(EntryIdx::new(2)).unwrap();
        keys(&mut app, "rr");
        app.rebuild_rows();
        app.cursor = app.row_of_commit(EntryIdx::new(3)).unwrap();

        press(&mut app, KeyCode::Esc);

        assert_eq!(app.selected_entry_idx(), Some(EntryIdx::new(2)));
    }
}

#[cfg(test)]
mod visual_select_tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::App;
    use crate::dag::CommitInfo;
    use crate::input::handle_key;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
    use crate::lua::LuaEngine;

    /// `v`, two steps down, Space: the three commits the range covers are
    /// selected at once.
    #[test]
    fn space_selects_the_whole_visual_commit_range() {
        let mut app = App::for_test();
        for tag in ['a', 'b', 'c', 'd'] {
            app.push_test_commit(CommitInfo::for_test(
                &format!("{tag}{tag}nnomkxrqvlypszwlwkvvqnstvzoxrs"),
                &format!("{tag}1"),
            ));
        }
        app.rebuild_rows();
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let lua = LuaEngine::for_test();
        for key in ['v', 'j', 'j', ' '] {
            let event = KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE);
            handle_key(&mut app, &keymaps, &lua, event);
        }

        assert_eq!(app.selection_count(), 3);
        assert_eq!(
            app.selection.display_text().as_deref(),
            Some("3 commits selected")
        );
    }
}

#[cfg(test)]
mod run_input_tests {
    use crate::app::{App, AppMode};
    use crate::dag::CommitInfo;
    use crate::idx::EntryIdx;
    use crate::input::Action;
    use crate::jj_command::JJCommandKind;
    use crate::keymap::CommandFlags;
    use crate::types::RevisionArg;

    fn two_commits_selected() -> App {
        let mut app = App::for_test();
        for tag in ['a', 'b'] {
            app.push_test_commit(CommitInfo::for_test(
                &format!("{tag}{tag}nnomkxrqvlypszwlwkvvqnstvzoxrs"),
                &format!("{tag}1"),
            ));
        }
        app.rebuild_rows();
        for i in 0..2 {
            app.toggle_commit_selection(EntryIdx::new(i));
        }
        app
    }

    fn revisions() -> crate::types::SmallVec<RevisionArg> {
        smallvec::smallvec![RevisionArg::new("a1"), RevisionArg::new("b1")]
    }

    /// jj refuses the pair, so it's refused before a command is typed.
    #[test]
    fn ignoring_changes_while_restoring_descendants_is_refused() {
        let mut app = two_commits_selected();
        let flags = CommandFlags::IGNORE_CHANGES | CommandFlags::RESTORE_DESCENDANTS;
        assert!(matches!(
            super::enter_run_input(&mut app, flags),
            Action::None
        ));
        assert!(app.status_message.is_some());
        assert!(matches!(app.mode, AppMode::Normal));
    }

    /// Several revisions normally ask for a job count; passthrough allows
    /// only one job, so it runs with one straight away.
    #[test]
    fn passthrough_skips_the_job_count() {
        let mut app = two_commits_selected();
        let action = crate::input::modal::submit_run_command(
            &mut app,
            revisions(),
            CommandFlags::PASSTHROUGH,
            "cargo test".into(),
        );
        let Action::RunJj { cmd, .. } = action else {
            panic!("expected the run to start");
        };
        assert!(matches!(cmd.kind, JJCommandKind::Run { jobs: Some(1), .. }));

        let action = crate::input::modal::submit_run_command(
            &mut app,
            revisions(),
            CommandFlags::empty(),
            "cargo test".into(),
        );
        assert!(matches!(action, Action::None));
        assert!(matches!(app.mode, AppMode::TextInput { .. }));
    }
}

#[cfg(test)]
mod converge_input_tests {
    use crate::app::App;
    use crate::dag::{CommitInfo, DivergenceInfo};
    use crate::idx::EntryIdx;
    use crate::input::Action;
    use crate::jj_command::JJCommandKind;
    use crate::keymap::CommandFlags;

    const CHANGE: &str = "uunnomkxrqvlypszwlwkvvqnstvzoxrs";

    /// Two copies of change `uu`, then an ordinary commit.
    fn with_divergence() -> App {
        let mut app = App::for_test();
        for (commit, suffix) in [("u1", Some(1)), ("u2", Some(2))] {
            let mut info = CommitInfo::for_test(CHANGE, commit);
            info.divergence = suffix.map(|s| DivergenceInfo {
                is_divergent: true,
                is_hidden: false,
                suffix: Some(s),
            });
            app.push_test_commit(info);
        }
        app.push_test_commit(CommitInfo::for_test(
            "bbnnomkxrqvlypszwlwkvvqnstvzoxrs",
            "b1",
        ));
        app.rebuild_rows();
        app
    }

    fn changes(action: Action) -> Vec<String> {
        let Action::RunJj { cmd, .. } = action else {
            panic!("expected a command");
        };
        let JJCommandKind::Converge { changes } = cmd.kind else {
            panic!("expected converge");
        };
        changes.iter().map(|c| c.to_string()).collect()
    }

    /// On a divergent copy, converge is aimed at that change.
    #[test]
    fn converge_aims_at_the_divergent_change_under_the_cursor() {
        let mut app = with_divergence();
        app.cursor = app.row_of_commit(EntryIdx::new(1)).unwrap();
        assert_eq!(
            changes(super::converge(&mut app, CommandFlags::empty())),
            [CHANGE]
        );
    }

    /// Elsewhere there's nothing in view to aim at, so jj's own converge
    /// revset decides.
    #[test]
    fn off_a_divergent_commit_converge_is_left_to_jj() {
        let mut app = with_divergence();
        app.cursor = app.row_of_commit(EntryIdx::new(2)).unwrap();
        assert!(changes(super::converge(&mut app, CommandFlags::empty())).is_empty());
    }

    /// Both copies selected name the change once.
    #[test]
    fn selected_copies_of_one_change_name_it_once() {
        let mut app = with_divergence();
        for i in 0..2 {
            app.toggle_commit_selection(EntryIdx::new(i));
        }
        assert_eq!(
            changes(super::converge(&mut app, CommandFlags::empty())),
            [CHANGE]
        );
    }
}
