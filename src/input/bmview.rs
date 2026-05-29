use smallvec::smallvec;

use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::{ChangeId, PendingCommand, PendingSelection, RemoteName, TargetOperation};

use super::action::{enter_target_select, jump_to_commit_in_dag};
use super::Action;

pub(super) fn dispatch(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
    match action {
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
                Action::SuspendAndRunJj(JJCommand {
                    kind: JJCommandKind::GitPushBookmark {
                        bookmarks: smallvec![name],
                        remote: None,
                    },
                    flags,
                })
            }
        }
        AppAction::BmViewJumpToCommit => {
            // On a conflict target row: resolve by setting the bookmark there.
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
        _ => Action::None,
    }
}
