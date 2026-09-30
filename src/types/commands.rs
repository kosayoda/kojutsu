use super::id::{
    BookmarkName, CommitId, RemoteName, RevisionArg, SmallVec, Str, TagName, WorkspaceName,
};
use super::operations::{ChangeSelection, RebaseSource, SplitKind, SquashKind, SquashTarget};
use crate::keymap::CommandFlags;

/// What to do after selecting an item from a list.
pub enum PendingSelection {
    /// Delete the chosen bookmarks.
    BookmarkDelete {
        flags: CommandFlags,
    },
    /// Forget the chosen bookmarks.
    BookmarkForget {
        flags: CommandFlags,
    },
    /// Move the chosen bookmark from `source` to a target commit (enters
    /// TargetSelect after selection).
    BookmarkMove {
        source: RevisionArg,
        flags: CommandFlags,
    },
    /// Rename the chosen bookmark (enters TextInput after selection).
    BookmarkRename {
        flags: CommandFlags,
    },
    /// Forget a workspace (text is the workspace name from the list).
    WorkspaceForget {
        flags: CommandFlags,
    },
    /// Track remote bookmarks.
    BookmarkTrack {
        flags: CommandFlags,
    },
    /// Untrack remote bookmarks.
    BookmarkUntrack {
        flags: CommandFlags,
    },
    /// Push bookmarks to remote.
    GitPushBookmark {
        flags: CommandFlags,
    },
    /// Delete a tag.
    TagDelete {
        flags: CommandFlags,
    },
    /// Switch to a named revset preset.
    PresetSelect,
    /// Filter op log by workspace.
    OpLogWorkspaceFilter,
    CommandCompletion {
        input: String,
    },
    FileListAnnotate {
        commit_id: CommitId,
    },
    /// Select the remote a git command talks to.
    GitRemote {
        command: RemoteCommand,
        flags: CommandFlags,
    },
    /// Pick a `jj run` command from presets and history; the list's custom
    /// entry opens a free-text input instead.
    RunCommand {
        change_ids: SmallVec<RevisionArg>,
        flags: CommandFlags,
    },
    /// Resume a Lua thread suspended on `kojutsu.ui.choose`. `multi` mirrors
    /// the list's own multi-select flag: it decides whether the thread is
    /// resumed with a single string or an array of them.
    LuaResume {
        multi: bool,
    },
}

/// A git command that talks to a remote, waiting to learn which one.
pub enum RemoteCommand {
    Fetch { all_remotes: bool },
    Push { all: bool },
    PushBookmark { bookmarks: SmallVec<BookmarkName> },
}

impl RemoteCommand {
    /// The command, against `remote` (`None` for jj's default).
    pub fn to_kind(self, remote: Option<RemoteName>) -> crate::jj_command::JJCommandKind {
        use crate::jj_command::JJCommandKind;
        match self {
            Self::Fetch { all_remotes } => JJCommandKind::GitFetch {
                all_remotes,
                remote,
            },
            Self::Push { all } => JJCommandKind::GitPush { all, remote },
            Self::PushBookmark { bookmarks } => {
                JJCommandKind::GitPushBookmark { bookmarks, remote }
            }
        }
    }
}

/// What to do after selecting a single commit in CommitSelect mode.
pub enum PendingCommitSelect {
    WorkspaceAdd {
        path: String,
        name: Option<WorkspaceName>,
    },
}

impl PendingCommitSelect {
    pub fn prompt(&self) -> &'static str {
        match self {
            PendingCommitSelect::WorkspaceAdd { .. } => "workspace revision",
        }
    }
}

/// What a text prompt does with the text once it is submitted.
pub enum TextPrompt {
    /// The text completes a jj command.
    Command(CommandPrompt),
    /// The text feeds a step that isn't a jj command yet.
    Step(PromptStep),
}

impl From<CommandPrompt> for TextPrompt {
    fn from(prompt: CommandPrompt) -> Self {
        Self::Command(prompt)
    }
}

impl From<PromptStep> for TextPrompt {
    fn from(step: PromptStep) -> Self {
        Self::Step(step)
    }
}

/// A jj command waiting on one piece of text.
pub enum CommandPrompt {
    /// Text is the description.
    Describe {
        change_ids: SmallVec<RevisionArg>,
        flags: CommandFlags,
    },
    /// Text is the squashed commit's message.
    SquashWithMessage {
        builder: ReadyCommand,
        flags: CommandFlags,
    },
    /// Text is the new bookmark's name.
    BookmarkCreate {
        change_id: RevisionArg,
        flags: CommandFlags,
    },
    /// Text is the name of the bookmark to create or move there.
    BookmarkSet {
        change_id: RevisionArg,
        flags: CommandFlags,
    },
    /// Text is the change to point the named bookmark at.
    BookmarkSetByName {
        name: BookmarkName,
        flags: CommandFlags,
    },
    /// Text is the bookmark's new name.
    BookmarkRename {
        old_name: BookmarkName,
        flags: CommandFlags,
    },
    /// Text is the commit message.
    Commit {
        flags: CommandFlags,
        selection: ChangeSelection,
    },
    /// Text is the name of the tag to create or move there.
    TagSet {
        change_id: RevisionArg,
        flags: CommandFlags,
    },
    /// Text is the change to point the named tag at.
    TagSetByName { name: TagName, flags: CommandFlags },
    /// Text is the current workspace's new name.
    WorkspaceRename { flags: CommandFlags },
}

