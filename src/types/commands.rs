use strum::IntoEnumIterator as _;

use super::id::{BookmarkName, ChangeId, SmallVec};
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
    /// Switch to a named revset preset.
    PresetSelect,
    /// Filter op log by workspace.
    OpLogWorkspaceFilter,
    /// Select a remote for git fetch.
    GitRemoteForFetch {
        all_remotes: bool,
        flags: CommandFlags,
    },
    /// Select a remote for git push.
    GitRemoteForPush { all: bool, flags: CommandFlags },
    /// Select a remote for git push bookmark (bookmarks already chosen).
    GitRemoteForPushBookmark {
        bookmarks: SmallVec<BookmarkName>,
        flags: CommandFlags,
    },
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
    /// Widen the current revset to include a specific change, then jump to it.
    WidenRevset { change_id: String },
}

/// What to do when a TextInput is submitted.
pub enum PendingCommand {
    Describe {
        change_ids: SmallVec<ChangeId>,
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
    /// Set bookmark to a change ID (name already known, text is change ID).
    BookmarkSetByName {
        name: BookmarkName,
        flags: CommandFlags,
    },
    /// Rename a bookmark (old name already selected, text is new name).
    BookmarkRename {
        old_name: BookmarkName,
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
    /// Set tag to a change ID (name already known, text is change ID).
    TagSetByName { name: String, flags: CommandFlags },
    /// Workspace add step 1: collecting path. Text = path.
    WorkspaceAddPath { flags: CommandFlags },
    /// Workspace add step 2: path collected, collecting name. Text = name.
    WorkspaceAddName { path: String, flags: CommandFlags },
    /// Rename current workspace. Text = new name.
    WorkspaceRename { flags: CommandFlags },
}

impl PendingCommand {
    /// Convert to a `JJCommand` given the user's input text.
    ///
    /// Panics if called on `Revset` -- that variant is handled separately.
    pub fn into_jj_command(self, text: String) -> JJCommand {
        match self {
            PendingCommand::Describe { change_ids, flags } => JJCommand::Describe {
                change_ids,
                message: text,
                flags,
            },
            PendingCommand::SquashWithMessage { builder, flags } => builder.build(text, flags),
            PendingCommand::Revset => panic!("Revset pending command handled separately"),
            PendingCommand::BookmarkCreate { change_id, flags } => JJCommand::BookmarkCreate {
                name: BookmarkName::new(text),
                change_id,
                flags,
            },
            PendingCommand::BookmarkSet { change_id, flags } => JJCommand::BookmarkSet {
                name: BookmarkName::new(text),
                change_id,
                flags,
            },
            PendingCommand::BookmarkSetByName { name, flags } => JJCommand::BookmarkSet {
                name,
                change_id: ChangeId::new(text),
                flags,
            },
            PendingCommand::BookmarkRename { old_name, flags } => JJCommand::BookmarkRename {
                old_name,
                new_name: BookmarkName::new(text),
                flags,
            },
            PendingCommand::TagSet { change_id, flags } => JJCommand::TagSet {
                name: text,
                change_id,
                flags,
            },
            PendingCommand::TagSetByName { name, flags } => JJCommand::TagSet {
                name,
                change_id: ChangeId::new(text),
                flags,
            },
            PendingCommand::Commit { flags, selection } => JJCommand::Commit {
                message: Some(text),
                selection,
                flags,
            },
            PendingCommand::WorkspaceRename { flags } => JJCommand::WorkspaceRename {
                new_name: text,
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
    Rebase {
        source_mode: RebaseSource,
        /// All source commit IDs (supports multi-commit rebase).
        sources: SmallVec<ChangeId>,
    },
    RestoreFrom,
    RestoreInto,
    BookmarkMove {
        bookmark_name: BookmarkName,
    },
    DuplicateOnto,
    Revert {
        sources: SmallVec<ChangeId>,
    },
}

impl TargetOperation {
    /// Whether this operation supports selecting multiple targets.
    pub fn multi_target(&self) -> bool {
        matches!(self, TargetOperation::Rebase { .. })
    }

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
            TargetOperation::Rebase { source_mode, .. } => match source_mode {
                RebaseSource::Revision => "rebase revision",
                RebaseSource::Source => "rebase source",
                RebaseSource::Branch => "rebase branch",
            },
            TargetOperation::RestoreFrom => "restore from",
            TargetOperation::RestoreInto => "restore into",
            TargetOperation::BookmarkMove { .. } => "move bookmark",
            TargetOperation::DuplicateOnto => "duplicate onto",
            TargetOperation::Revert { .. } => "revert",
        }
    }

    pub fn follow_up(
        self,
        source: ChangeId,
        targets: SmallVec<ChangeId>,
        flags: CommandFlags,
        selection: ChangeSelection,
    ) -> Vec<FollowUpOption> {
        match self {
            TargetOperation::Squash(kind) => squash_follow_up(
                source,
                Some(SquashTarget {
                    target: targets.into_iter().next().expect("target required"),
                    kind,
                }),
                selection,
                flags,
            ),
            TargetOperation::Split(kind) => auto_follow_up(
                "split",
                JJCommand::Split {
                    change_id: source,
                    target: Some(SplitTarget {
                        target: targets.into_iter().next().expect("target required"),
                        kind,
                    }),
                    selection,
                    flags,
                },
            ),
            TargetOperation::Rebase {
                source_mode,
                sources,
            } => rebase_follow_up(sources, targets, source_mode, flags),
            TargetOperation::RestoreFrom => auto_follow_up(
                "restore",
                JJCommand::Restore {
                    from: targets.into_iter().next(),
                    into: None,
                    changes_in: None,
                    selection,
                    flags,
                },
            ),
            TargetOperation::RestoreInto => auto_follow_up(
                "restore",
                JJCommand::Restore {
                    from: None,
                    into: targets.into_iter().next(),
                    changes_in: None,
                    selection,
                    flags,
                },
            ),
            TargetOperation::BookmarkMove { bookmark_name } => auto_follow_up(
                "move",
                JJCommand::BookmarkMove {
                    name: bookmark_name.clone(),
                    target: targets.into_iter().next().expect("target required"),
                    flags,
                },
            ),
            TargetOperation::DuplicateOnto => auto_follow_up(
                "duplicate",
                JJCommand::Duplicate {
                    change_ids: smallvec::smallvec![source],
                    onto: targets.into_iter().next(),
                    flags,
                },
            ),
            TargetOperation::Revert { sources } => RebaseKind::iter()
                .map(|kind| FollowUpOption {
                    key: kind.key(),
                    label: kind.label(),
                    action: FollowUpAction::Execute(JJCommand::Revert {
                        change_ids: sources.clone(),
                        dest: RebaseTarget {
                            targets: targets.clone(),
                            kind,
                        },
                        flags,
                    }),
                })
                .collect(),
        }
    }
}

/// Single auto-executing follow-up (used when no user choice is needed).
fn auto_follow_up(label: &'static str, cmd: JJCommand) -> Vec<FollowUpOption> {
    vec![FollowUpOption {
        key: ' ',
        label,
        action: FollowUpAction::Execute(cmd),
    }]
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
    sources: SmallVec<ChangeId>,
    targets: SmallVec<ChangeId>,
    source_mode: RebaseSource,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    RebaseKind::iter()
        .map(|kind| FollowUpOption {
            key: kind.key(),
            label: kind.label(),
            action: FollowUpAction::Execute(JJCommand::Rebase {
                change_ids: sources.clone(),
                source_mode: source_mode.clone(),
                dest: RebaseTarget {
                    targets: targets.clone(),
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
    /// Build a command with an inline message.
    pub fn build(self, message: String, flags: CommandFlags) -> JJCommand {
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
}
