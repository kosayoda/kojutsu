use strum::IntoEnumIterator as _;

use super::id::ChangeId;
use super::operations::{
    MessageMode, RebaseKind, RebaseSource, RebaseTarget, SplitKind, SplitTarget, SquashKind,
    SquashTarget,
};
use crate::jj_command::{ChangeSelection, JJCommand};
use crate::keymap::CommandFlags;

/// What to do after selecting an item from a list.
pub enum PendingSelection {
    /// Delete a bookmark on the selected commit.
    BookmarkDelete {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Forget a bookmark on the selected commit.
    BookmarkForget {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Move a bookmark to a target commit (enters TargetSelect after selection).
    BookmarkMove {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Rename a bookmark (enters TextInput after selection).
    BookmarkRename {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Forget a workspace (text is the workspace name from the list).
    WorkspaceForget { flags: CommandFlags },
    /// Track remote bookmarks.
    BookmarkTrack { flags: CommandFlags },
    /// Untrack remote bookmarks.
    BookmarkUntrack { flags: CommandFlags },
    /// Push bookmarks to remote.
    GitPushBookmark { flags: CommandFlags },
    /// Delete a tag.
    TagDelete { flags: CommandFlags },
}

/// What to do after selecting a single commit in CommitSelect mode.
pub enum PendingCommitSelect {
    WorkspaceAdd { path: String, name: Option<String> },
}

impl PendingCommitSelect {
    pub fn prompt(&self) -> &'static str {
        match self {
            PendingCommitSelect::WorkspaceAdd { .. } => "workspace revision",
        }
    }

    pub fn into_jj_command(self, target: ChangeId, flags: CommandFlags) -> JJCommand {
        match self {
            PendingCommitSelect::WorkspaceAdd { path, name } => JJCommand::WorkspaceAdd {
                path,
                name,
                revision: target,
                flags,
            },
        }
    }
}

/// An option in a follow-up prompt (shown after target selection).
pub struct FollowUpOption {
    pub key: char,
    pub label: &'static str,
    pub action: FollowUpAction,
}

/// What happens when a follow-up option is selected.
pub enum FollowUpAction {
    /// Execute a command immediately.
    Execute(JJCommand),
    /// Enter a text input, then execute.
    TextInput {
        prompt: String,
        pending: PendingCommand,
    },
}

/// What to do when a TextInput is submitted.
pub enum PendingCommand {
    Describe {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    SquashWithMessage {
        builder: ReadyCommand,
        flags: CommandFlags,
    },
    /// The text is a revset expression to evaluate.
    Revset,
    /// Create a bookmark with the given name.
    BookmarkCreate {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Set (create or update) a bookmark.
    BookmarkSet {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Rename a bookmark (old name already selected, text is new name).
    BookmarkRename {
        old_name: String,
        flags: CommandFlags,
    },
    /// Commit with inline message (text is the message).
    Commit {
        flags: CommandFlags,
        selection: ChangeSelection,
    },
    /// Set (create or update) a tag.
    TagSet {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Workspace add step 1: collecting path. Text = path.
    WorkspaceAddPath { flags: CommandFlags },
    /// Workspace add step 2: path collected, collecting name. Text = name.
    WorkspaceAddName { path: String, flags: CommandFlags },
}

impl PendingCommand {
    /// Convert to a `JJCommand` given the user's input text.
    ///
    /// Panics if called on `Revset` -- that variant is handled separately.
    pub fn into_jj_command(self, text: String) -> JJCommand {
        match self {
            PendingCommand::Describe { change_id, flags } => JJCommand::Describe {
                change_id,
                message: text,
                flags,
            },
            PendingCommand::SquashWithMessage { builder, flags } => {
                builder.build_with_message(text, flags)
            }
            PendingCommand::Revset => panic!("Revset pending command handled separately"),
            PendingCommand::BookmarkCreate { change_id, flags } => JJCommand::BookmarkCreate {
                name: text,
                change_id,
                flags,
            },
            PendingCommand::BookmarkSet { change_id, flags } => JJCommand::BookmarkSet {
                name: text,
                change_id,
                flags,
            },
            PendingCommand::BookmarkRename { old_name, flags } => JJCommand::BookmarkRename {
                old_name,
                new_name: text,
                flags,
            },
            PendingCommand::TagSet { change_id, flags } => JJCommand::TagSet {
                name: text,
                change_id,
                flags,
            },
            PendingCommand::Commit { flags, selection } => JJCommand::Commit {
                message: Some(text),
                selection,
                flags,
            },
            PendingCommand::WorkspaceAddPath { .. } | PendingCommand::WorkspaceAddName { .. } => {
                panic!("Workspace add steps handled separately in handle_text_input")
            }
        }
    }
}

/// What kind of two-commit target selection we're doing.
#[derive(Debug, Clone)]
pub enum TargetOperation {
    Squash(SquashKind),
    Split(SplitKind),
    Rebase(RebaseSource),
    RestoreFrom,
    RestoreInto,
    BookmarkMove { bookmark_name: String },
    DuplicateOnto,
}

impl TargetOperation {
    pub fn label(&self) -> &'static str {
        match self {
            TargetOperation::Squash(kind) => match kind {
                SquashKind::Into => "squash into",
                SquashKind::Onto => "squash onto",
                SquashKind::After => "squash after",
                SquashKind::Before => "squash before",
            },
            TargetOperation::Split(kind) => match kind {
                SplitKind::Onto => "split onto",
                SplitKind::After => "split after",
                SplitKind::Before => "split before",
            },
            TargetOperation::Rebase(source) => match source {
                RebaseSource::Revision => "rebase revision",
                RebaseSource::Source => "rebase source",
                RebaseSource::Branch => "rebase branch",
            },
            TargetOperation::RestoreFrom => "restore from",
            TargetOperation::RestoreInto => "restore into",
            TargetOperation::BookmarkMove { .. } => "move bookmark",
            TargetOperation::DuplicateOnto => "duplicate onto",
        }
    }

    pub fn follow_up(
        self,
        source: ChangeId,
        target: ChangeId,
        flags: CommandFlags,
        selection: ChangeSelection,
    ) -> Vec<FollowUpOption> {
        match self {
            TargetOperation::Squash(kind) => squash_follow_up(
                source,
                Some(SquashTarget { target, kind }),
                selection,
                flags,
            ),
            TargetOperation::Split(kind) => vec![FollowUpOption {
                key: ' ',
                label: "split",
                action: FollowUpAction::Execute(JJCommand::Split {
                    change_id: source,
                    target: Some(SplitTarget { target, kind }),
                    selection,
                    flags,
                }),
            }],
            TargetOperation::Rebase(source_mode) => {
                rebase_follow_up(source, target, source_mode, flags)
            }
            TargetOperation::RestoreFrom => vec![FollowUpOption {
                key: ' ',
                label: "restore",
                action: FollowUpAction::Execute(JJCommand::Restore {
                    from: Some(target),
                    into: None,
                    changes_in: None,
                    selection,
                    flags,
                }),
            }],
            TargetOperation::RestoreInto => vec![FollowUpOption {
                key: ' ',
                label: "restore",
                action: FollowUpAction::Execute(JJCommand::Restore {
                    from: None,
                    into: Some(target),
                    changes_in: None,
                    selection,
                    flags,
                }),
            }],
            TargetOperation::BookmarkMove { bookmark_name } => {
                // Bookmark move executes immediately -- no follow-up choice.
                vec![FollowUpOption {
                    key: ' ', // won't be shown; auto-executed below
                    label: "move",
                    action: FollowUpAction::Execute(JJCommand::BookmarkMove {
                        name: bookmark_name.clone(),
                        target,
                        flags,
                    }),
                }]
            }
            TargetOperation::DuplicateOnto => {
                // Duplicate onto executes immediately -- single option, auto-executed.
                vec![FollowUpOption {
                    key: ' ',
                    label: "duplicate",
                    action: FollowUpAction::Execute(JJCommand::Duplicate {
                        change_id: source,
                        onto: Some(target),
                        flags,
                    }),
                }]
            }
        }
    }
}

/// Build follow-up options for a squash command.
fn squash_follow_up(
    source: ChangeId,
    target: Option<SquashTarget>,
    selection: ChangeSelection,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    let default_cmd = JJCommand::Squash {
        change_id: source.clone(),
        target: target.clone(),
        message: MessageMode::Default,
        selection: selection.clone(),
        flags,
    };
    let use_dest_cmd = JJCommand::Squash {
        change_id: source.clone(),
        target: target.clone(),
        message: MessageMode::UseDestination,
        selection: selection.clone(),
        flags,
    };
    let builder = ReadyCommand::Squash {
        source,
        target,
        selection,
    };

    vec![
        FollowUpOption {
            key: 's',
            label: "squash",
            action: FollowUpAction::Execute(default_cmd),
        },
        FollowUpOption {
            key: 'm',
            label: "with message",
            action: FollowUpAction::TextInput {
                prompt: "squash message: ".to_string(),
                pending: PendingCommand::SquashWithMessage { builder, flags },
            },
        },
        FollowUpOption {
            key: 'd',
            label: "use dest message",
            action: FollowUpAction::Execute(use_dest_cmd),
        },
    ]
}

/// Build follow-up options for a rebase command (dest mode selection).
fn rebase_follow_up(
    source: ChangeId,
    target: ChangeId,
    source_mode: RebaseSource,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    RebaseKind::iter()
        .map(|kind| FollowUpOption {
            key: kind.key(),
            label: kind.label(),
            action: FollowUpAction::Execute(JJCommand::Rebase {
                change_id: source.clone(),
                source_mode: source_mode.clone(),
                dest: RebaseTarget {
                    target: target.clone(),
                    kind,
                },
                flags,
            }),
        })
        .collect()
}

/// A partially-constructed command that needs a message from the user.
pub enum ReadyCommand {
    Squash {
        source: ChangeId,
        target: Option<SquashTarget>,
        selection: ChangeSelection,
    },
}

impl ReadyCommand {
    /// Build with default message behavior (jj handles it).
    pub fn build_default(self, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash {
                source,
                target,
                selection,
            } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::Default,
                selection,
                flags,
            },
        }
    }

    /// Build with an inline message.
    pub fn build_with_message(self, message: String, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash {
                source,
                target,
                selection,
            } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::Inline(message),
                selection,
                flags,
            },
        }
    }

    /// Build with --use-destination-message.
    pub fn build_use_dest_message(self, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash {
                source,
                target,
                selection,
            } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::UseDestination,
                selection,
                flags,
            },
        }
    }
}
