use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use compact_str::format_compact;

use crate::app::{JumpTarget, GLOBAL_TOGGLES};
use crate::dag::BookmarkRef;
use crate::keymap::CommandFlags;
use crate::types::{
    BookmarkName, ChangeId, MessageMode, RebaseSource, RebaseTarget, SmallVec, SplitTarget,
    SquashTarget, Str,
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

/// How to filter changes for squash/commit operations.
#[derive(Debug, Clone)]
pub enum ChangeSelection {
    /// Include all changes (no filtering).
    All,
    /// Include only these files (maps to [FILESETS] positional args).
    Files(Vec<Str>),
    /// Line-level selection (maps to --interactive --tool with selection JSON).
    Lines(PathBuf),
}

/// A typesafe representation of a jj CLI command.
#[derive(Debug, Clone)]
pub enum JJCommand {
    Abandon {
        change_ids: SmallVec<ChangeId>,
        flags: CommandFlags,
    },
    /// Describe with an inline message (non-interactive).
    Describe {
        change_ids: SmallVec<ChangeId>,
        message: String,
        flags: CommandFlags,
    },
    /// Describe via jj's configured editor (interactive -- needs terminal).
    DescribeInEditor {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    Edit {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    New {
        change_ids: SmallVec<ChangeId>,
        insert: Option<InsertPosition>,
        flags: CommandFlags,
    },
    Squash {
        change_id: ChangeId,
        target: Option<SquashTarget>,
        message: MessageMode,
        /// How to filter changes.
        selection: ChangeSelection,
        flags: CommandFlags,
    },
    Rebase {
        change_ids: SmallVec<ChangeId>,
        source_mode: RebaseSource,
        dest: RebaseTarget,
        flags: CommandFlags,
    },
    Restore {
        from: Option<ChangeId>,
        into: Option<ChangeId>,
        changes_in: Option<ChangeId>,
        selection: ChangeSelection,
        flags: CommandFlags,
    },
    Split {
        change_id: ChangeId,
        target: Option<SplitTarget>,
        selection: ChangeSelection,
        flags: CommandFlags,
    },
    BookmarkCreate {
        name: BookmarkName,
        change_id: ChangeId,
        flags: CommandFlags,
    },
    BookmarkSet {
        name: BookmarkName,
        change_id: ChangeId,
        flags: CommandFlags,
    },
    BookmarkDelete {
        names: SmallVec<BookmarkName>,
        flags: CommandFlags,
    },
    BookmarkForget {
        names: SmallVec<BookmarkName>,
        flags: CommandFlags,
    },
    BookmarkMove {
        name: BookmarkName,
        target: ChangeId,
        flags: CommandFlags,
    },
    BookmarkRename {
        old_name: BookmarkName,
        new_name: BookmarkName,
        flags: CommandFlags,
    },
    BookmarkAdvance {
        change_id: Option<ChangeId>,
        flags: CommandFlags,
    },
    BookmarkTrack {
        bookmarks: SmallVec<BookmarkRef>,
        flags: CommandFlags,
    },
    BookmarkUntrack {
        bookmarks: SmallVec<BookmarkRef>,
        flags: CommandFlags,
    },
    Undo {
        flags: CommandFlags,
    },
    Redo {
        flags: CommandFlags,
    },
    GitFetch {
        all_remotes: bool,
        remote: Option<Str>,
        flags: CommandFlags,
    },
    GitPush {
        all: bool,
        remote: Option<Str>,
        flags: CommandFlags,
    },
    GitPushChange {
        change_id: ChangeId,
        remote: Option<Str>,
        flags: CommandFlags,
    },
    GitPushBookmark {
        bookmarks: SmallVec<BookmarkName>,
        remote: Option<Str>,
        flags: CommandFlags,
    },
    GitExport {
        flags: CommandFlags,
    },
    GitImport {
        flags: CommandFlags,
    },
    Absorb {
        /// Source revision. `None` = default (absorb from @).
        from: Option<ChangeId>,
        /// How to filter changes. Absorb supports all changes or file-level selection.
        selection: ChangeSelection,
        flags: CommandFlags,
    },
    Commit {
        /// Inline message. `None` = open $EDITOR.
        message: Option<String>,
        /// How to filter changes.
        selection: ChangeSelection,
        flags: CommandFlags,
    },
    Duplicate {
        change_ids: SmallVec<ChangeId>,
        /// Target revision for `--onto`. `None` = duplicate onto same parents.
        onto: Option<ChangeId>,
        flags: CommandFlags,
    },
    Parallelize {
        change_ids: SmallVec<ChangeId>,
        flags: CommandFlags,
    },
    SimplifyParents {
        change_ids: SmallVec<ChangeId>,
        flags: CommandFlags,
    },
    Revert {
        change_ids: SmallVec<ChangeId>,
        flags: CommandFlags,
    },
    WorkspaceAdd {
        path: String,
        name: Option<String>,
        revision: ChangeId,
        flags: CommandFlags,
    },
    WorkspaceForget {
        names: SmallVec<String>,
        flags: CommandFlags,
    },
    WorkspaceList {
        flags: CommandFlags,
    },
    TagSet {
        name: String,
        change_id: ChangeId,
        flags: CommandFlags,
    },
    TagDelete {
        names: SmallVec<String>,
        flags: CommandFlags,
    },
    TagList {
        flags: CommandFlags,
    },
}

/// The result of running a jj command.
pub struct JJCommandResult {
    /// The command that was displayed to the user.
    pub display: String,
    /// Raw stdout+stderr bytes (with ANSI color codes).
    pub output: Vec<u8>,
    /// Whether the command exited successfully.
    pub success: bool,
}

impl JJCommand {
    /// Extract the `CommandFlags` from any variant.
    fn flags(&self) -> CommandFlags {
        match self {
            JJCommand::Abandon { flags, .. }
            | JJCommand::Describe { flags, .. }
            | JJCommand::DescribeInEditor { flags, .. }
            | JJCommand::Edit { flags, .. }
            | JJCommand::New { flags, .. }
            | JJCommand::Rebase { flags, .. }
            | JJCommand::Restore { flags, .. }
            | JJCommand::Split { flags, .. }
            | JJCommand::BookmarkCreate { flags, .. }
            | JJCommand::BookmarkSet { flags, .. }
            | JJCommand::BookmarkDelete { flags, .. }
            | JJCommand::BookmarkForget { flags, .. }
            | JJCommand::BookmarkMove { flags, .. }
            | JJCommand::BookmarkRename { flags, .. }
            | JJCommand::BookmarkAdvance { flags, .. }
            | JJCommand::BookmarkTrack { flags, .. }
            | JJCommand::BookmarkUntrack { flags, .. }
            | JJCommand::Undo { flags, .. }
            | JJCommand::Redo { flags, .. }
            | JJCommand::GitFetch { flags, .. }
            | JJCommand::GitPush { flags, .. }
            | JJCommand::GitPushChange { flags, .. }
            | JJCommand::GitPushBookmark { flags, .. }
            | JJCommand::GitExport { flags, .. }
            | JJCommand::GitImport { flags, .. }
            | JJCommand::Absorb { flags, .. }
            | JJCommand::Commit { flags, .. }
            | JJCommand::Duplicate { flags, .. }
            | JJCommand::Parallelize { flags, .. }
            | JJCommand::SimplifyParents { flags, .. }
            | JJCommand::Revert { flags, .. }
            | JJCommand::Squash { flags, .. }
            | JJCommand::WorkspaceAdd { flags, .. }
            | JJCommand::WorkspaceForget { flags, .. }
            | JJCommand::WorkspaceList { flags, .. }
            | JJCommand::TagSet { flags, .. }
            | JJCommand::TagDelete { flags, .. }
            | JJCommand::TagList { flags, .. } => *flags,
        }
    }

    /// Where to jump the cursor after this command succeeds.
    pub fn jump_target(&self) -> Option<JumpTarget> {
        match self {
            // Commands that move @.
            JJCommand::New { .. }
            | JJCommand::Edit { .. }
            | JJCommand::Commit { .. }
            | JJCommand::Squash { .. }
            | JJCommand::Abandon { .. }
            | JJCommand::Absorb { .. }
            | JJCommand::Split { .. }
            | JJCommand::Parallelize { .. }
            | JJCommand::SimplifyParents { .. }
            | JJCommand::Revert { .. } => Some(JumpTarget::WorkingCopy),
            // Track: jump to the first tracked bookmark's commit.
            JJCommand::BookmarkTrack { bookmarks, .. } => bookmarks
                .first()
                .map(|br| JumpTarget::Bookmark(br.name.clone())),
            // Everything else: use normal ChangeId restoration.
            _ => None,
        }
    }

    /// Build the CLI arguments for `jj`.
    pub fn args(&self) -> Vec<Str> {
        let flags = self.flags();
        let mut args = match self {
            JJCommand::Abandon { change_ids, .. } => {
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
            JJCommand::Describe {
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
            JJCommand::DescribeInEditor { change_id, .. } => {
                vec!["describe".into(), format_compact!("{change_id}")]
            }
            JJCommand::Edit { change_id, .. } => {
                vec!["edit".into(), format_compact!("{change_id}")]
            }
            JJCommand::New {
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
            JJCommand::Rebase {
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
                args.push(format_compact!("{}", dest.kind.flag()));
                args.push(format_compact!("{}", dest.target));
                args
            }
            JJCommand::Restore {
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
            JJCommand::Split {
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
            JJCommand::BookmarkCreate {
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
            JJCommand::BookmarkSet {
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
            JJCommand::BookmarkDelete { names, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "delete".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommand::BookmarkForget { names, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "forget".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommand::BookmarkMove { name, target, .. } => {
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
            JJCommand::BookmarkRename {
                old_name, new_name, ..
            } => {
                vec![
                    "bookmark".into(),
                    "rename".into(),
                    Str::from(old_name.as_str()),
                    Str::from(new_name.as_str()),
                ]
            }
            JJCommand::BookmarkAdvance { change_id, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "advance".into()];
                if let Some(id) = change_id {
                    args.push("--to".into());
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommand::BookmarkTrack { bookmarks, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "track".into()];
                for br in bookmarks {
                    args.push(Str::from(br.name.as_str()));
                    args.push("--remote".into());
                    args.push(Str::from(br.remote.as_str()));
                }
                args
            }
            JJCommand::BookmarkUntrack { bookmarks, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "untrack".into()];
                for br in bookmarks {
                    args.push(Str::from(br.name.as_str()));
                    args.push("--remote".into());
                    args.push(Str::from(br.remote.as_str()));
                }
                args
            }
            JJCommand::Undo { .. } => vec!["undo".into()],
            JJCommand::Redo { .. } => vec!["redo".into()],
            JJCommand::GitFetch {
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
                    args.push(r.clone());
                }
                args
            }
            JJCommand::GitPush { all, remote, .. } => {
                let mut args: Vec<Str> = vec!["git".into(), "push".into()];
                if *all {
                    args.push("--all".into());
                }
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(r.clone());
                }
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::DRY_RUN, "--dry-run"),
                        (CommandFlags::ALLOW_NEW, "--allow-new"),
                    ],
                );
                args
            }
            JJCommand::GitPushChange {
                change_id,
                remote,
                ..
            } => {
                let mut args: Vec<Str> = vec!["git".into(), "push".into()];
                args.push("-c".into());
                args.push(format_compact!("{change_id}"));
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(r.clone());
                }
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::DRY_RUN, "--dry-run"),
                        (CommandFlags::ALLOW_NEW, "--allow-new"),
                    ],
                );
                args
            }
            JJCommand::GitPushBookmark {
                bookmarks,
                remote,
                ..
            } => {
                let mut args: Vec<Str> = vec!["git".into(), "push".into()];
                for name in bookmarks {
                    args.push("--bookmark".into());
                    args.push(Str::from(name.as_str()));
                }
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(r.clone());
                }
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::DRY_RUN, "--dry-run"),
                        (CommandFlags::ALLOW_NEW, "--allow-new"),
                    ],
                );
                args
            }
            JJCommand::GitExport { .. } => vec!["git".into(), "export".into()],
            JJCommand::GitImport { .. } => vec!["git".into(), "import".into()],
            JJCommand::Absorb {
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
            JJCommand::Commit {
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
            JJCommand::Parallelize { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["parallelize".into()];
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommand::SimplifyParents { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["simplify-parents".into()];
                for id in change_ids {
                    args.push("-r".into());
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommand::Revert { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["revert".into()];
                for id in change_ids {
                    args.push("-r".into());
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommand::Duplicate {
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
            JJCommand::Squash {
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
            JJCommand::WorkspaceAdd {
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
            JJCommand::WorkspaceForget { names, .. } => {
                let mut args: Vec<Str> = vec!["workspace".into(), "forget".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommand::WorkspaceList { .. } => {
                vec!["workspace".into(), "list".into()]
            }
            JJCommand::TagSet {
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
            JJCommand::TagDelete { names, .. } => {
                let mut args: Vec<Str> = vec!["tag".into(), "delete".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommand::TagList { .. } => {
                vec!["tag".into(), "list".into()]
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
        let args = self.args();
        let joined = shlex::try_join(args.iter().map(|s| s.as_str()))
            .expect("jj arguments should not contain NUL bytes");
        format!("$ jj {joined}")
    }

    /// Whether this command needs an interactive terminal (editor/diff tool).
    pub fn is_interactive(&self) -> bool {
        match self {
            JJCommand::DescribeInEditor { .. } => true,
            JJCommand::Squash {
                message,
                selection,
                flags,
                ..
            } => {
                flags.contains(CommandFlags::INTERACTIVE)
                    || matches!(message, MessageMode::Default)
                    || matches!(selection, ChangeSelection::Lines(_))
            }
            JJCommand::Commit {
                message,
                selection,
                flags,
                ..
            } => {
                message.is_none()
                    || flags.contains(CommandFlags::INTERACTIVE)
                    || matches!(selection, ChangeSelection::Lines(_))
            }
            JJCommand::Restore {
                selection, flags, ..
            } => {
                flags.contains(CommandFlags::INTERACTIVE)
                    || matches!(selection, ChangeSelection::Lines(_))
            }
            JJCommand::Split { .. } => true,
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
                    output: b"interrupted".to_vec(),
                    success: false,
                }
            }
            Ok(Output { stderr, status, .. }) => JJCommandResult {
                display,
                output: stderr,
                success: status.success(),
            },
            Err(e) => jj_error(display, e),
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

        let (result, was_interrupted) = with_sigint_suppressed(|| {
            Command::new("jj")
                .args(&args)
                .arg("-R")
                .arg(repo_path)
                .arg("--color=always")
                .current_dir(repo_path)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
        });

        match result {
            Ok(Output { status, .. }) if was_interrupted || status.code().is_none() => {
                JJCommandResult {
                    display,
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
                output: merge_captured_output(stdout, stderr),
                success: status.success(),
            },
            Err(e) => jj_error(display, e),
        }
    }

    /// Execute the command against a repository path (captured output).
    pub fn run(&self, repo_path: &Path) -> JJCommandResult {
        let args = self.args();
        let display = self.display();

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
                output: merge_captured_output(stdout, stderr),
                success: status.success(),
            },
            Err(e) => jj_error(display, e),
        }
    }
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
fn jj_error(display: String, err: std::io::Error) -> JJCommandResult {
    JJCommandResult {
        display,
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
