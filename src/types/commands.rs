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

/// What to do when a TextInput is submitted.
pub enum PendingCommand {
    Describe {
        change_ids: SmallVec<RevisionArg>,
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
        change_id: RevisionArg,
        flags: CommandFlags,
    },
    /// Set (create or update) a bookmark.
    BookmarkSet {
        change_id: RevisionArg,
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
        change_id: RevisionArg,
        flags: CommandFlags,
    },
    /// Set tag to a change ID (name already known, text is change ID).
    TagSetByName {
        name: TagName,
        flags: CommandFlags,
    },
    /// Workspace add step 1: collecting path. Text = path.
    WorkspaceAddPath {
        flags: CommandFlags,
    },
    /// Workspace add step 2: path collected, collecting name. Text = name.
    WorkspaceAddName {
        path: String,
        flags: CommandFlags,
    },
    /// Rename current workspace. Text = new name.
    WorkspaceRename {
        flags: CommandFlags,
    },
    /// Run step 1: collecting the command to run over revisions. Text = command line.
    RunCommand {
        change_ids: SmallVec<RevisionArg>,
        flags: CommandFlags,
    },
    /// Run step 2: command collected, collecting `--jobs`. Text = job count (empty = jj default).
    RunJobs {
        change_ids: SmallVec<RevisionArg>,
        argv: Vec<Str>,
        flags: CommandFlags,
    },
    RawCommand,
    LuaResume,
}

/// What kind of two-commit target selection we're doing.
#[derive(Debug, Clone)]
pub enum TargetOperation {
    Squash(SquashKind),
    Split(SplitKind),
    Rebase {
        source_mode: RebaseSource,
        /// All source commit IDs (supports multi-commit rebase).
        sources: SmallVec<RevisionArg>,
    },
    RestoreFrom,
    RestoreInto,
    BookmarkMove {
        bookmark_name: BookmarkName,
    },
    DuplicateOnto {
        sources: SmallVec<RevisionArg>,
    },
    Revert {
        sources: SmallVec<RevisionArg>,
    },
    Interdiff,
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
            TargetOperation::DuplicateOnto { .. } => "duplicate onto",
            TargetOperation::Revert { .. } => "revert",
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
