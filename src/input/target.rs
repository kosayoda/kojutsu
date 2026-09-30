//! Actions that act on whatever the cursor is on, in any view that shows
//! it: a revision, a commit, bookmarks, tags or workspaces. Each is written
//! once against the `App::target_*` accessors, and a view with nothing for
//! it says so rather than ignoring the key.

use smallvec::smallvec;

use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::ActiveView;
use crate::types::{
    CommandPrompt, CommitId, PendingSelection, RemoteCommand, SmallVec, Str, TargetOperation,
};

use super::Action;
use super::action::{enter_target_select, jump_to_commit_in_dag};
use super::bookmark::PendingSelectionKind;

/// Report that `action` has nothing to act on under the cursor.
fn nothing_here(app: &mut App, action: AppAction) -> Action {
    app.set_error(format!("nothing to {} here", action.label()));
    Action::None
}

pub(super) fn edit(app: &mut App, flags: CommandFlags) -> Action {
    let Some(change_id) = app.target_revision() else {
        return nothing_here(app, AppAction::Edit);
    };
    Action::run(JJCommand {
        kind: JJCommandKind::Edit { change_id },
        flags,
    })
}

pub(super) fn new(app: &mut App, flags: CommandFlags) -> Action {
    let change_ids = app.target_revisions();
    if change_ids.is_empty() {
        return nothing_here(app, AppAction::New);
    }
    Action::run(JJCommand {
        kind: JJCommandKind::New {
            change_ids,
            insert: None,
        },
        flags,
    })
}

pub(super) fn jump_to_commit(app: &mut App, flags: CommandFlags) -> Action {
    // On one side of a conflicted bookmark, "going there" resolves the
    // conflict by setting the bookmark to that side.
    if let Some((entry, target)) = app.selected_conflict_target() {
        return Action::run(JJCommand {
            kind: JJCommandKind::BookmarkSet {
                name: entry.name.clone(),
                change_id: target.summary.revision(),
            },
            flags: flags | CommandFlags::ALLOW_BACKWARDS,
        });
    }
    let Some((commit_id, change_id)) = app.target_commit() else {
        return nothing_here(app, AppAction::JumpToCommit);
    };
    jump_to_commit_in_dag(app, &commit_id, change_id.as_ref());
    Action::None
}

/// Compare the change under the cursor with its natural counterpart: a
/// commit picked in the DAG, the current version of an evolog step, the
/// remote side of a bookmark.
pub(super) fn interdiff(app: &mut App, flags: CommandFlags) -> Action {
    match app.active_view {
        ActiveView::Dag => enter_target_select(app, TargetOperation::Interdiff, flags),
        ActiveView::Evolog => evolog_interdiff(app),
        ActiveView::Bookmarks => bookmark_interdiff(app),
        _ => nothing_here(app, AppAction::Interdiff),
    }
}

fn evolog_interdiff(app: &mut App) -> Action {
    let Some(entry) = app.selected_evolog_entry() else {
        return nothing_here(app, AppAction::Interdiff);
    };
    if entry.is_current {
        app.set_error("already on current version");
        return Action::None;
    }
    let from = entry.commit_id.clone();
    let from_label = Str::from(entry.change_id.display());
    let Some(current) = app.evolog.entries.iter().find(|e| e.is_current) else {
        app.set_error("no current version found");
        return Action::None;
    };
    let to = current.commit_id.clone();
    let to_label = Str::from(current.change_id.display());
    app.enter_interdiff_view(from, to, from_label, to_label);
    Action::None
}

