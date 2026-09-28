use smallvec::smallvec;

use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::{PendingCommand, PendingSelection, RemoteName, TargetOperation};

use crate::input::Action;
use crate::input::action::{enter_target_select, jump_to_commit_in_dag};

pub(in crate::input) fn dispatch(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
    match action {
        AppAction::BookmarkViewDelete => {
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
        AppAction::BookmarkViewTrack => {
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
        AppAction::BookmarkViewUntrack => {
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
        AppAction::BookmarkViewPush => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            if app.views.remotes.len() > 1 {
                let items = app.views.remotes.iter().map(|r| r.to_string()).collect();
                app.mode = AppMode::select_from_list(
                    "push bookmark to remote",
                    items,
                    false,
                    PendingSelection::GitRemoteForPushBookmark {
                        bookmarks: smallvec![name],
                        flags,
                    },
                    false,
                );
                Action::None
            } else {
                Action::RunJj(JJCommand {
                    kind: JJCommandKind::GitPushBookmark {
                        bookmarks: smallvec![name],
                        remote: None,
                    },
                    flags,
                })
            }
        }
        AppAction::BookmarkViewJumpToCommit => {
            // On a conflict target row: resolve by setting the bookmark there.
            if let Some((entry, target)) = app.selected_conflict_target() {
                let name = entry.name.clone();
                return Action::RunJj(JJCommand {
                    kind: JJCommandKind::BookmarkSet {
                        name,
                        change_id: target.summary.revision(),
                    },
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
        AppAction::BookmarkViewEdit => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let Some(change_id) = entry.revision.clone() else {
                app.set_error("bookmark has no associated commit");
                return Action::None;
            };
            Action::RunJj(JJCommand {
                kind: JJCommandKind::Edit { change_id },
                flags,
            })
        }
        AppAction::BookmarkViewRename => {
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
        AppAction::BookmarkViewMove => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            let bookmark_name = entry.name.clone();
            app.switch_view(crate::app::ActiveView::Dag);
            enter_target_select(app, TargetOperation::BookmarkMove { bookmark_name }, flags)
        }
        AppAction::BookmarkViewForget => {
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
        AppAction::BookmarkViewSet => {
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
        AppAction::BookmarkViewFetchDefault => {
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
                Action::RunJj(JJCommand {
                    kind: JJCommandKind::GitFetch {
                        all_remotes: false,
                        remote: None,
                    },
                    flags,
                })
            }
        }
        AppAction::BookmarkViewFetchBookmark => {
            if let Some((entry, target)) = app.selected_remote_target() {
                return Action::RunJj(JJCommand {
                    kind: JJCommandKind::GitFetchBookmark {
                        bookmark: entry.name.clone(),
                        remote: RemoteName::new(target.remote.as_str()),
                    },
                    flags,
                });
            }
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            if let Some(remote) = entry.kind.remote() {
                return Action::RunJj(JJCommand {
                    kind: JJCommandKind::GitFetchBookmark {
                        bookmark: entry.name.clone(),
                        remote: RemoteName::new(remote.as_str()),
                    },
                    flags,
                });
            }
            app.set_error("no remote to fetch from");
            Action::None
        }
        AppAction::BookmarkViewFetchAllRemotes => Action::RunJj(JJCommand {
            kind: JJCommandKind::GitFetch {
                all_remotes: true,
                remote: None,
            },
            flags,
        }),
        AppAction::BookmarkViewInterdiff => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return Action::None;
            };
            if !entry.kind.is_dirty() {
                app.set_error("bookmark is not dirty");
                return Action::None;
            }
            let Some(local_commit) = entry.commit_id.clone() else {
                app.set_error("bookmark has no local commit");
                return Action::None;
            };
            let name = entry.name.clone();
            let details = app.views.bookmark_details.get(&name);
            let remote_commit = details
                .and_then(|d| d.remote_targets.first())
                .map(|t| t.summary.commit_id.clone());
            let Some(remote_commit) = remote_commit else {
                app.set_error("no remote target to diff against");
                return Action::None;
            };
            let from_label = crate::types::Str::from(format!("{}@remote", name));
            let to_label = crate::types::Str::from(name.to_string());
            app.enter_interdiff_view(remote_commit, local_commit, from_label, to_label);
            Action::None
        }
        _ => Action::None,
    }
}