impl CommandPrompt {
    /// The command, completed with the submitted text.
    pub fn into_command(self, text: String) -> crate::jj_command::JJCommand {
        use crate::jj_command::{JJCommand, JJCommandKind};
        let (kind, flags) = match self {
            Self::Describe { change_ids, flags } => (
                JJCommandKind::Describe {
                    change_ids,
                    message: text,
                },
                flags,
            ),
            Self::SquashWithMessage { builder, flags } => return builder.build(text, flags),
            Self::BookmarkCreate { change_id, flags } => (
                JJCommandKind::BookmarkCreate {
                    name: BookmarkName::new(text),
                    change_id,
                },
                flags,
            ),
            Self::BookmarkSet { change_id, flags } => (
                JJCommandKind::BookmarkSet {
                    name: BookmarkName::new(text),
                    change_id,
                },
                flags,
            ),
            Self::BookmarkSetByName { name, flags } => (
                JJCommandKind::BookmarkSet {
                    name,
                    change_id: RevisionArg::new(text),
                },
                flags,
            ),
            Self::BookmarkRename { old_name, flags } => (
                JJCommandKind::BookmarkRename {
                    old_name,
                    new_name: BookmarkName::new(text),
                },
                flags,
            ),
            Self::Commit { flags, selection } => (
                JJCommandKind::Commit {
                    message: Some(text),
                    selection,
                },
                flags,
            ),
            Self::TagSet { change_id, flags } => (
                JJCommandKind::TagSet {
                    name: TagName::new(text),
                    change_id,
                },
                flags,
            ),
            Self::TagSetByName { name, flags } => (
                JJCommandKind::TagSet {
                    name,
                    change_id: RevisionArg::new(text),
                },
                flags,
            ),
            Self::WorkspaceRename { flags } => (
                JJCommandKind::WorkspaceRename {
                    new_name: WorkspaceName::new(text),
                },
                flags,
            ),
        };
        JJCommand { kind, flags }
    }
}

/// A prompt whose text isn't a jj command's last piece: it sets the revset,
/// resumes a plugin, is parsed as a command line, or leads to the next
/// prompt in a sequence.
pub enum PromptStep {
    /// Text is a revset expression to show.
    Revset,
    /// Workspace add, step 1: text is the path.
    WorkspaceAddPath { flags: CommandFlags },
    /// Workspace add, step 2: text is the name (empty for jj's default).
    WorkspaceAddName { path: String, flags: CommandFlags },
    /// Run, step 1: text is the command line to run over the revisions.
    RunCommand {
        change_ids: SmallVec<RevisionArg>,
        flags: CommandFlags,
    },
    /// Run, step 2: text is the job count (empty for jj's default).
    RunJobs {
        change_ids: SmallVec<RevisionArg>,
        argv: Vec<Str>,
        flags: CommandFlags,
    },
    /// Text is a jj command line typed in full.
    RawCommand,
    /// Text resumes the plugin thread waiting on it.
    LuaResume,
}

/// What kind of two-commit target selection we're doing.
#[derive(Debug, Clone)]
pub enum TargetOperation {
    Squash(SquashKind),
    Split(SplitKind),
    Rebase { source_mode: RebaseSource },
    RestoreFrom,
    RestoreInto,
    BookmarkMove { bookmark_name: BookmarkName },
    DuplicateOnto,
    Revert,
    Interdiff,
}

impl TargetOperation {
    /// Whether the command it builds takes the file or line selection.
    pub fn takes_selection(&self) -> bool {
        matches!(
            self,
            TargetOperation::Squash(_)
                | TargetOperation::Split(_)
                | TargetOperation::RestoreFrom
                | TargetOperation::RestoreInto
        )
    }

    /// Whether it acts on every selected commit, rather than on the one
    /// under the cursor (or the one a file selection is in).
    pub fn takes_many_sources(&self) -> bool {
        matches!(
            self,
            TargetOperation::Rebase { .. }
                | TargetOperation::DuplicateOnto
                | TargetOperation::Revert
        )
    }

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
            TargetOperation::Revert => "revert",
            TargetOperation::Interdiff => "interdiff with",
        }
    }
}

/// A partially-constructed command that needs a message from the user.
pub enum ReadyCommand {
    Squash {
        source: RevisionArg,
        target: Option<SquashTarget>,
        selection: ChangeSelection,
    },
}
