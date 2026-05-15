use std::path::Path;
use std::process::{Command, Output};

use compact_str::format_compact;

use crate::app::{JumpTarget, GLOBAL_TOGGLES};
use crate::dag::BookmarkRef;
use crate::keymap::CommandFlags;
use strum::IntoEnumIterator;

use crate::types::{
    BookmarkName, ChangeId, ChangeSelection, MessageMode, OperationId, PendingCommand,
    PendingCommitSelect, ReadyCommand, RebaseKind, RebaseSource, RebaseTarget, RemoteName,
    SmallVec, SplitTarget, SquashTarget, Str, TagName, TargetOperation, WorkspaceName,
};

/// Where to insert a new commit relative to its parent.
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

/// A typesafe representation of a jj CLI command.
#[derive(Debug, Clone)]
pub struct JJCommand {
    pub kind: JJCommandKind,
    pub flags: CommandFlags,
}

/// The specific jj subcommand and its arguments.
#[derive(Debug, Clone)]
pub enum JJCommandKind {
    Abandon {
        change_ids: SmallVec<ChangeId>,
    },
    /// Describe with an inline message (non-interactive).
    Describe {
        change_ids: SmallVec<ChangeId>,
        message: String,
    },
    /// Describe via jj's configured editor (interactive -- needs terminal).
    DescribeInEditor {
        change_id: ChangeId,
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
        /// How to filter changes.
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
        /// Source revision. `None` = default (absorb from @).
        from: Option<ChangeId>,
        /// How to filter changes. Absorb supports all changes or file-level selection.
        selection: ChangeSelection,
    },
    Commit {
        /// Inline message. `None` = open $EDITOR.
        message: Option<String>,
        /// How to filter changes.
        selection: ChangeSelection,
    },
    Duplicate {
        change_ids: SmallVec<ChangeId>,
        /// Target revision for `--onto`. `None` = duplicate onto same parents.
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
    },
    FileUntrack {
        paths: SmallVec<Str>,
    },
    Resolve {
        change_id: ChangeId,
        path: Str,
        tool: ResolveTool,
    },
}

#[derive(Debug, Clone)]
pub enum ResolveTool {
    Ours,
    Theirs,
    Default,
}

/// What role a token plays in a jj command line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandPartKind {
    Prompt,
    Binary,
    Subcommand,
    Flag,
    Revision,
    String,
}

/// A single token in a syntax-highlighted jj command line.
#[derive(Clone)]
pub struct CommandPart {
    pub text: String,
    pub kind: CommandPartKind,
}

/// The result of running a jj command.
pub struct JJCommandResult {
    /// The command that was displayed to the user.
    pub display: String,
    /// Structured, syntax-highlightable command parts.
    pub display_parts: Vec<CommandPart>,
    /// Raw stdout+stderr bytes (with ANSI color codes).
    pub output: Vec<u8>,
    /// Whether the command exited successfully.
    pub success: bool,
}

impl JJCommand {
    /// Return a clone of this command with an additional flag set.
    pub fn with_flag(mut self, flag: CommandFlags) -> Self {
        self.flags |= flag;
        self
    }

    /// Build follow-up retry options for a failed jj command based on its error
    /// output. Returns an empty vec when no retry is applicable.
    pub fn retry_options(&self, output: &[u8]) -> Vec<FollowUpOption> {
        let mut options = Vec::new();
        let text = String::from_utf8_lossy(output);

        if text.contains("immutable") {
            options.push(FollowUpOption {
                key: 'r',
                label: "retry with --ignore-immutable",
                action: FollowUpAction::Execute(
                    self.clone().with_flag(CommandFlags::IGNORE_IMMUTABLE),
                ),
            });
        } else if text.contains("stale") && text.contains("working copy") {
            options.push(FollowUpOption {
                key: 'r',
                label: "retry with --ignore-working-copy",
                action: FollowUpAction::Execute(
                    self.clone().with_flag(CommandFlags::IGNORE_WORKING_COPY),
                ),
            });
        }

        options
    }

