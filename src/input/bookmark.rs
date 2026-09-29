use crate::app::{App, AppMode};
use crate::dag::BookmarkRef;
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::CommandFlags;
use crate::types::{BookmarkName, FollowUpAction, FollowUpOption, SmallVec};

use super::Action;

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
        app.selected_entry_idx()
            .is_some_and(|idx| app.dag.nodes[idx].commit.is_working_copy())
    });

    if is_wc {
        // On working copy: advance immediately (jj default = advance to @).
        Action::run(JJCommand {
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
            origin: None,
        };
        Action::None
    }
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