fn bookmark_interdiff(app: &mut App) -> Action {
    let Some(entry) = app.selected_bookmark_entry() else {
        return nothing_here(app, AppAction::Interdiff);
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
    let remote_commit: Option<CommitId> = app
        .views
        .bookmark_details
        .get(&name)
        .and_then(|d| d.remote_targets.first())
        .map(|t| t.summary.commit_id.clone());
    let Some(remote_commit) = remote_commit else {
        app.set_error("no remote target to diff against");
        return Action::None;
    };
    let from_label = Str::from(format!("{name}@remote"));
    let to_label = Str::from(name.to_string());
    app.enter_interdiff_view(remote_commit, local_commit, from_label, to_label);
    Action::None
}

/// Delete, forget, move or rename one of the target bookmarks, asking which
/// when there are several.
pub(super) fn bookmark(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    flags: CommandFlags,
    kind: PendingSelectionKind,
) -> Action {
    let names = app.target_bookmarks();
    if names.is_empty() {
        app.set_status("no bookmarks here");
        return Action::None;
    }
    let on_select = match kind {
        PendingSelectionKind::Delete => PendingSelection::BookmarkDelete { flags },
        PendingSelectionKind::Forget => PendingSelection::BookmarkForget { flags },
        PendingSelectionKind::Rename => PendingSelection::BookmarkRename { flags },
        PendingSelectionKind::Move => {
            let Some(source) = app.target_revision() else {
                app.set_error("bookmark has no associated commit");
                return Action::None;
            };
            PendingSelection::BookmarkMove { source, flags }
        }
    };
    let items: SmallVec<String> = names.iter().map(|n| n.to_string()).collect();
    if items.len() == 1 {
        return super::list::resolve_selection(app, lua, on_select, items);
    }
    app.mode = AppMode::select_from_list(
        kind.title(),
        items.into_vec(),
        kind.is_multi(),
        on_select,
        false,
    );
    Action::None
}

/// Point a bookmark at a commit, asking for whichever half the cursor
/// doesn't give: the name for a commit, the commit for a bookmark.
pub(super) fn bookmark_set(app: &mut App, flags: CommandFlags) -> Action {
    match app.active_view {
        ActiveView::Bookmarks => {
            let Some(entry) = app.selected_bookmark_entry() else {
                return nothing_here(app, AppAction::BookmarkSet);
            };
            let name = entry.name.clone();
            app.mode = AppMode::text_input(
                format!("set {name} to (change id): "),
                "",
                CommandPrompt::BookmarkSetByName { name, flags },
            );
        }
        _ => {
            let Some(change_id) = app.target_revision() else {
                return nothing_here(app, AppAction::BookmarkSet);
            };
            app.mode = AppMode::text_input(
                "set bookmark: ",
                "",
                CommandPrompt::BookmarkSet { change_id, flags },
            );
        }
    }
    Action::None
}

/// Track (or untrack) a remote bookmark: the one under the cursor in the
/// bookmark view, or one picked from all of them elsewhere.
pub(super) fn bookmark_track(app: &mut App, flags: CommandFlags, track: bool) -> Action {
    let action = if track {
        AppAction::BookmarkTrack
    } else {
        AppAction::BookmarkUntrack
    };
    if app.active_view == ActiveView::Bookmarks {
        let Some(bookmark) = app.selected_bookmark_ref() else {
            app.set_error(if track {
                "bookmark is already local"
            } else {
                "bookmark has no remote to untrack"
            });
            return Action::None;
        };
        let bookmarks = smallvec![bookmark];
        let kind = if track {
            JJCommandKind::BookmarkTrack { bookmarks }
        } else {
            JJCommandKind::BookmarkUntrack { bookmarks }
        };
        return Action::run(JJCommand { kind, flags });
    }
    if app.active_view != ActiveView::Dag {
        return nothing_here(app, action);
    }
    let bookmarks: Vec<String> = app
        .views
        .remote_bookmarks
        .iter()
        .filter(|rb| rb.is_tracked != track)
        .map(|rb| format!("{}@{}", rb.name, rb.remote))
        .collect();
    if bookmarks.is_empty() {
        app.set_status(if track {
            "no untracked remote bookmarks"
        } else {
            "no tracked remote bookmarks"
        });
        return Action::None;
    }
    let (title, on_select) = if track {
        ("track bookmark", PendingSelection::BookmarkTrack { flags })
    } else {
        (
            "untrack bookmark",
            PendingSelection::BookmarkUntrack { flags },
        )
    };
    app.mode = AppMode::select_from_list(title, bookmarks, true, on_select, true);
    Action::None
}

/// Push target bookmarks, asking which when there are several and which
/// remote when there is more than one.
pub(super) fn git_push_bookmark(app: &mut App, flags: CommandFlags) -> Action {
    let names = app.target_bookmarks();
    match names.len() {
        0 => {
            app.set_error("no bookmarks here");
            Action::None
        }
        1 => with_remote(
            app,
            "push bookmark to remote",
            RemoteCommand::PushBookmark { bookmarks: names },
            flags,
        ),
        _ => {
            let items = names.iter().map(|n| n.to_string()).collect();
            app.mode = AppMode::select_from_list(
                "push bookmark",
                items,
                true,
                PendingSelection::GitPushBookmark { flags },
                false,
            );
            Action::None
        }
    }
}

/// Fetch the remote bookmark under the cursor in the bookmark view.
pub(super) fn git_fetch_bookmark(app: &mut App, flags: CommandFlags) -> Action {
    let Some(bookmark) = app.selected_bookmark_ref() else {
        app.set_error("no remote to fetch from");
        return Action::None;
    };
    Action::run(JJCommand {
        kind: JJCommandKind::GitFetchBookmark {
            bookmark: bookmark.name,
            remote: bookmark.remote,
        },
        flags,
    })
}

/// Run a git command against the only remote, or ask which one first.
pub(super) fn with_remote(
    app: &mut App,
    prompt: &str,
    command: RemoteCommand,
    flags: CommandFlags,
) -> Action {
    if app.views.remotes.len() > 1 {
        let items = app.views.remotes.iter().map(|r| r.to_string()).collect();
        app.mode = AppMode::select_from_list(
            prompt,
            items,
            false,
            PendingSelection::GitRemote { command, flags },
            false,
        );
        return Action::None;
    }
    Action::run(JJCommand {
        kind: command.to_kind(None),
        flags,
    })
}

/// Track (or untrack) remote tags: in the tag view, the remote under the
/// cursor or those of the tag under it; elsewhere, any remote tag. Asks
/// which when there are several.
pub(super) fn tag_track(app: &mut App, flags: CommandFlags, track: bool) -> Action {
    let tags = app.remote_tags_to_track(track);
    match tags.len() {
        0 => {
            app.set_status(if track {
                "no untracked remote tags here"
            } else {
                "no tracked remote tags here"
            });
            Action::None
        }
        1 => {
            let tags = tags.into_iter().collect();
            let kind = if track {
                JJCommandKind::TagTrack { tags }
            } else {
                JJCommandKind::TagUntrack { tags }
            };
            Action::run(JJCommand { kind, flags })
        }
        _ => {
            let items = tags
                .iter()
                .map(|t| format!("{}@{}", t.name, t.remote))
                .collect();
            let (title, on_select) = if track {
                ("track tag", PendingSelection::TagTrack { flags })
            } else {
                ("untrack tag", PendingSelection::TagUntrack { flags })
            };
            app.mode = AppMode::select_from_list(title, items, true, on_select, true);
            Action::None
        }
    }
}

/// Parse `"name@remote"` display strings into `TagRef` values.
pub(super) fn parse_remote_tags(names: SmallVec<String>) -> SmallVec<crate::dag::TagRef> {
    super::bookmark::parse_remote_refs(names, |name, remote| crate::dag::TagRef {
        name: crate::types::TagName::new(name),
        remote: crate::types::RemoteName::new(remote),
    })
}

/// Delete target tags, asking which when there are several.
pub(super) fn tag_delete(app: &mut App, flags: CommandFlags) -> Action {
    let names = app.target_tags();
    match names.len() {
        0 => {
            app.set_status("no tags here");
            Action::None
        }
        1 => Action::run(JJCommand {
            kind: JJCommandKind::TagDelete { names },
            flags,
        }),
        _ => {
            let items = names.iter().map(|t| t.to_string()).collect();
            app.mode = AppMode::select_from_list(
                "delete tag",
                items,
                true,
                PendingSelection::TagDelete { flags },
                false,
            );
            Action::None
        }
    }
}

/// Point a tag at a commit, asking for whichever half the cursor doesn't
/// give: the name for a commit, the commit for a tag.
pub(super) fn tag_set(app: &mut App, flags: CommandFlags) -> Action {
    match app.active_view {
        ActiveView::Tags => {
            let Some(entry) = app.selected_tag_entry() else {
                return nothing_here(app, AppAction::TagSet);
            };
            let name = entry.name.clone();
            app.mode = AppMode::text_input(
                format!("set {name} to (change id): "),
                "",
                CommandPrompt::TagSetByName { name, flags },
            );
        }
        _ => {
            let Some(change_id) = app.target_revision() else {
                return nothing_here(app, AppAction::TagSet);
            };
            app.mode =
                AppMode::text_input("set tag: ", "", CommandPrompt::TagSet { change_id, flags });
        }
    }
    Action::None
}

/// Forget target workspaces, asking which when there are several.
pub(super) fn workspace_forget(app: &mut App, flags: CommandFlags) -> Action {
    let names = app.target_workspaces();
    match names.len() {
        0 => {
            app.set_error("no other workspace here");
            Action::None
        }
        1 => Action::run(JJCommand {
            kind: JJCommandKind::WorkspaceForget { names },
            flags,
        }),
        _ => {
            let items = names.iter().map(|w| w.to_string()).collect();
            app.mode = AppMode::select_from_list(
                "forget workspace",
                items,
                true,
                PendingSelection::WorkspaceForget { flags },
                false,
            );
            Action::None
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{App, BookmarkKind, BookmarkViewEntry, TagViewEntry, draw_log};
    use crate::history::EvoLogEntry;
    use crate::input::{Action, handle_key};
    use crate::jj_command::JJCommandKind;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
    use crate::lua::LuaEngine;
    use crate::types::ActiveView;
    use crate::types::{BookmarkName, CommitId, RevisionArg, TagName};

    const COMMIT: &str = "7bbaa2cb";

    fn press(app: &mut App, code: KeyCode) -> Action {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let lua = LuaEngine::for_test();
        handle_key(app, &keymaps, &lua, KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn command(action: Action) -> JJCommandKind {
        match action {
            Action::RunJj { cmd, .. } => cmd.kind,
            _ => panic!("expected a jj command"),
        }
    }

    fn bookmarks_view() -> App {
        let mut app = App::for_test();
        app.views.bookmark_entries.push(BookmarkViewEntry {
            name: BookmarkName::new("main"),
            commit_id: Some(CommitId::new(COMMIT)),
            change_id: None,
            short_commit_id: None,
            revision: Some(RevisionArg::new("qpv")),
            description: None,
            kind: BookmarkKind::Local {
                is_dirty: false,
                is_conflicted: false,
            },
        });
        app.switch_view(ActiveView::Bookmarks);
        app
    }

    fn tags_view() -> App {
        let mut app = App::for_test();
        app.views.tag_entries.push(TagViewEntry {
            name: TagName::new("v1"),
            commit_id: Some(CommitId::new(COMMIT)),
            change_id: None,
            short_commit_id: None,
            revision: Some(RevisionArg::new("qpv")),
            description: None,
            presence: crate::dag::TagPresence::Local,
        });
        app.switch_view(ActiveView::Tags);
        app
    }

    fn evolog_view() -> App {
        let mut app = App::for_test();
        app.evolog.entries = draw_log(
            vec![EvoLogEntry::for_test(CommitId::new(COMMIT), Vec::new())],
            &app.config.glyphs,
        );
        app.active_view = ActiveView::Evolog;
        app.rebuild_rows();
        app
    }

    fn edited(kind: JJCommandKind) -> String {
        match kind {
            JJCommandKind::Edit { change_id } => change_id.to_string(),
            _ => panic!("expected an edit"),
        }
    }

    /// One `edit`, bound to `e` in every view that shows a revision, acts
    /// on whatever that view has under the cursor.
    #[test]
    fn edit_acts_on_each_views_revision() {
        let mut dag = App::with_test_commit("qpvuntsm", COMMIT);
        assert_eq!(
            edited(command(press(&mut dag, KeyCode::Char('e')))),
            "qpvuntsm"
        );
        assert_eq!(
            edited(command(press(&mut bookmarks_view(), KeyCode::Char('e')))),
            "qpv"
        );
        assert_eq!(
            edited(command(press(&mut tags_view(), KeyCode::Char('e')))),
            "qpv"
        );
        assert_eq!(
            edited(command(press(&mut evolog_view(), KeyCode::Char('e')))),
            COMMIT
        );
    }

    #[test]
    fn new_from_an_evolog_step_starts_on_that_version() {
        let kind = command(press(&mut evolog_view(), KeyCode::Char('n')));
        assert!(
            matches!(kind, JJCommandKind::New { change_ids, insert: None }
                if change_ids.as_slice() == [RevisionArg::new(COMMIT)])
        );
    }

    /// The DAG asks which of a commit's bookmarks; the bookmark view has
    /// already said.
    #[test]
    fn deleting_in_the_bookmark_view_takes_the_row() {
        let kind = command(press(&mut bookmarks_view(), KeyCode::Char('d')));
        assert!(matches!(kind, JJCommandKind::BookmarkDelete { names }
                if names.as_slice() == [BookmarkName::new("main")]));
    }

    #[test]
    fn deleting_in_the_tag_view_takes_the_row() {
        let kind = command(press(&mut tags_view(), KeyCode::Char('d')));
        assert!(matches!(kind, JJCommandKind::TagDelete { names }
                if names.as_slice() == [TagName::new("v1")]));
    }

    /// With a single remote there is nothing to ask.
    #[test]
    fn fetch_with_one_remote_runs_straight_away() {
        let mut app = bookmarks_view();
        press(&mut app, KeyCode::Char('f'));
        let kind = command(press(&mut app, KeyCode::Char('f')));
        assert!(matches!(
            kind,
            JJCommandKind::GitFetch {
                all_remotes: false,
                remote: None
            }
        ));
    }

    /// A key with nothing to act on says so instead of doing nothing.
    #[test]
    fn an_action_with_nothing_under_the_cursor_reports_it() {
        let mut app = App::for_test();
        app.switch_view(ActiveView::Operations);
        assert!(matches!(
            super::edit(&mut app, crate::keymap::CommandFlags::empty()),
            Action::None
        ));
        let message = &app.status_message.as_ref().expect("an error").0;
        assert_eq!(message, "nothing to edit here");
    }
}