    /// Where to jump the cursor after this command succeeds.
    pub fn jump_target(&self) -> Option<JumpTarget> {
        match &self.kind {
            // Commands that move @.
            JJCommandKind::New { .. }
            | JJCommandKind::Edit { .. }
            | JJCommandKind::Commit { .. }
            | JJCommandKind::Squash { .. }
            | JJCommandKind::Abandon { .. }
            | JJCommandKind::Absorb { .. }
            | JJCommandKind::Split { .. }
            | JJCommandKind::Parallelize { .. }
            | JJCommandKind::SimplifyParents { .. }
            | JJCommandKind::Revert { .. } => Some(JumpTarget::WorkingCopy),
            // Bookmark operations: jump to the affected bookmark.
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
            // Everything else: use normal ChangeId restoration.
            _ => None,
        }
    }

    /// Build the CLI arguments for `jj`.
    pub fn args(&self) -> Vec<Str> {
        let flags = self.flags;
        let mut args = match &self.kind {
            JJCommandKind::Abandon { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["abandon".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::RETAIN_BOOKMARKS, "--retain-bookmarks"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                    ],
                );
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::Describe {
                change_ids,
                message,
                ..
            } => {
                let mut args: Vec<Str> =
                    vec!["describe".into(), "-m".into(), Str::from(message.as_str())];
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::DescribeInEditor { change_id, .. } => {
                vec!["describe".into(), format_compact!("{change_id}")]
            }
            JJCommandKind::Edit { change_id, .. } => {
                vec!["edit".into(), format_compact!("{change_id}")]
            }
            JJCommandKind::New {
                change_ids, insert, ..
            } => {
                let mut args: Vec<Str> = vec!["new".into()];
                if let Some(pos) = insert {
                    args.push(pos.flag().into());
                }
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                push_flags(&mut args, flags, &[(CommandFlags::NO_EDIT, "--no-edit")]);
                args
            }
            JJCommandKind::Rebase {
                change_ids,
                source_mode,
                dest,
                ..
            } => {
                let mut args: Vec<Str> = vec!["rebase".into()];
                let flag = match source_mode {
                    RebaseSource::Revision => "-r",
                    RebaseSource::Source => "-s",
                    RebaseSource::Branch => "-b",
                };
                for id in change_ids {
                    args.push(flag.into());
                    args.push(format_compact!("{id}"));
                }
                for target in &dest.targets {
                    args.push(format_compact!("{}", dest.kind.flag()));
                    args.push(format_compact!("{target}"));
                }
                args
            }
            JJCommandKind::Restore {
                from,
                into,
                changes_in,
                selection,
                ..
            } => {
                let mut args: Vec<Str> = vec!["restore".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                    ],
                );
                if let Some(id) = from {
                    args.push("--from".into());
                    args.push(format_compact!("{id}"));
                }
                if let Some(id) = into {
                    args.push("--into".into());
                    args.push(format_compact!("{id}"));
                }
                if let Some(id) = changes_in {
                    args.push("--changes-in".into());
                    args.push(format_compact!("{id}"));
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::Split {
                change_id,
                target,
                selection,
                ..
            } => {
                let mut args: Vec<Str> =
                    vec!["split".into(), "-r".into(), format_compact!("{change_id}")];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::PARALLEL, "--parallel"),
                    ],
                );
                if let Some(t) = target {
                    args.push(format_compact!("{}", t.kind.flag()));
                    args.push(format_compact!("{}", t.target));
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::BookmarkCreate {
                name, change_id, ..
            } => {
                vec![
                    "bookmark".into(),
                    "create".into(),
                    "-r".into(),
                    format_compact!("{change_id}"),
                    Str::from(name.as_str()),
                ]
            }
            JJCommandKind::BookmarkSet {
                name, change_id, ..
            } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "set".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push("-r".into());
                args.push(format_compact!("{change_id}"));
                args.push(Str::from(name.as_str()));
                args
            }
            JJCommandKind::BookmarkDelete { names, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "delete".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommandKind::BookmarkForget { names, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "forget".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommandKind::BookmarkMove { name, target, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "move".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push("--to".into());
                args.push(format_compact!("{target}"));
                args.push(Str::from(name.as_str()));
                args
            }
            JJCommandKind::BookmarkRename {
                old_name, new_name, ..
            } => {
                vec![
                    "bookmark".into(),
                    "rename".into(),
                    Str::from(old_name.as_str()),
                    Str::from(new_name.as_str()),
                ]
            }
            JJCommandKind::BookmarkAdvance { change_id, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "advance".into()];
                if let Some(id) = change_id {
                    args.push("--to".into());
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::BookmarkTrack { bookmarks, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "track".into()];
                for br in bookmarks {
                    args.push(Str::from(br.name.as_str()));
                    args.push("--remote".into());
                    args.push(Str::from(br.remote.as_str()));
                }
                args
            }
            JJCommandKind::BookmarkUntrack { bookmarks, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "untrack".into()];
                for br in bookmarks {
                    args.push(Str::from(br.name.as_str()));
                    args.push("--remote".into());
                    args.push(Str::from(br.remote.as_str()));
                }
                args
            }
            JJCommandKind::Undo => vec!["undo".into()],
            JJCommandKind::Redo => vec!["redo".into()],
            JJCommandKind::GitFetch {
                all_remotes,
                remote,
                ..
            } => {
                let mut args: Vec<Str> = vec!["git".into(), "fetch".into()];
                if *all_remotes {
                    args.push("--all-remotes".into());
                }
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(Str::from(r.as_str()));
                }
                args
            }
            JJCommandKind::GitPush { all, remote, .. } => {
                let mut args: Vec<Str> = vec!["git".into(), "push".into()];
                if *all {
                    args.push("--all".into());
                }
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(Str::from(r.as_str()));
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommandKind::GitPushChange {
                change_id, remote, ..
            } => {
                let mut args: Vec<Str> = vec!["git".into(), "push".into()];
                args.push("-c".into());
                args.push(format_compact!("{change_id}"));
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(Str::from(r.as_str()));
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommandKind::GitPushBookmark {
                bookmarks, remote, ..
            } => {
                let mut args: Vec<Str> = vec!["git".into(), "push".into()];
                for name in bookmarks {
                    args.push("--bookmark".into());
                    args.push(Str::from(name.as_str()));
                }
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(Str::from(r.as_str()));
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommandKind::GitFetchBookmark {
                bookmark, remote, ..
            } => {
                vec![
                    "git".into(),
                    "fetch".into(),
                    "-b".into(),
                    Str::from(bookmark.as_str()),
                    "--remote".into(),
                    Str::from(remote.as_str()),
                ]
            }
            JJCommandKind::GitExport => vec!["git".into(), "export".into()],
            JJCommandKind::GitImport => vec!["git".into(), "import".into()],
            JJCommandKind::Absorb {
                from, selection, ..
            } => {
                let mut args: Vec<Str> = vec!["absorb".into()];
                if let Some(id) = from {
                    args.push("--from".into());
                    args.push(format_compact!("{id}"));
                }
                match selection {
                    ChangeSelection::All => {}
                    ChangeSelection::Files(paths) => {
                        args.extend(paths.iter().cloned());
                    }
                    ChangeSelection::Lines(_) => {
                        debug_assert!(false, "line selection should be blocked for absorb");
                    }
                }
                args
            }
            JJCommandKind::Commit {
                message, selection, ..
            } => {
                let mut args: Vec<Str> = vec!["commit".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::INTERACTIVE, "--interactive")],
                );
                if let Some(msg) = message {
                    args.push("-m".into());
                    args.push(Str::from(msg.as_str()));
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::Parallelize { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["parallelize".into()];
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::SimplifyParents { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["simplify-parents".into()];
                for id in change_ids {
                    args.push("-r".into());
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::Revert {
                change_ids, dest, ..
            } => {
                let mut args: Vec<Str> = vec!["revert".into()];
                for id in change_ids {
                    args.push("-r".into());
                    args.push(format_compact!("{id}"));
                }
                for target in &dest.targets {
                    args.push(dest.kind.flag().into());
                    args.push(format_compact!("{target}"));
                }
                args
            }
            JJCommandKind::Duplicate {
                change_ids, onto, ..
            } => {
                let mut args: Vec<Str> = vec!["duplicate".into()];
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                if let Some(target) = onto {
                    args.push("--onto".into());
                    args.push(format_compact!("{target}"));
                }
                args
            }
            JJCommandKind::Squash {
                change_id,
                target,
                message,
                selection,
                ..
            } => {
                let mut args: Vec<Str> = vec!["squash".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::KEEP_EMPTIED, "--keep-emptied"),
                    ],
                );
                match message {
                    MessageMode::Default => {}
                    MessageMode::Inline(msg) => {
                        args.push("-m".into());
                        args.push(Str::from(msg.as_str()));
                    }
                    MessageMode::UseDestination => {
                        args.push("--use-destination-message".into());
                    }
                }
                match target {
                    None => {
                        args.push("-r".into());
                        args.push(format_compact!("{change_id}"));
                    }
                    Some(t) => {
                        args.push("--from".into());
                        args.push(format_compact!("{change_id}"));
                        args.push(format_compact!("{}", t.kind.flag()));
                        args.push(format_compact!("{}", t.target));
                    }
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::WorkspaceAdd {
                path,
                name,
                revision,
                ..
            } => {
                let mut args: Vec<Str> = vec!["workspace".into(), "add".into()];
                if let Some(n) = name {
                    args.push("--name".into());
                    args.push(Str::from(n.as_str()));
                }
                args.push("-r".into());
                args.push(format_compact!("{revision}"));
                args.push(Str::from(path.as_str()));
                args
            }
            JJCommandKind::WorkspaceForget { names, .. } => {
                let mut args: Vec<Str> = vec!["workspace".into(), "forget".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommandKind::WorkspaceList => {
                vec!["workspace".into(), "list".into()]
            }
            JJCommandKind::WorkspaceRename { new_name, .. } => {
                vec![
                    "workspace".into(),
                    "rename".into(),
                    Str::from(new_name.as_str()),
                ]
            }
            JJCommandKind::TagSet {
                name, change_id, ..
            } => {
                let mut args: Vec<Str> = vec!["tag".into(), "set".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push("-r".into());
                args.push(format_compact!("{change_id}"));
                args.push(Str::from(name.as_str()));
                args
            }
            JJCommandKind::TagDelete { names, .. } => {
                let mut args: Vec<Str> = vec!["tag".into(), "delete".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommandKind::OpRestore { op_id, .. } => {
                vec!["op".into(), "restore".into(), Str::from(op_id.as_str())]
            }
            JJCommandKind::OpRevert { op_id, .. } => {
                vec!["op".into(), "revert".into(), Str::from(op_id.as_str())]
            }
            JJCommandKind::OpAbandon { op_id, .. } => {
                vec!["op".into(), "abandon".into(), Str::from(op_id.as_str())]
            }
            JJCommandKind::Fix { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["fix".into(), "-s".into()];
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::FileUntrack { paths, .. } => {
                let mut args: Vec<Str> = vec!["file".into(), "untrack".into()];
                args.extend(paths.iter().cloned());
                args
            }
            JJCommandKind::Resolve {
                change_id,
                path,
                tool,
                ..
            } => {
                let mut args: Vec<Str> = vec!["resolve".into()];
                args.push("-r".into());
                args.push(format_compact!("{change_id}"));
                match tool {
                    ResolveTool::Ours => args.push("--tool=:ours".into()),
                    ResolveTool::Theirs => args.push("--tool=:theirs".into()),
                    ResolveTool::Default => {}
                }
                args.push(path.clone());
                args
            }
        };

        // Append global flags (ignore-immutable, etc.) once at the end.
        push_global_flags(&mut args, flags);
        args
    }

    /// Human-readable display string shown in the command output overlay.
    ///
    /// Arguments containing spaces or special characters are quoted so the
    /// display looks like a valid shell command the user could copy-paste.
    pub fn display(&self) -> String {
        let parts = self.display_parts();
        parts
            .iter()
            .map(|p| {
                // Only quote user-provided values (strings); structural parts
                // (prompt, binary, subcommand, flag, revision) are always safe.
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

    /// Structured command parts for syntax-highlighted display.
    pub fn display_parts(&self) -> Vec<CommandPart> {
        let args = self.args();
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
        tag_args(&args, &mut parts);
        parts
    }

    /// Whether this command needs an interactive terminal (editor/diff tool).
    pub fn is_interactive(&self) -> bool {
        let flags = self.flags;
        match &self.kind {
            JJCommandKind::DescribeInEditor { .. } => true,
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
            _ => false,
        }
    }

    /// Execute an interactive command that inherits the terminal.
    ///
    /// The caller must restore the terminal before calling this and
    /// re-initialize it after.
    pub fn run_interactive(&self, repo_path: &Path) -> JJCommandResult {
        let args = self.args();
        let display = self.display();
        let display_parts = self.display_parts();

        let (result, was_interrupted) = with_sigint_suppressed(|| {
            Command::new("jj")
                .args(&args)
                .arg("-R")
                .arg(repo_path)
                .arg("--color=always")
                .current_dir(repo_path)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                // Capture stderr so error messages are available for display.
                .stderr(std::process::Stdio::piped())
                .output()
        });

        match result {
            Ok(Output { status, .. }) if was_interrupted || status.code().is_none() => {
                JJCommandResult {
                    display,
                    display_parts,
                    output: b"interrupted".to_vec(),
                    success: false,
                }
            }
            Ok(Output { stderr, status, .. }) => JJCommandResult {
                display,
                display_parts,
                output: stderr,
                success: status.success(),
            },
            Err(e) => jj_error(display, display_parts, e),
        }
    }

    /// Execute a suspended command with inherited stdin but captured stdout/stderr.
    ///
    /// Used for commands that need terminal access for SSH prompts but don't
    /// need an interactive editor. Output is captured and returned for display
    /// in the TUI overlay.
    pub fn run_suspend_captured(&self, repo_path: &Path) -> JJCommandResult {
        let args = self.args();
        let display = self.display();
        let display_parts = self.display_parts();

        let (result, was_interrupted) = with_sigint_suppressed(|| {
            let mut child = Command::new("jj")
                .args(&args)
                .arg("-R")
                .arg(repo_path)
                .arg("--color=always")
                .current_dir(repo_path)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            // Tee stdout and stderr: forward each chunk to the real
            // terminal in real-time (so progress bars are visible) while
            // also accumulating the bytes for the post-command overlay.
            let child_stdout = child.stdout.take().unwrap();
            let child_stderr = child.stderr.take().unwrap();

            let stdout_thread = std::thread::spawn(move || tee_pipe(child_stdout));
            let stderr_thread = std::thread::spawn(move || tee_pipe(child_stderr));

            let status = child.wait()?;
            let stdout = stdout_thread.join().unwrap_or_default();
            let stderr = stderr_thread.join().unwrap_or_default();

            Ok(Output {
                status,
                stdout,
                stderr,
            })
        });

        match result {
            Ok(Output { status, .. }) if was_interrupted || status.code().is_none() => {
                JJCommandResult {
                    display,
                    display_parts,
                    output: b"interrupted".to_vec(),
                    success: false,
                }
            }
            Ok(Output {
                stdout,
                stderr,
                status,
            }) => JJCommandResult {
                display,
                display_parts,
                output: merge_captured_output(stdout, stderr),
                success: status.success(),
            },
            Err(e) => jj_error(display, display_parts, e),
        }
    }

    /// Execute the command against a repository path (captured output).
    pub fn run(&self, repo_path: &Path) -> JJCommandResult {
        let args = self.args();
        let display = self.display();
        let display_parts = self.display_parts();

        let result = Command::new("jj")
            .args(&args)
            .arg("-R")
            .arg(repo_path)
            .arg("--color=always")
            .current_dir(repo_path)
            // Safety: use a no-op editor so that if jj unexpectedly opens
            // an editor in captured mode, it won't hang waiting for input.
            .env("JJ_EDITOR", ":")
            .output();

        match result {
            Ok(Output {
                stdout,
                stderr,
                status,
            }) => JJCommandResult {
                display,
                display_parts,
                output: merge_captured_output(stdout, stderr),
                success: status.success(),
            },
            Err(e) => jj_error(display, display_parts, e),
        }
    }
}

/// Read from a child process pipe, writing each chunk to the terminal in
/// real-time (so progress bars are visible) while accumulating the full output.
fn tee_pipe(mut pipe: impl std::io::Read + Send + 'static) -> Vec<u8> {
    use std::io::Write;
    let mut buf = [0u8; 4096];
    let mut captured = Vec::new();
    // Always write to stderr so progress bars and output appear on the
    // terminal without interfering with any stdout redirection.
    let mut out = std::io::stderr();
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                captured.extend_from_slice(chunk);
                let _ = out.write_all(chunk);
                let _ = out.flush();
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    captured
}

/// Merge stdout and stderr into a single output buffer with a newline separator.
fn merge_captured_output(mut stdout: Vec<u8>, stderr: Vec<u8>) -> Vec<u8> {
    if !stderr.is_empty() {
        if !stdout.is_empty() && !stdout.ends_with(b"\n") {
            stdout.push(b'\n');
        }
        stdout.extend_from_slice(&stderr);
    }
    stdout
}

/// Build a `JJCommandResult` for a failed jj invocation.
fn jj_error(
    display: String,
    display_parts: Vec<CommandPart>,
    err: std::io::Error,
) -> JJCommandResult {
    JJCommandResult {
        display,
        display_parts,
        output: format!("failed to run jj: {err}").into_bytes(),
        success: false,
    }
}

/// Push CLI flag arguments for any active command-specific flags.
fn push_flags(args: &mut Vec<Str>, flags: CommandFlags, mapping: &[(CommandFlags, &str)]) {
    for (flag, arg) in mapping {
        if flags.contains(*flag) {
            args.push(Str::from(*arg));
        }
    }
}

/// Push args for change selection (file paths or --tool for line-level).
fn push_change_selection(args: &mut Vec<Str>, selection: &ChangeSelection) {
    match selection {
        ChangeSelection::All => {}
        ChangeSelection::Files(paths) => {
            args.extend(paths.iter().cloned());
        }
        ChangeSelection::Lines(json_path) => {
            let exe = std::env::current_exe().unwrap_or_else(|_| "kojutsu".into());
            args.extend([
                "--interactive".into(),
                "--tool".into(),
                "kojutsu-select".into(),
                "--config".into(),
                Str::from(format!(
                    "merge-tools.kojutsu-select.program={}",
                    toml_string_escape(&exe.display().to_string())
                )),
                "--config".into(),
                Str::from(format!(
                    "merge-tools.kojutsu-select.edit-args=[\"--apply-diff\", {}, \"$left\", \"$right\"]",
                    toml_string_escape(&json_path.display().to_string())
                )),
            ]);
        }
    }
}

/// Run `body` with SIGINT suppressed in the parent process.
///
/// Ignores SIGINT while the child runs so that Ctrl-C during an SSH password
/// prompt (or editor) doesn't kill our process. The child still receives
/// SIGINT normally since it has its own signal disposition after exec.
///
/// Returns `(result, was_interrupted)`.
fn with_sigint_suppressed<T>(body: impl FnOnce() -> T) -> (T, bool) {
    let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook = signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone()).ok();
    let result = body();
    if let Some(id) = hook {
        signal_hook::low_level::unregister(id);
    }
    (
        result,
        interrupted.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Escape a string for use in jj --config TOML values.
fn toml_string_escape(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Push CLI flags for all active global toggles (ignore-immutable, etc.).
/// Called once at the end of `args()` so individual variants don't need to.
fn push_global_flags(args: &mut Vec<Str>, flags: CommandFlags) {
    for toggle in GLOBAL_TOGGLES {
        if flags.contains(toggle.flag) {
            args.push(Str::from(toggle.cli_flag));
        }
    }
}

/// Two-word jj subcommands (first word → second word is also a subcommand).
const COMPOUND_SUBCOMMANDS: &[&str] = &["git", "bookmark", "workspace", "tag", "op", "file"];

/// Flags whose next argument is a revision/change ID.
const REVISION_FLAGS: &[&str] = &[
    "-r", "-s", "-b", "-c", "--from", "--into", "--to", "--onto", "-d",
];

/// Tag a flat argument list with `CommandPartKind`.
fn tag_args(args: &[Str], parts: &mut Vec<CommandPart>) {
    if args.is_empty() {
        return;
    }

    let mut i = 0;

    // First arg is always a subcommand.
    parts.push(CommandPart {
        text: args[0].to_string(),
        kind: CommandPartKind::Subcommand,
    });
    i += 1;

    // If it's a compound subcommand, the second arg is also a subcommand.
    if COMPOUND_SUBCOMMANDS.contains(&args[0].as_str()) {
        if let Some(arg) = args.get(i) {
            if !arg.starts_with('-') {
                parts.push(CommandPart {
                    text: arg.to_string(),
                    kind: CommandPartKind::Subcommand,
                });
                i += 1;
            }
        }
    }

    // Tag remaining args.
    let mut next_is_revision = false;
    while i < args.len() {
        let arg = &args[i];
        if next_is_revision {
            parts.push(CommandPart {
                text: arg.to_string(),
                kind: CommandPartKind::Revision,
            });
            next_is_revision = false;
        } else if arg.starts_with('-') {
            parts.push(CommandPart {
                text: arg.to_string(),
                kind: CommandPartKind::Flag,
            });
            if REVISION_FLAGS.contains(&arg.as_str()) {
                next_is_revision = true;
            }
        } else {
            // Bare positional arg — could be a revision or a string.
            // For commands that take bare revisions (abandon, edit, new, parallelize,
            // duplicate, fix), these are revisions. Heuristic: if it looks like a
            // hex/reverse-hex ID (only [a-z0-9/] and short), treat as revision.
            let kind = if looks_like_revision(arg) {
                CommandPartKind::Revision
            } else {
                CommandPartKind::String
            };
            parts.push(CommandPart {
                text: arg.to_string(),
                kind,
            });
        }
        i += 1;
    }
}

/// Heuristic: does this look like a jj change ID or commit ID?
fn looks_like_revision(s: &str) -> bool {
    // Change IDs are reverse-hex [a-z], commit IDs are hex [0-9a-f].
    // Allow `/` for divergence suffix (e.g. "xvzw/2").
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '/')
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

impl PendingCommitSelect {
    pub fn into_jj_command(self, target: ChangeId, flags: CommandFlags) -> JJCommand {
        match self {
            PendingCommitSelect::WorkspaceAdd { path, name } => JJCommand {
                kind: JJCommandKind::WorkspaceAdd {
                    path,
                    name,
                    revision: target,
                },
                flags,
            },
        }
    }
}

impl PendingCommand {
    /// Convert to a `JJCommand` given the user's input text.
    ///
    /// Returns `None` for variants that are handled separately (`Revset`,
    /// `WorkspaceAddPath`, `WorkspaceAddName`).
    pub fn into_jj_command(self, text: String) -> Option<JJCommand> {
        match self {
            PendingCommand::Describe { change_ids, flags } => Some(JJCommand {
                kind: JJCommandKind::Describe {
                    change_ids,
                    message: text,
                },
                flags,
            }),
            PendingCommand::SquashWithMessage { builder, flags } => {
                Some(builder.build(text, flags))
            }
            PendingCommand::Revset
            | PendingCommand::WorkspaceAddPath { .. }
            | PendingCommand::WorkspaceAddName { .. } => None,
            PendingCommand::BookmarkCreate { change_id, flags } => Some(JJCommand {
                kind: JJCommandKind::BookmarkCreate {
                    name: BookmarkName::new(text),
                    change_id,
                },
                flags,
            }),
            PendingCommand::BookmarkSet { change_id, flags } => Some(JJCommand {
                kind: JJCommandKind::BookmarkSet {
                    name: BookmarkName::new(text),
                    change_id,
                },
                flags,
            }),
            PendingCommand::BookmarkSetByName { name, flags } => Some(JJCommand {
                kind: JJCommandKind::BookmarkSet {
                    name,
                    change_id: ChangeId::new(text),
                },
                flags,
            }),
            PendingCommand::BookmarkRename { old_name, flags } => Some(JJCommand {
                kind: JJCommandKind::BookmarkRename {
                    old_name,
                    new_name: BookmarkName::new(text),
                },
                flags,
            }),
            PendingCommand::TagSet { change_id, flags } => Some(JJCommand {
                kind: JJCommandKind::TagSet {
                    name: TagName::new(text),
                    change_id,
                },
                flags,
            }),
            PendingCommand::TagSetByName { name, flags } => Some(JJCommand {
                kind: JJCommandKind::TagSet {
                    name,
                    change_id: ChangeId::new(text),
                },
                flags,
            }),
            PendingCommand::Commit { flags, selection } => Some(JJCommand {
                kind: JJCommandKind::Commit {
                    message: Some(text),
                    selection,
                },
                flags,
            }),
            PendingCommand::WorkspaceRename { flags } => Some(JJCommand {
                kind: JJCommandKind::WorkspaceRename {
                    new_name: WorkspaceName::new(text),
                },
                flags,
            }),
        }
    }
}

impl TargetOperation {
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
                JJCommand {
                    kind: JJCommandKind::Split {
                        change_id: source,
                        target: Some(SplitTarget {
                            target: targets.into_iter().next().expect("target required"),
                            kind,
                        }),
                        selection,
                    },
                    flags,
                },
            ),
            TargetOperation::Rebase {
                source_mode,
                sources,
            } => rebase_follow_up(sources, targets, source_mode, flags),
            TargetOperation::RestoreFrom => auto_follow_up(
                "restore",
                JJCommand {
                    kind: JJCommandKind::Restore {
                        from: targets.into_iter().next(),
                        into: None,
                        changes_in: None,
                        selection,
                    },
                    flags,
                },
            ),
            TargetOperation::RestoreInto => auto_follow_up(
                "restore",
                JJCommand {
                    kind: JJCommandKind::Restore {
                        from: None,
                        into: targets.into_iter().next(),
                        changes_in: None,
                        selection,
                    },
                    flags,
                },
            ),
            TargetOperation::BookmarkMove { bookmark_name } => auto_follow_up(
                "move",
                JJCommand {
                    kind: JJCommandKind::BookmarkMove {
                        name: bookmark_name.clone(),
                        target: targets.into_iter().next().expect("target required"),
                    },
                    flags,
                },
            ),
            TargetOperation::DuplicateOnto => auto_follow_up(
                "duplicate",
                JJCommand {
                    kind: JJCommandKind::Duplicate {
                        change_ids: smallvec::smallvec![source],
                        onto: targets.into_iter().next(),
                    },
                    flags,
                },
            ),
            TargetOperation::Revert { sources } => RebaseKind::iter()
                .map(|kind| FollowUpOption {
                    key: kind.key(),
                    label: kind.label(),
                    action: FollowUpAction::Execute(JJCommand {
                        kind: JJCommandKind::Revert {
                            change_ids: sources.clone(),
                            dest: RebaseTarget {
                                targets: targets.clone(),
                                kind,
                            },
                        },
                        flags,
                    }),
                })
                .collect(),
        }
    }
}

impl ReadyCommand {
    /// Build a command with an inline message.
    pub fn build(self, message: String, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash {
                source,
                target,
                selection,
            } => JJCommand {
                kind: JJCommandKind::Squash {
                    change_id: source,
                    target,
                    message: MessageMode::Inline(message),
                    selection,
                },
                flags,
            },
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
    let default_cmd = JJCommand {
        kind: JJCommandKind::Squash {
            change_id: source.clone(),
            target: target.clone(),
            message: MessageMode::Default,
            selection: selection.clone(),
        },
        flags,
    };
    let use_dest_cmd = JJCommand {
        kind: JJCommandKind::Squash {
            change_id: source.clone(),
            target: target.clone(),
            message: MessageMode::UseDestination,
            selection: selection.clone(),
        },
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
            action: FollowUpAction::Execute(JJCommand {
                kind: JJCommandKind::Rebase {
                    change_ids: sources.clone(),
                    source_mode: source_mode.clone(),
                    dest: RebaseTarget {
                        targets: targets.clone(),
                        kind,
                    },
                },
                flags,
            }),
        })
        .collect()
}
