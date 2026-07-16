mod args;
pub mod completion;
mod follow_up;
mod run;

pub use completion::Completion;
pub use completion::{common_prefix, complete, replace_current_token, split_for_completion};
pub use follow_up::{FollowUpAction, FollowUpOption};

use std::sync::{Arc, atomic::AtomicI32, atomic::Ordering};

use crate::dag::BookmarkRef;
use crate::keymap::CommandFlags;
use crate::types::{
    BookmarkName, ChangeId, ChangeSelection, JumpTarget, MessageMode, OperationId, RebaseSource,
    RebaseTarget, RemoteName, SmallVec, SplitTarget, SquashTarget, Str, TagName, WorkspaceName,
};

#[derive(Clone)]
pub struct KillHandle(Arc<AtomicI32>);

impl KillHandle {
    pub fn pair() -> (Self, Self) {
        let arc = Arc::new(AtomicI32::new(0));
        (Self(Arc::clone(&arc)), Self(arc))
    }

    pub(crate) fn set_pgid(&self, pgid: i32) {
        self.0.store(pgid, Ordering::Release);
    }

    pub fn kill(&self) {
        let pgid = self.0.load(Ordering::Acquire);
        if pgid > 0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pgid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum InsertPosition {
    After,
    Before,
}

impl InsertPosition {
    fn flag(self) -> &'static str {
        match self {
            InsertPosition::After => "--insert-after",
            InsertPosition::Before => "--insert-before",
        }
    }
}

#[derive(Debug, Clone)]
pub struct JJCommand {
    pub kind: JJCommandKind,
    pub flags: CommandFlags,
}

#[derive(Debug, Clone)]
pub enum JJCommandKind {
    Abandon {
        change_ids: SmallVec<ChangeId>,
    },
    Describe {
        change_ids: SmallVec<ChangeId>,
        message: String,
    },
    DescribeInEditor {
        change_id: ChangeId,
    },
    Diffedit {
        change_id: ChangeId,
        selection: ChangeSelection,
    },
    Edit {
        change_id: ChangeId,
    },
    New {
        change_ids: SmallVec<ChangeId>,
        insert: Option<InsertPosition>,
    },
    Squash {
        change_id: ChangeId,
        target: Option<SquashTarget>,
        message: MessageMode,
        selection: ChangeSelection,
    },
    Rebase {
        change_ids: SmallVec<ChangeId>,
        source_mode: RebaseSource,
        dest: RebaseTarget,
    },
    Restore {
        from: Option<ChangeId>,
        into: Option<ChangeId>,
        changes_in: Option<ChangeId>,
        selection: ChangeSelection,
    },
    Split {
        change_id: ChangeId,
        target: Option<SplitTarget>,
        selection: ChangeSelection,
    },
    BookmarkCreate {
        name: BookmarkName,
        change_id: ChangeId,
    },
    BookmarkSet {
        name: BookmarkName,
        change_id: ChangeId,
    },
    BookmarkDelete {
        names: SmallVec<BookmarkName>,
    },
    BookmarkForget {
        names: SmallVec<BookmarkName>,
    },
    BookmarkMove {
        name: BookmarkName,
        target: ChangeId,
    },
    BookmarkRename {
        old_name: BookmarkName,
        new_name: BookmarkName,
    },
    BookmarkAdvance {
        change_id: Option<ChangeId>,
    },
    BookmarkTrack {
        bookmarks: SmallVec<BookmarkRef>,
    },
    BookmarkUntrack {
        bookmarks: SmallVec<BookmarkRef>,
    },
    Undo,
    Redo,
    GitFetch {
        all_remotes: bool,
        remote: Option<RemoteName>,
    },
    GitPush {
        all: bool,
        remote: Option<RemoteName>,
    },
    GitPushChange {
        change_id: ChangeId,
        remote: Option<RemoteName>,
    },
    GitPushBookmark {
        bookmarks: SmallVec<BookmarkName>,
        remote: Option<RemoteName>,
    },
    GitFetchBookmark {
        bookmark: BookmarkName,
        remote: RemoteName,
    },
    GitExport,
    GitImport,
    Absorb {
        from: Option<ChangeId>,
        selection: ChangeSelection,
    },
    Commit {
        message: Option<String>,
        selection: ChangeSelection,
    },
    Duplicate {
        change_ids: SmallVec<ChangeId>,
        onto: Option<ChangeId>,
    },
    Parallelize {
        change_ids: SmallVec<ChangeId>,
    },
    SimplifyParents {
        change_ids: SmallVec<ChangeId>,
    },
    Revert {
        change_ids: SmallVec<ChangeId>,
        dest: RebaseTarget,
    },
    WorkspaceAdd {
        path: String,
        name: Option<WorkspaceName>,
        revision: ChangeId,
    },
    WorkspaceForget {
        names: SmallVec<WorkspaceName>,
    },
    WorkspaceList,
    WorkspaceRename {
        new_name: WorkspaceName,
    },
    TagSet {
        name: TagName,
        change_id: ChangeId,
    },
    TagDelete {
        names: SmallVec<TagName>,
    },
    OpRestore {
        op_id: OperationId,
    },
    OpRevert {
        op_id: OperationId,
    },
    OpAbandon {
        op_id: OperationId,
    },
    Fix {
        change_ids: SmallVec<ChangeId>,
        selection: ChangeSelection,
    },
    Run {
        change_ids: SmallVec<ChangeId>,
        /// The command to run in each private working copy (already
        /// shlex-split; jj executes it without a shell).
        argv: Vec<Str>,
        /// Parallelism (`--jobs`); `None` lets jj resolve `run.jobs`.
        jobs: Option<usize>,
    },
    FileUntrack {
        paths: SmallVec<Str>,
    },
    Resolve {
        change_id: ChangeId,
        path: Str,
        tool: ResolveTool,
    },
    Raw {
        args: Vec<Str>,
    },
}

#[derive(Debug, Clone)]
pub enum ResolveTool {
    Ours,
    Theirs,
    Default,
    /// Apply pre-resolved content from a file (written by the per-hunk
    /// picker) via a merge tool that invokes kojutsu itself.
    Content(std::path::PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandPartKind {
    Prompt,
    Binary,
    Subcommand,
    Flag,
    Revision,
    String,
}

#[derive(Clone)]
pub struct CommandPart {
    pub text: String,
    pub kind: CommandPartKind,
}

pub struct JJCommandResult {
    pub display: String,
    pub display_parts: Vec<CommandPart>,
    pub output: Vec<u8>,
    pub success: bool,
    /// The command was killed (Esc/^C) rather than running to completion.
    pub cancelled: bool,
    /// Exit code, when the command ran to completion.
    pub code: Option<i32>,
}

impl JJCommand {
    pub fn with_flag(mut self, flag: CommandFlags) -> Self {
        self.flags |= flag;
        self
    }

    pub fn jump_target(&self) -> Option<JumpTarget> {
        match &self.kind {
            JJCommandKind::New { .. }
            | JJCommandKind::Edit { .. }
            | JJCommandKind::Commit { .. }
            | JJCommandKind::Squash { .. }
            | JJCommandKind::Abandon { .. }
            | JJCommandKind::Absorb { .. }
            | JJCommandKind::Split { .. }
            | JJCommandKind::Diffedit { .. }
            | JJCommandKind::Parallelize { .. }
            | JJCommandKind::SimplifyParents { .. }
            | JJCommandKind::Revert { .. } => Some(JumpTarget::WorkingCopy),
            JJCommandKind::BookmarkTrack { bookmarks, .. } => bookmarks
                .first()
                .map(|br| JumpTarget::Bookmark(br.name.clone())),
            JJCommandKind::BookmarkUntrack { bookmarks, .. } => bookmarks
                .first()
                .map(|br| JumpTarget::Bookmark(br.name.clone())),
            JJCommandKind::BookmarkCreate { name, .. }
            | JJCommandKind::BookmarkSet { name, .. }
            | JJCommandKind::BookmarkMove { name, .. } => Some(JumpTarget::Bookmark(name.clone())),
            JJCommandKind::BookmarkRename { new_name, .. } => {
                Some(JumpTarget::Bookmark(new_name.clone()))
            }
            _ => None,
        }
    }

    pub fn display(&self) -> String {
        let parts = self.display_parts();
        parts
            .iter()
            .map(|p| {
                if p.kind == CommandPartKind::String
                    && p.text
                        .contains(|c: char| c.is_whitespace() || "\"'\\$`!#&|;(){}".contains(c))
                {
                    shlex::try_quote(&p.text)
                        .map(|q| q.into_owned())
                        .unwrap_or_else(|_| p.text.clone())
                } else {
                    p.text.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn display_parts(&self) -> Vec<CommandPart> {
        let mut parts = vec![
            CommandPart {
                text: "$".into(),
                kind: CommandPartKind::Prompt,
            },
            CommandPart {
                text: "jj".into(),
                kind: CommandPartKind::Binary,
            },
        ];
        parts.extend(
            self.tagged_args()
                .into_iter()
                .map(|(text, kind)| CommandPart {
                    text: text.to_string(),
                    kind,
                }),
        );
        parts
    }

    pub fn is_interactive(&self) -> bool {
        let flags = self.flags;
        match &self.kind {
            JJCommandKind::DescribeInEditor { .. } | JJCommandKind::Diffedit { .. } => true,
            JJCommandKind::Squash {
                message, selection, ..
            } => {
                flags.contains(CommandFlags::INTERACTIVE)
                    || matches!(message, MessageMode::Default)
                    || matches!(selection, ChangeSelection::Lines(_))
            }
            JJCommandKind::Commit {
                message, selection, ..
            } => {
                message.is_none()
                    || flags.contains(CommandFlags::INTERACTIVE)
                    || matches!(selection, ChangeSelection::Lines(_))
            }
            JJCommandKind::Restore { selection, .. } => {
                flags.contains(CommandFlags::INTERACTIVE)
                    || matches!(selection, ChangeSelection::Lines(_))
            }
            JJCommandKind::Split { .. } => true,
            JJCommandKind::Resolve { tool, .. } => matches!(tool, ResolveTool::Default),
            JJCommandKind::Raw { .. } => true,
            _ => false,
        }
    }
}
