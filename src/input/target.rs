//! Actions that act on whatever the cursor is on, in any view that shows
//! it: a revision, a commit, bookmarks, tags or workspaces. Each is written
//! once against the `App::target_*` accessors, and a view with nothing for
//! it says so rather than ignoring the key.

use smallvec::smallvec;

use crate::app::{App, AppMode};
use crate::jj_command::{FollowUpAction, FollowUpOption, JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::ActiveView;
use crate::types::{
    CommandPrompt, CommitId, DisplayRow, PendingSelection, RemoteCommand, RemoteEdit, RemoteName,
    SmallVec, Str, TargetOperation, WorkspaceName,
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
/// remote side of a bookmark or tag.
pub(super) fn interdiff(app: &mut App, flags: CommandFlags) -> Action {
    match app.active_view {
        ActiveView::Dag => enter_target_select(app, TargetOperation::Interdiff, flags),
        ActiveView::Evolog => evolog_interdiff(app),
        ActiveView::Bookmarks => bookmark_interdiff(app),
        ActiveView::Tags => tag_interdiff(app),
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
    let name = entry.name.to_string();
    let local = entry.commit_id.clone();
    let remotes = app
        .views
        .bookmark_details
        .get(&entry.name)
        .map(|d| {
            d.remote_targets
                .iter()
                .map(|t| (t.remote.clone(), t.summary.commit_id.clone()))
                .collect()
        })
        .unwrap_or_default();
    let on_remote = match app.rows.get(app.cursor.raw()) {
        Some(DisplayRow::BookmarkRemoteTarget { target_idx, .. }) => Some(target_idx.raw()),
        _ => None,
    };
    ref_interdiff(app, &name, local, remotes, on_remote)
}

fn tag_interdiff(app: &mut App) -> Action {
    let Some(entry) = app.selected_tag_entry() else {
        return nothing_here(app, AppAction::Interdiff);
    };
    let name = entry.name.to_string();
    let local = entry.commit_id.clone();
    let remotes = app
        .views
        .tag_details
        .get(&entry.name)
        .map(|d| {
            d.remote_targets
                .iter()
                .map(|t| (t.remote.clone(), t.summary.commit_id.clone()))
                .collect()
        })
        .unwrap_or_default();
    let on_remote = match app.rows.get(app.cursor.raw()) {
        Some(DisplayRow::TagRemoteTarget { target_idx, .. }) => Some(target_idx.raw()),
        _ => None,
    };
    ref_interdiff(app, &name, local, remotes, on_remote)
}

/// Compare a bookmark or tag on a remote with the local one: the remote
/// whose row the cursor is on, or else the first pointing somewhere else.
fn ref_interdiff(
    app: &mut App,
    name: &str,
    local: Option<CommitId>,
    remotes: Vec<(RemoteName, CommitId)>,
    on_remote: Option<usize>,
) -> Action {
    let Some(local) = local else {
        app.set_error(format!("{name} has no local commit"));
        return Action::None;
    };
    let remote = match on_remote {
        Some(idx) => remotes.into_iter().nth(idx),
        None => remotes.into_iter().find(|(_, commit)| *commit != local),
    };
    let Some((remote, remote_commit)) = remote else {
        app.set_error(format!("{name} is the same on its remotes"));
        return Action::None;
    };
    if remote_commit == local {
        app.set_error(format!("{name}@{remote} is the same as {name}"));
        return Action::None;
    }
    let from_label = Str::from(format!("{name}@{remote}"));
    let to_label = Str::from(name);
    app.enter_interdiff_view(remote_commit, local, from_label, to_label);
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

/// Change a remote: the only one there is, or the one picked.
pub(super) fn edit_remote(app: &mut App, edit: RemoteEdit, flags: CommandFlags) -> Action {
    if edit == RemoteEdit::SetPushUrl
        && let Some(reason) = app
            .jj
            .refusal(crate::jj_version::JjFeature::SeparatePushUrl)
    {
        app.set_error(reason);
        return Action::None;
    }
    match app.views.remotes.as_slice() {
        [] => {
            app.set_error("no remotes");
            Action::None
        }
        [only] => {
            let name = only.name.clone();
            edit_named_remote(app, name, edit, flags)
        }
        remotes => {
            let items = remotes.iter().map(|r| r.name.to_string()).collect();
            let title = match edit {
                RemoteEdit::Rename => "rename remote",
                RemoteEdit::SetUrl => "set url of remote",
                RemoteEdit::SetPushUrl => "set push url of remote",
                RemoteEdit::Remove => "remove remote",
            };
            app.mode = AppMode::select_from_list(
                title,
                items,
                false,
                PendingSelection::RemoteToEdit { edit, flags },
                false,
            );
            Action::None
        }
    }
}

/// Ask for what changing `name` takes: a new name, a URL starting from the
/// current one, or a confirmation, since removing forgets its bookmarks.
pub(super) fn edit_named_remote(
    app: &mut App,
    name: RemoteName,
    edit: RemoteEdit,
    flags: CommandFlags,
) -> Action {
    let remote = app.views.remotes.iter().find(|r| r.name == name);
    let fetch_url = remote.and_then(|r| r.fetch_url.clone()).unwrap_or_default();
    let push_url = remote.and_then(|r| r.push_url.clone()).unwrap_or_default();
    app.mode = match edit {
        RemoteEdit::Rename => AppMode::text_input(
            format!("rename {name} to: "),
            name.to_string(),
            CommandPrompt::GitRemoteRename { old: name, flags },
        ),
        RemoteEdit::SetUrl => AppMode::text_input(
            format!("{name} url: "),
            fetch_url,
            CommandPrompt::GitRemoteSetUrl {
                name,
                push: false,
                flags,
            },
        ),
        RemoteEdit::SetPushUrl => AppMode::text_input(
            format!("{name} push url: "),
            push_url,
            CommandPrompt::GitRemoteSetUrl {
                name,
                push: true,
                flags,
            },
        ),
        RemoteEdit::Remove => AppMode::FollowUp {
            prompt: crate::app::FollowUpPrompt::Text(format!(
                "remove remote {name} and forget its bookmarks?"
            )),
            options: vec![FollowUpOption {
                key: 'y',
                label: "remove",
                action: FollowUpAction::Execute(JJCommand {
                    kind: JJCommandKind::GitRemoteRemove { name },
                    flags,
                }),
            }],
            origin: None,
        },
    };
    Action::None
}

/// Run a git command against the only remote, or ask which first: several
/// at once where jj takes them.
pub(super) fn with_remote(
    app: &mut App,
    prompt: &str,
    command: RemoteCommand,
    flags: CommandFlags,
) -> Action {
    if app.views.remotes.len() > 1 {
        let items = app
            .views
            .remotes
            .iter()
            .map(|r| r.name.to_string())
            .collect();
        let multi = command.takes_several(app.jj);
        app.mode = AppMode::select_from_list(
            prompt,
            items,
            multi,
            PendingSelection::GitRemote { command, flags },
            false,
        );
        return Action::None;
    }
    Action::run(JJCommand {
        kind: command.to_kind(SmallVec::new()),
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

/// Remove target workspaces other than this one, asking which when there
/// are several, and confirming before their directories are deleted.
pub(super) fn workspace_remove(app: &mut App, flags: CommandFlags) -> Action {
    let names: SmallVec<WorkspaceName> = app
        .target_workspaces()
        .into_iter()
        .filter(|name| !app.is_current_workspace(name))
        .collect();
    match names.len() {
        0 => {
            app.set_error("no other workspace here");
            Action::None
        }
        1 => confirm_workspace_remove(app, names, flags),
        _ => {
            let items = names.iter().map(|w| w.to_string()).collect();
            app.mode = AppMode::select_from_list(
                "remove workspace",
                items,
                true,
                PendingSelection::WorkspaceRemove { flags },
                false,
            );
            Action::None
        }
    }
}

/// Ask before removing `names`: unlike forgetting, it deletes their
/// directories, so the prompt names what goes.
pub(super) fn confirm_workspace_remove(
    app: &mut App,
    names: SmallVec<WorkspaceName>,
    flags: CommandFlags,
) -> Action {
    let doomed: Vec<String> = names
        .iter()
        .map(|name| match app.workspace_path(name) {
            Some(path) => path.display().to_string(),
            None => format!("{name} (path unknown)"),
        })
        .collect();
    let remove = FollowUpOption {
        key: 'y',
        label: "remove and delete",
        action: FollowUpAction::Execute(JJCommand {
            kind: JJCommandKind::WorkspaceRemove { names },
            flags,
        }),
    };
    match crate::jj_command::offerable(vec![remove], app.jj) {
        Ok(options) => {
            app.mode = AppMode::FollowUp {
                prompt: crate::app::FollowUpPrompt::Text(format!(
                    "delete {} from disk?",
                    doomed.join(", ")
                )),
                options,
                origin: None,
            };
        }
        Err(reason) => app.set_error(reason),
    }
    Action::None
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
                ref remotes
            } if remotes.is_empty()
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

#[cfg(test)]
mod workspace_remove_tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{App, AppMode, FollowUpPrompt};
    use crate::dag::WorkspaceInfo;
    use crate::input::{Action, handle_key};
    use crate::jj_command::{FollowUpAction, JJCommandKind};
    use crate::jj_version::{InstalledJj, JjVersion};
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
    use crate::lua::LuaEngine;
    use crate::types::{ActiveView, WorkspaceName};

    fn workspace(name: &str, is_current: bool) -> WorkspaceInfo {
        WorkspaceInfo {
            name: WorkspaceName::new(name),
            commit_id: None,
            change_id: None,
            description: None,
            is_current,
            path: Some(format!("/work/{name}").into()),
        }
    }

    /// The workspace view on `default` (this one) and `second`, with the
    /// cursor on `on`.
    fn workspaces_view(on: usize) -> App {
        let mut app = App::for_test();
        app.jj = InstalledJj::known(JjVersion::new(0, 46, 0));
        app.views.workspace_entries = vec![workspace("default", true), workspace("second", false)];
        app.switch_view(ActiveView::Workspaces);
        for _ in 0..on {
            press(&mut app, 'j');
        }
        app
    }

    fn status(app: &App) -> &str {
        app.status_message
            .as_ref()
            .map_or("", |(msg, _)| msg.as_str())
    }

    fn press(app: &mut App, c: char) -> Action {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let lua = LuaEngine::for_test();
        handle_key(
            app,
            &keymaps,
            &lua,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        )
    }

    /// Removing deletes a directory, so it asks first and says which.
    #[test]
    fn removing_asks_before_deleting_the_directory() {
        let mut app = workspaces_view(1);
        assert!(matches!(press(&mut app, 'x'), Action::None));
        let AppMode::FollowUp {
            prompt: FollowUpPrompt::Text(prompt),
            options,
            ..
        } = &app.mode
        else {
            panic!("expected a confirmation");
        };
        assert_eq!(prompt, "delete /work/second from disk?");
        let [option] = options.as_slice() else {
            panic!("expected one option");
        };
        let FollowUpAction::Execute(cmd) = &option.action else {
            panic!("expected a command");
        };
        assert!(
            matches!(&cmd.kind, JJCommandKind::WorkspaceRemove { names } if names[0] == WorkspaceName::new("second"))
        );
    }

    /// The workspace kojutsu runs in is never offered: removing it would
    /// delete the directory out from under it.
    #[test]
    fn this_workspace_is_never_removed() {
        let mut app = workspaces_view(0);
        press(&mut app, 'x');
        assert!(matches!(app.mode, AppMode::Normal));
        assert_eq!(status(&app), "no other workspace here");
    }

    /// On a jj without `workspace remove`, say so instead of asking.
    #[test]
    fn an_older_jj_is_told_rather_than_asked() {
        let mut app = workspaces_view(1);
        app.jj = InstalledJj::known(JjVersion::new(0, 45, 1));
        press(&mut app, 'x');
        assert!(matches!(app.mode, AppMode::Normal));
        assert_eq!(
            status(&app),
            "jj workspace remove needs jj 0.46.0 or newer; the installed jj is 0.45.1"
        );
    }
}

#[cfg(test)]
mod remote_choice_tests {
    use crate::app::{App, AppMode};
    use crate::jj_version::{InstalledJj, JjVersion};
    use crate::keymap::CommandFlags;
    use crate::types::{RemoteCommand, RemoteName, SmallVec};

    use super::with_remote;

    fn chooses_several(command: RemoteCommand, minor: u32) -> bool {
        let mut app = App::for_test();
        app.jj = InstalledJj::known(JjVersion::new(0, minor, 0));
        app.views.remotes = ["origin", "upstream"]
            .map(|name| crate::dag::GitRemote {
                name: RemoteName::new(name),
                fetch_url: None,
                push_url: None,
            })
            .into();
        with_remote(&mut app, "remote", command, CommandFlags::empty());
        match &app.mode {
            AppMode::SelectFromList(state) => state.multi,
            _ => panic!("expected a remote list"),
        }
    }

    /// A push picks several remotes only where jj takes them; a fetch
    /// always has.
    #[test]
    fn several_remotes_are_offered_where_jj_takes_them() {
        assert!(!chooses_several(RemoteCommand::Push { all: false }, 45));
        assert!(chooses_several(RemoteCommand::Push { all: false }, 46));
        let bookmarks = || RemoteCommand::PushBookmark {
            bookmarks: SmallVec::new(),
        };
        assert!(!chooses_several(bookmarks(), 45));
        assert!(chooses_several(bookmarks(), 46));
        assert!(chooses_several(
            RemoteCommand::Fetch { all_remotes: false },
            36
        ));
    }

    fn remote(name: &str, url: &str) -> crate::dag::GitRemote {
        crate::dag::GitRemote {
            name: RemoteName::new(name),
            fetch_url: Some(url.into()),
            push_url: Some(url.into()),
        }
    }

    fn prompt(app: &App) -> (&str, String) {
        match &app.mode {
            AppMode::TextInput { prompt, input, .. } => (prompt.as_str(), input.value().into()),
            _ => panic!("expected a prompt"),
        }
    }

    /// With one remote there is nothing to pick: straight to what it needs,
    /// starting from what it has.
    #[test]
    fn the_only_remote_is_edited_without_a_pick() {
        let mut app = App::for_test();
        app.views.remotes = vec![remote("origin", "git@example.com:a/b")];
        super::edit_remote(
            &mut app,
            crate::types::RemoteEdit::Rename,
            CommandFlags::empty(),
        );
        assert_eq!(prompt(&app), ("rename origin to: ", "origin".into()));
        super::edit_remote(
            &mut app,
            crate::types::RemoteEdit::SetUrl,
            CommandFlags::empty(),
        );
        assert_eq!(prompt(&app), ("origin url: ", "git@example.com:a/b".into()));
    }

    /// Removing a remote forgets its bookmarks, so it asks first.
    #[test]
    fn removing_a_remote_asks_first() {
        let mut app = App::for_test();
        app.views.remotes = vec![remote("origin", "u")];
        super::edit_remote(
            &mut app,
            crate::types::RemoteEdit::Remove,
            CommandFlags::empty(),
        );
        assert!(matches!(
            &app.mode,
            AppMode::FollowUp { prompt: crate::app::FollowUpPrompt::Text(text), .. }
                if text == "remove remote origin and forget its bookmarks?"
        ));
    }

    /// A separate push URL needs jj 0.37: said before asking for it.
    #[test]
    fn a_push_url_on_an_older_jj_is_refused_before_the_prompt() {
        let mut app = App::for_test();
        app.jj = InstalledJj::known(JjVersion::new(0, 36, 0));
        app.views.remotes = vec![remote("origin", "u")];
        super::edit_remote(
            &mut app,
            crate::types::RemoteEdit::SetPushUrl,
            CommandFlags::empty(),
        );
        assert!(matches!(app.mode, AppMode::Normal));
    }

    /// Several remotes: pick which first.
    #[test]
    fn several_remotes_are_picked_from() {
        let mut app = App::for_test();
        app.views.remotes = vec![remote("origin", "a"), remote("upstream", "b")];
        super::edit_remote(
            &mut app,
            crate::types::RemoteEdit::Rename,
            CommandFlags::empty(),
        );
        assert!(matches!(app.mode, AppMode::SelectFromList(_)));
    }
}

#[cfg(test)]
mod ref_interdiff_tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{App, TagViewEntry};
    use crate::dag::{
        CommitSummary, ShortId, TagDetails, TagLocalTarget, TagPresence, TagRemoteTarget,
    };
    use crate::input::handle_key;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
    use crate::lua::LuaEngine;
    use crate::types::{ActiveView, CommitId, RemoteName, RevisionArg, TagName};

    fn summary(commit: &str) -> CommitSummary {
        CommitSummary {
            commit_id: CommitId::new(commit),
            change_id: ShortId::new("qpvuntsm"),
            short_commit_id: ShortId::new(commit),
            description: None,
            divergence: None,
        }
    }

    /// The tag view with tag `v1` here at `local`, and at `remote` on origin.
    fn tags_view(local: &str, remote: &str) -> App {
        let mut app = App::for_test();
        app.views.tag_entries.push(TagViewEntry {
            name: TagName::new("v1"),
            commit_id: Some(CommitId::new(local)),
            change_id: None,
            short_commit_id: None,
            revision: Some(RevisionArg::new("qpv")),
            description: None,
            presence: TagPresence::Local,
        });
        app.views.tag_details.insert(
            TagName::new("v1"),
            TagDetails {
                presence: TagPresence::Local,
                local_target: Some(TagLocalTarget {
                    summary: summary(local),
                }),
                remote_targets: vec![TagRemoteTarget {
                    remote: RemoteName::new("origin"),
                    summary: summary(remote),
                    is_tracked: true,
                }],
            },
        );
        app.switch_view(ActiveView::Tags);
        app
    }

    fn press_i(app: &mut App) {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        handle_key(
            app,
            &keymaps,
            &LuaEngine::for_test(),
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
        );
    }

    /// A tag that moved on the remote compares the two, labelled with the
    /// remote it came from.
    #[test]
    fn a_tag_compares_with_its_remote() {
        let mut app = tags_view("aaaa1111", "bbbb2222");
        press_i(&mut app);
        assert_eq!(app.active_view, ActiveView::Interdiff);
        let interdiff = app.interdiff.as_ref().expect("an interdiff");
        assert_eq!(
            (interdiff.from_label.as_str(), interdiff.to_label.as_str()),
            ("v1@origin", "v1")
        );
    }

    #[test]
    fn a_tag_the_same_everywhere_says_so() {
        let mut app = tags_view("aaaa1111", "aaaa1111");
        press_i(&mut app);
        assert_eq!(app.active_view, ActiveView::Tags);
        assert!(matches!(
            &app.status_message,
            Some((msg, _)) if msg == "v1 is the same on its remotes"
        ));
    }
}
