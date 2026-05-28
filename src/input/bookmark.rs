use crate::app::{App, AppMode};
use crate::dag::BookmarkRef;
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::CommandFlags;
use crate::types::{
    BookmarkName, ChangeId, DisplayRow, FollowUpAction, FollowUpOption, PendingCommand,
    PendingSelection, SmallVec, TagName,
};

use super::Action;

/// Show a select-from-list for remote bookmarks, or a status message if empty.
pub(super) fn enter_remote_bookmark_select(
    app: &mut App,
    bookmarks: Vec<String>,
    empty_msg: &str,
    title: &str,
    on_select: PendingSelection,
) -> Action {
    if bookmarks.is_empty() {
        app.set_status(empty_msg);
        return Action::None;
    }
    app.mode = AppMode::select_from_list(title, bookmarks, true, on_select, true);
    Action::None
}

pub(super) enum BookmarkTextAction {
    Create,
    Set,
}

#[derive(Clone, Copy)]
pub(super) enum PendingSelectionKind {
    Delete,
    Forget,
    Move,
    Rename,
}

impl PendingSelectionKind {
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Delete => "delete bookmark",
            Self::Forget => "forget bookmark",
            Self::Move => "move bookmark",
            Self::Rename => "rename bookmark",
        }
    }

    pub(super) fn to_pending(self, change_id: ChangeId, flags: CommandFlags) -> PendingSelection {
        match self {
            Self::Delete => PendingSelection::BookmarkDelete { change_id, flags },
            Self::Forget => PendingSelection::BookmarkForget { change_id, flags },
            Self::Move => PendingSelection::BookmarkMove { change_id, flags },
            Self::Rename => PendingSelection::BookmarkRename { change_id, flags },
        }
    }

    pub(super) fn is_multi(self) -> bool {
        matches!(self, Self::Delete | Self::Forget)
    }
}

pub(super) fn enter_bookmark_advance(app: &mut App, flags: CommandFlags) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };

    // Check if the selected commit is the working copy.
    let is_wc = app.selected_bookmarks().is_some_and(|_| {
        // Check via the entries
        let entry_idx = match app.rows.get(app.cursor.raw()) {
            Some(DisplayRow::CommitNode { entry_idx })
            | Some(DisplayRow::DescriptionLine { entry_idx, .. })
            | Some(DisplayRow::GraphLink { entry_idx, .. })
            | Some(DisplayRow::FileChange { entry_idx, .. })
            | Some(DisplayRow::DiffLine { entry_idx, .. }) => Some(*entry_idx),
            Some(DisplayRow::BookmarkItem { .. })
            | Some(DisplayRow::BookmarkConflictTarget { .. })
            | Some(DisplayRow::BookmarkRemoteTarget { .. })
            | Some(DisplayRow::TagItem { .. })
            | Some(DisplayRow::TagRemoteTarget { .. })
            | Some(DisplayRow::OpLogItem { .. })
            | Some(DisplayRow::OpLogDetailLine { .. })
            | Some(DisplayRow::OpLogGraphLink { .. })
            | Some(DisplayRow::OpLogLoadMore)
            | Some(DisplayRow::EvoLogItem { .. })
            | Some(DisplayRow::EvoLogFileChange { .. })
            | Some(DisplayRow::EvoLogFileDiffLine { .. })
            | Some(DisplayRow::EvoLogGraphLink { .. })
            | Some(DisplayRow::WorkspaceItem { .. })
            | Some(DisplayRow::CommandLogItem { .. })
            | Some(DisplayRow::CommandLogDetail { .. })
            | Some(DisplayRow::ConflictHeader { .. })
            | Some(DisplayRow::ConflictSide { .. })
            | Some(DisplayRow::ConflictContext { .. })
            | Some(DisplayRow::InterdiffHeader)
            | Some(DisplayRow::InterdiffFileChange { .. })
            | Some(DisplayRow::InterdiffDiffLine { .. })
            | Some(DisplayRow::AnnotateLine { .. })
            | Some(DisplayRow::AnnotateDetail { .. })
            | None => None,
        };
        entry_idx.is_some_and(|idx| app.nodes[idx].commit.is_working_copy())
    });

    if is_wc {
        // On working copy: advance immediately (jj default = advance to @).
        Action::RunJj(JJCommand {
            kind: JJCommandKind::BookmarkAdvance { change_id: None },
            flags,
        })
    } else {
        // Not on working copy: show follow-up to choose between selected and @.
        app.mode = AppMode::FollowUp {
            prompt: "advance bookmarks to:".to_string(),
            options: vec![
                FollowUpOption {
                    key: 's',
                    label: "selected",
                    action: FollowUpAction::Execute(JJCommand {
                        kind: JJCommandKind::BookmarkAdvance {
                            change_id: Some(change_id),
                        },
                        flags,
                    }),
                },
                FollowUpOption {
                    key: '@',
                    label: "working copy",
                    action: FollowUpAction::Execute(JJCommand {
                        kind: JJCommandKind::BookmarkAdvance { change_id: None },
                        flags,
                    }),
                },
            ],
        };
        Action::None
    }
}

pub(super) fn enter_bookmark_text_input(
    app: &mut App,
    flags: CommandFlags,
    prompt: &str,
    action: BookmarkTextAction,
) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };

    let on_submit = match action {
        BookmarkTextAction::Create => PendingCommand::BookmarkCreate { change_id, flags },
        BookmarkTextAction::Set => PendingCommand::BookmarkSet { change_id, flags },
    };

    app.mode = AppMode::text_input(prompt, "", on_submit);
    Action::None
}

pub(super) fn enter_bookmark_select(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    flags: CommandFlags,
    kind: PendingSelectionKind,
) -> Action {
    let Some(change_id) = app.selected_change_id() else {
        return Action::None;
    };
    let bookmarks = app.selected_bookmarks().unwrap_or(&[]);
    if bookmarks.is_empty() {
        app.set_status("no bookmarks on this commit");
        return Action::None;
    }

    let on_select = kind.to_pending(change_id, flags);
    let items: Vec<String> = bookmarks.iter().map(|b| b.name.to_string()).collect();

    if items.len() == 1 {
        return super::list::resolve_selection(app, lua, on_select, items.into());
    }

    app.mode = AppMode::select_from_list(kind.title(), items, kind.is_multi(), on_select, false);
    Action::None
}

pub(super) fn enter_tag_delete(app: &mut App, flags: CommandFlags) -> Action {
    let tags = app.selected_tags().unwrap_or(&[]);
    if tags.is_empty() {
        app.set_status("no tags on this commit");
        return Action::None;
    }
    let items: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
    if items.len() == 1 {
        return Action::RunJj(JJCommand {
            kind: JJCommandKind::TagDelete {
                names: items.into_iter().map(TagName::new).collect(),
            },
            flags,
        });
    }
    let on_select = PendingSelection::TagDelete { flags };
    app.mode = AppMode::select_from_list("delete tag", items, true, on_select, false);
    Action::None
}

/// Parse `"name@remote"` display strings into `BookmarkRef` values.
pub(super) fn parse_remote_bookmarks(names: SmallVec<String>) -> SmallVec<BookmarkRef> {
    names
        .into_iter()
        .filter_map(|s| {
            let (name, remote) = s.rsplit_once('@')?;
            Some(BookmarkRef {
                name: BookmarkName::new(name),
                remote: crate::types::RemoteName::new(remote),
            })
        })
        .collect()
}
