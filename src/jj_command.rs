use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::app::GLOBAL_TOGGLES;
use crate::keymap::CommandFlags;
use crate::types::{ChangeId, MessageMode, RebaseSource, RebaseTarget, SplitTarget, SquashTarget};

/// How to filter changes for squash/commit operations.
#[derive(Debug, Clone)]
pub enum ChangeSelection {
    /// Include all changes (no filtering).
    All,
    /// Include only these files (maps to [FILESETS] positional args).
    Files(Vec<String>),
    /// Line-level selection (maps to --interactive --tool with selection JSON).
    Lines(PathBuf),
}

/// A typesafe representation of a jj CLI command.
#[derive(Debug, Clone)]
pub enum JJCommand {
    Abandon {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Describe with an inline message (non-interactive).
    Describe {
        change_id: ChangeId,
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
        change_id: ChangeId,
        insert_after: bool,
        insert_before: bool,
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
        change_id: ChangeId,
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
        name: String,
        change_id: ChangeId,
        flags: CommandFlags,
    },
    BookmarkSet {
        name: String,
        change_id: ChangeId,
        flags: CommandFlags,
    },
    BookmarkDelete {
        names: Vec<String>,
        flags: CommandFlags,
    },
    BookmarkForget {
        names: Vec<String>,
        flags: CommandFlags,
    },
    BookmarkMove {
        name: String,
        target: ChangeId,
        flags: CommandFlags,
    },
    BookmarkRename {
        old_name: String,
        new_name: String,
        flags: CommandFlags,
    },
    BookmarkAdvance {
        change_id: Option<ChangeId>,
        flags: CommandFlags,
    },
    BookmarkTrack {
        /// Each entry is `(bookmark_name, remote_name)`.
        bookmarks: Vec<(String, String)>,
        flags: CommandFlags,
    },
    BookmarkUntrack {
        /// Each entry is `(bookmark_name, remote_name)`.
        bookmarks: Vec<(String, String)>,
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
        flags: CommandFlags,
    },
    GitPush {
        all: bool,
        flags: CommandFlags,
    },
    GitPushChange {
        change_id: ChangeId,
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
        change_id: ChangeId,
        /// Target revision for `--onto`. `None` = duplicate onto same parents.
        onto: Option<ChangeId>,
        flags: CommandFlags,
    },
    WorkspaceAdd {
        path: String,
        name: Option<String>,
        revision: ChangeId,
        flags: CommandFlags,
    },
    WorkspaceForget {
        names: Vec<String>,
        flags: CommandFlags,
    },
    WorkspaceList {
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
            | JJCommand::GitExport { flags, .. }
            | JJCommand::GitImport { flags, .. }
            | JJCommand::Absorb { flags, .. }
            | JJCommand::Commit { flags, .. }
            | JJCommand::Duplicate { flags, .. }
            | JJCommand::Squash { flags, .. }
            | JJCommand::WorkspaceAdd { flags, .. }
            | JJCommand::WorkspaceForget { flags, .. }
            | JJCommand::WorkspaceList { flags, .. } => *flags,
        }
    }

