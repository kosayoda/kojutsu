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
    BookmarkName, ChangeSelection, JumpTarget, MessageMode, OperationId, RebaseSource,
    RebaseTarget, RemoteName, RevisionArg, SmallVec, SplitTarget, SquashTarget, Str, TagName,
    WorkspaceName,
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
        change_ids: SmallVec<RevisionArg>,
    },
    Describe {
        change_ids: SmallVec<RevisionArg>,
        message: String,
    },
    DescribeInEditor {
        change_id: RevisionArg,
    },
    Diffedit {
        change_id: RevisionArg,
        selection: ChangeSelection,
    },
    Edit {
        change_id: RevisionArg,
    },
    New {
        change_ids: SmallVec<RevisionArg>,
        insert: Option<InsertPosition>,
    },
    Squash {
        change_id: RevisionArg,
        target: Option<SquashTarget>,
        message: MessageMode,
        selection: ChangeSelection,
    },
    Rebase {
        change_ids: SmallVec<RevisionArg>,
        source_mode: RebaseSource,
        dest: RebaseTarget,
    },
    Restore {
        from: Option<RevisionArg>,
        into: Option<RevisionArg>,
        changes_in: Option<RevisionArg>,
        selection: ChangeSelection,
    },
    Split {
        change_id: RevisionArg,
        target: Option<SplitTarget>,
        selection: ChangeSelection,
    },
    BookmarkCreate {
        name: BookmarkName,
        change_id: RevisionArg,
    },
    BookmarkSet {
        name: BookmarkName,
        change_id: RevisionArg,
    },
    BookmarkDelete {
        names: SmallVec<BookmarkName>,
    },
    BookmarkForget {
        names: SmallVec<BookmarkName>,
    },
    BookmarkMove {
        name: BookmarkName,
        target: RevisionArg,
    },
    BookmarkRename {
        old_name: BookmarkName,
        new_name: BookmarkName,
    },
    BookmarkAdvance {
        change_id: Option<RevisionArg>,
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
        change_id: RevisionArg,
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
        from: Option<RevisionArg>,
        selection: ChangeSelection,
    },
    Commit {
        message: Option<String>,
        selection: ChangeSelection,
    },
    Duplicate {
        change_ids: SmallVec<RevisionArg>,
        onto: Option<RevisionArg>,
    },
    Parallelize {
        change_ids: SmallVec<RevisionArg>,
    },
    SimplifyParents {
        change_ids: SmallVec<RevisionArg>,
    },
    Revert {
        change_ids: SmallVec<RevisionArg>,
        dest: RebaseTarget,
    },
    WorkspaceAdd {
        path: String,
        name: Option<WorkspaceName>,
        revision: RevisionArg,
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
        change_id: RevisionArg,
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
        change_ids: SmallVec<RevisionArg>,
        selection: ChangeSelection,
    },
    Run {
        change_ids: SmallVec<RevisionArg>,
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
        change_id: RevisionArg,
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
    Fileset,
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
        self.display_parts()
            .iter()
            .map(|p| p.text.as_str())
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
        parts.extend(self.tagged_args().into_iter().map(|(text, kind)| {
            let text = if matches!(kind, CommandPartKind::String | CommandPartKind::Fileset)
                && text.contains(|c: char| c.is_whitespace() || "\"'\\$`!#&|;(){}".contains(c))
            {
                shlex::try_quote(&text)
                    .map(|q| q.into_owned())
                    .unwrap_or_else(|_| text.to_string())
            } else {
                text.to_string()
            };
            CommandPart { text, kind }
        }));
        parts
    }

    /// Whether the command needs the terminal handed to it, rather than being
    /// run with its output captured.
    ///
    /// Two reasons, handled differently on purpose. A `--interactive` diff
    /// editor is read back off the built args: several kinds pick that flag up
    /// from `CommandFlags::INTERACTIVE` or from a line-level selection, and
    /// restating those rules here is how the two copies drift — a kind that
    /// gained the flag without gaining a case would launch a diff editor
    /// against a closed stdin. Opening `$EDITOR` can't be read off the args,
    /// since it's the *absence* of `-m` or a tool default, so those stay
    /// listed; a miss there is milder, because captured runs set
    /// `JJ_EDITOR=:` and the edit is simply skipped.
    pub fn is_interactive(&self) -> bool {
        if self.args().iter().any(|a| a.as_str() == "--interactive") {
            return true;
        }
        match &self.kind {
            JJCommandKind::DescribeInEditor { .. }
            | JJCommandKind::Diffedit { .. }
            | JJCommandKind::Split { .. } => true,
            JJCommandKind::Squash { message, .. } => matches!(message, MessageMode::Default),
            JJCommandKind::Commit { message, .. } => message.is_none(),
            JJCommandKind::Resolve { tool, .. } => matches!(tool, ResolveTool::Default),
            // A command line the user typed; assume it may want the terminal.
            JJCommandKind::Raw { .. } => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod is_interactive_tests {
    use super::*;
    use crate::types::{ChangeSelection, MessageMode, RevisionArg};

    fn squash(message: MessageMode, selection: ChangeSelection, flags: CommandFlags) -> JJCommand {
        JJCommand {
            kind: JJCommandKind::Squash {
                change_id: RevisionArg::new("qpvuntsm"),
                target: None,
                message,
                selection,
            },
            flags,
        }
    }

    #[test]
    fn a_captured_squash_needs_no_terminal() {
        let cmd = squash(
            MessageMode::Inline("m".into()),
            ChangeSelection::All,
            CommandFlags::empty(),
        );
        assert!(!cmd.args().iter().any(|a| a.as_str() == "--interactive"));
        assert!(!cmd.is_interactive());
    }

    /// The reason to read the args rather than restate the rules: every way a
    /// command can acquire --interactive is covered by construction.
    #[test]
    fn anything_passing_interactive_is_interactive() {
        let by_flag = squash(
            MessageMode::Inline("m".into()),
            ChangeSelection::All,
            CommandFlags::INTERACTIVE,
        );
        assert!(by_flag.is_interactive());

        let by_selection = squash(
            MessageMode::Inline("m".into()),
            ChangeSelection::Lines(std::path::PathBuf::from("/tmp/sel.json")),
            CommandFlags::empty(),
        );
        assert!(by_selection.is_interactive());
    }

    /// Opening $EDITOR isn't visible in the args — it's the absence of -m.
    #[test]
    fn an_editor_message_still_needs_the_terminal() {
        let cmd = squash(
            MessageMode::Default,
            ChangeSelection::All,
            CommandFlags::empty(),
        );
        assert!(!cmd.args().iter().any(|a| a.as_str() == "--interactive"));
        assert!(cmd.is_interactive());
    }
}