    /// Build the CLI arguments for `jj`.
    pub fn args(&self) -> Vec<String> {
        let flags = self.flags();
        let mut args = match self {
            JJCommand::Abandon { change_id, .. } => {
                let mut args = vec!["abandon".to_string()];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::RETAIN_BOOKMARKS, "--retain-bookmarks"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                    ],
                );
                args.push(change_id.to_string());
                args
            }
            JJCommand::Describe {
                change_id, message, ..
            } => {
                let mut args = vec!["describe".to_string()];
                args.push("-m".to_string());
                args.push(message.clone());
                args.push(change_id.to_string());
                args
            }
            JJCommand::DescribeInEditor { change_id, .. } => {
                vec!["describe".to_string(), change_id.to_string()]
            }
            JJCommand::Edit { change_id, .. } => {
                vec!["edit".to_string(), change_id.to_string()]
            }
            JJCommand::New {
                change_id,
                insert_after,
                insert_before,
                ..
            } => {
                let mut args = vec!["new".to_string()];
                if *insert_after {
                    args.push("--insert-after".to_string());
                }
                if *insert_before {
                    args.push("--insert-before".to_string());
                }
                args.push(change_id.to_string());
                push_flags(&mut args, flags, &[(CommandFlags::NO_EDIT, "--no-edit")]);
                args
            }
            JJCommand::Rebase {
                change_id,
                source_mode,
                dest,
                ..
            } => {
                let mut args = vec!["rebase".to_string()];
                match source_mode {
                    RebaseSource::Revision => args.push("-r".to_string()),
                    RebaseSource::Source => args.push("-s".to_string()),
                    RebaseSource::Branch => args.push("-b".to_string()),
                }
                args.push(change_id.to_string());
                args.push(dest.kind.flag().to_string());
                args.push(dest.target.to_string());
                args
            }
            JJCommand::Restore {
                from,
                into,
                changes_in,
                selection,
                ..
            } => {
                let mut args = vec!["restore".to_string()];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                    ],
                );
                if let Some(id) = from {
                    args.push("--from".to_string());
                    args.push(id.to_string());
                }
                if let Some(id) = into {
                    args.push("--into".to_string());
                    args.push(id.to_string());
                }
                if let Some(id) = changes_in {
                    args.push("--changes-in".to_string());
                    args.push(id.to_string());
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
                let mut args = vec!["split".to_string(), "-r".to_string(), change_id.to_string()];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::PARALLEL, "--parallel"),
                    ],
                );
                if let Some(t) = target {
                    args.push(t.kind.flag().to_string());
                    args.push(t.target.to_string());
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommand::BookmarkCreate {
                name, change_id, ..
            } => {
                vec![
                    "bookmark".to_string(),
                    "create".to_string(),
                    "-r".to_string(),
                    change_id.to_string(),
                    name.clone(),
                ]
            }
            JJCommand::BookmarkSet {
                name, change_id, ..
            } => {
                let mut args = vec!["bookmark".to_string(), "set".to_string()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push("-r".to_string());
                args.push(change_id.to_string());
                args.push(name.clone());
                args
            }
            JJCommand::BookmarkDelete { names, .. } => {
                let mut args = vec!["bookmark".to_string(), "delete".to_string()];
                args.extend(names.iter().cloned());
                args
            }
            JJCommand::BookmarkForget { names, .. } => {
                let mut args = vec!["bookmark".to_string(), "forget".to_string()];
                args.extend(names.iter().cloned());
                args
            }
            JJCommand::BookmarkMove { name, target, .. } => {
                let mut args = vec!["bookmark".to_string(), "move".to_string()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push("--to".to_string());
                args.push(target.to_string());
                args.push(name.to_string());
                args
            }
            JJCommand::BookmarkRename {
                old_name, new_name, ..
            } => {
                vec![
                    "bookmark".to_string(),
                    "rename".to_string(),
                    old_name.clone(),
                    new_name.clone(),
                ]
            }
            JJCommand::BookmarkAdvance { change_id, .. } => {
                let mut args = vec!["bookmark".to_string(), "advance".to_string()];
                if let Some(id) = change_id {
                    args.push("--to".to_string());
                    args.push(id.to_string());
                }
                args
            }
            JJCommand::BookmarkTrack { bookmarks, .. } => {
                let mut args = vec!["bookmark".to_string(), "track".to_string()];
                for (name, remote) in bookmarks {
                    args.push(name.clone());
                    args.push("--remote".to_string());
                    args.push(remote.clone());
                }
                args
            }
            JJCommand::BookmarkUntrack { bookmarks, .. } => {
                let mut args = vec!["bookmark".to_string(), "untrack".to_string()];
                for (name, remote) in bookmarks {
                    args.push(name.clone());
                    args.push("--remote".to_string());
                    args.push(remote.clone());
                }
                args
            }
            JJCommand::Undo { .. } => vec!["undo".to_string()],
            JJCommand::Redo { .. } => vec!["redo".to_string()],
            JJCommand::GitFetch { all_remotes, .. } => {
                let mut args = vec!["git".to_string(), "fetch".to_string()];
                if *all_remotes {
                    args.push("--all-remotes".to_string());
                }
                args
            }
            JJCommand::GitPush { all, .. } => {
                let mut args = vec!["git".to_string(), "push".to_string()];
                if *all {
                    args.push("--all".to_string());
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommand::GitPushChange { change_id, .. } => {
                let mut args = vec!["git".to_string(), "push".to_string()];
                args.push("-c".to_string());
                args.push(change_id.to_string());
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommand::GitExport { .. } => vec!["git".to_string(), "export".to_string()],
            JJCommand::GitImport { .. } => vec!["git".to_string(), "import".to_string()],
            JJCommand::Absorb {
                from, selection, ..
            } => {
                let mut args = vec!["absorb".to_string()];
                if let Some(id) = from {
                    args.push("--from".to_string());
                    args.push(id.to_string());
                }
                match selection {
                    ChangeSelection::All => {}
                    ChangeSelection::Files(paths) => args.extend(paths.iter().cloned()),
                    ChangeSelection::Lines(_) => {
                        debug_assert!(false, "line selection should be blocked for absorb");
                    }
                }
                args
            }
            JJCommand::Commit {
                message, selection, ..
            } => {
                let mut args = vec!["commit".to_string()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::INTERACTIVE, "--interactive")],
                );
                if let Some(msg) = message {
                    args.push("-m".to_string());
                    args.push(msg.clone());
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommand::Duplicate {
                change_id, onto, ..
            } => {
                let mut args = vec!["duplicate".to_string(), change_id.to_string()];
                if let Some(target) = onto {
                    args.push("--onto".to_string());
                    args.push(target.to_string());
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
                let mut args = vec!["squash".to_string()];
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
                        args.push("-m".to_string());
                        args.push(msg.clone());
                    }
                    MessageMode::UseDestination => {
                        args.push("--use-destination-message".to_string());
                    }
                }
                match target {
                    None => {
                        args.push("-r".to_string());
                        args.push(change_id.to_string());
                    }
                    Some(t) => {
                        args.push("--from".to_string());
                        args.push(change_id.to_string());
                        args.push(t.kind.flag().to_string());
                        args.push(t.target.to_string());
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
                let mut args = vec!["workspace".to_string(), "add".to_string()];
                if let Some(n) = name {
                    args.push("--name".to_string());
                    args.push(n.clone());
                }
                args.push("-r".to_string());
                args.push(revision.to_string());
                args.push(path.clone());
                args
            }
            JJCommand::WorkspaceForget { names, .. } => {
                let mut args = vec!["workspace".to_string(), "forget".to_string()];
                args.extend(names.iter().cloned());
                args
            }
            JJCommand::WorkspaceList { .. } => {
                vec!["workspace".to_string(), "list".to_string()]
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
        let quoted: Vec<String> = args
            .iter()
            .map(|a| {
                if a.contains(|c: char| c.is_whitespace() || "\"'\\$`!#&|;(){}".contains(c))
                    || a.is_empty()
                {
                    format!("{:?}", a)
                } else {
                    a.clone()
                }
            })
            .collect();
        format!("$ jj {}", quoted.join(" "))
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

        // Ignore SIGINT in the parent while the child runs. Without this,
        // Ctrl-C during an SSH password prompt (or editor) would kill both
        // the child and our process. The child still receives SIGINT normally
        // since it has its own signal disposition after exec.
        //
        // We use signal_hook::flag to set an AtomicBool on SIGINT instead of
        // the default terminate-the-process behavior.
        let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sigint_hook =
            signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone()).ok();

        let result = Command::new("jj")
            .args(&args)
            .arg("-R")
            .arg(repo_path)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status();

        // Restore default SIGINT handling.
        if let Some(id) = sigint_hook {
            signal_hook::low_level::unregister(id);
        }
        let was_interrupted = interrupted.load(std::sync::atomic::Ordering::Relaxed);

        match result {
            Ok(status) => {
                let output = if was_interrupted || status.code().is_none() {
                    b"interrupted".to_vec()
                } else {
                    Vec::new()
                };
                JJCommandResult {
                    display,
                    output,
                    success: status.success() && !was_interrupted,
                }
            }
            Err(e) => JJCommandResult {
                display,
                output: format!("failed to run jj: {e}").into_bytes(),
                success: false,
            },
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
            // Safety: use a no-op editor so that if jj unexpectedly opens
            // an editor in captured mode, it won't hang waiting for input.
            .env("JJ_EDITOR", ":")
            .output();

        match result {
            Ok(Output {
                stdout,
                stderr,
                status,
            }) => {
                let mut output = stdout;
                if !stderr.is_empty() {
                    if !output.is_empty() && !output.ends_with(b"\n") {
                        output.push(b'\n');
                    }
                    output.extend_from_slice(&stderr);
                }
                JJCommandResult {
                    display,
                    output,
                    success: status.success(),
                }
            }
            Err(e) => JJCommandResult {
                display,
                output: format!("failed to run jj: {e}").into_bytes(),
                success: false,
            },
        }
    }
}

/// Push CLI flag arguments for any active command-specific flags.
fn push_flags(args: &mut Vec<String>, flags: CommandFlags, mapping: &[(CommandFlags, &str)]) {
    for (flag, arg) in mapping {
        if flags.contains(*flag) {
            args.push(arg.to_string());
        }
    }
}

/// Push args for change selection (file paths or --tool for line-level).
fn push_change_selection(args: &mut Vec<String>, selection: &ChangeSelection) {
    match selection {
        ChangeSelection::All => {}
        ChangeSelection::Files(paths) => {
            args.extend(paths.iter().cloned());
        }
        ChangeSelection::Lines(json_path) => {
            let exe = std::env::current_exe().unwrap_or_else(|_| "kojutsu".into());
            args.extend([
                "--interactive".to_string(),
                "--tool".to_string(),
                "kojutsu-select".to_string(),
                "--config".to_string(),
                format!(
                    "merge-tools.kojutsu-select.program={}",
                    shell_escape(&exe.display().to_string())
                ),
                "--config".to_string(),
                format!(
                    "merge-tools.kojutsu-select.edit-args=[\"--apply-diff\", \"{}\", \"$left\", \"$right\"]",
                    json_path.display()
                ),
            ]);
        }
    }
}

/// Escape a string for use in jj --config values.
fn shell_escape(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Push CLI flags for all active global toggles (ignore-immutable, etc.).
/// Called once at the end of `args()` so individual variants don't need to.
fn push_global_flags(args: &mut Vec<String>, flags: CommandFlags) {
    for toggle in GLOBAL_TOGGLES {
        if flags.contains(toggle.flag) {
            args.push(toggle.cli_flag.to_string());
        }
    }
}
