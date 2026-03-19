use std::path::Path;
use std::process::{Command, Output};

use crate::app::{MessageMode, RebaseDestMode, RebaseSourceMode, SquashTarget, GLOBAL_TOGGLES};
use crate::keymap::CommandFlags;

/// A typesafe representation of a jj CLI command.
#[derive(Debug, Clone)]
pub enum JJCommand {
    Abandon {
        change_id: String,
        flags: CommandFlags,
    },
    /// Describe with an inline message (non-interactive).
    Describe {
        change_id: String,
        message: String,
        flags: CommandFlags,
    },
    /// Describe via jj's configured editor (interactive -- needs terminal).
    DescribeInEditor {
        change_id: String,
        flags: CommandFlags,
    },
    Edit {
        change_id: String,
        flags: CommandFlags,
    },
    New {
        change_id: String,
        insert_after: bool,
        insert_before: bool,
        flags: CommandFlags,
    },
    Squash {
        change_id: String,
        target: Option<SquashTarget>,
        message: MessageMode,
        flags: CommandFlags,
    },
    Rebase {
        change_id: String,
        source_mode: RebaseSourceMode,
        dest: RebaseDestMode,
        flags: CommandFlags,
    },
    BookmarkCreate {
        name: String,
        change_id: String,
        flags: CommandFlags,
    },
    BookmarkSet {
        name: String,
        change_id: String,
        flags: CommandFlags,
    },
    BookmarkDelete {
        name: String,
        flags: CommandFlags,
    },
    BookmarkForget {
        name: String,
        flags: CommandFlags,
    },
    BookmarkMove {
        name: String,
        target: String,
        flags: CommandFlags,
    },
    BookmarkRename {
        old_name: String,
        new_name: String,
        flags: CommandFlags,
    },
    BookmarkAdvance {
        change_id: Option<String>,
        flags: CommandFlags,
    },
    BookmarkTrack {
        name: String,
        flags: CommandFlags,
    },
    BookmarkUntrack {
        name: String,
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
        change_id: String,
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
        from: Option<String>,
        flags: CommandFlags,
    },
    Commit {
        /// Inline message. `None` = open $EDITOR.
        message: Option<String>,
        flags: CommandFlags,
    },
    Duplicate {
        change_id: String,
        /// Target revision for `--onto`. `None` = duplicate onto same parents.
        onto: Option<String>,
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
            | JJCommand::Squash { flags, .. } => *flags,
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
                args.push(change_id.clone());
                args
            }
            JJCommand::Describe {
                change_id, message, ..
            } => {
                let mut args = vec!["describe".to_string()];
                args.push("-m".to_string());
                args.push(message.clone());
                args.push(change_id.clone());
                args
            }
            JJCommand::DescribeInEditor { change_id, .. } => {
                vec!["describe".to_string(), change_id.clone()]
            }
            JJCommand::Edit { change_id, .. } => {
                vec!["edit".to_string(), change_id.clone()]
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
                args.push(change_id.clone());
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
                    RebaseSourceMode::Revision => args.push("-r".to_string()),
                    RebaseSourceMode::Source => args.push("-s".to_string()),
                    RebaseSourceMode::Branch => args.push("-b".to_string()),
                }
                args.push(change_id.clone());
                match dest {
                    RebaseDestMode::Onto(t) => {
                        args.push("-d".to_string());
                        args.push(t.clone());
                    }
                    RebaseDestMode::After(t) => {
                        args.push("-A".to_string());
                        args.push(t.clone());
                    }
                    RebaseDestMode::Before(t) => {
                        args.push("-B".to_string());
                        args.push(t.clone());
                    }
                }
                args
            }
            JJCommand::BookmarkCreate {
                name, change_id, ..
            } => {
                vec![
                    "bookmark".to_string(),
                    "create".to_string(),
                    "-r".to_string(),
                    change_id.clone(),
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
                args.push(change_id.clone());
                args.push(name.clone());
                args
            }
            JJCommand::BookmarkDelete { name, .. } => {
                vec!["bookmark".to_string(), "delete".to_string(), name.clone()]
            }
            JJCommand::BookmarkForget { name, .. } => {
                vec!["bookmark".to_string(), "forget".to_string(), name.clone()]
            }
            JJCommand::BookmarkMove { name, target, .. } => {
                let mut args = vec!["bookmark".to_string(), "move".to_string()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push("--to".to_string());
                args.push(target.clone());
                args.push(name.clone());
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
                    args.push(id.clone());
                }
                args
            }
            JJCommand::BookmarkTrack { name, .. } => {
                vec!["bookmark".to_string(), "track".to_string(), name.clone()]
            }
            JJCommand::BookmarkUntrack { name, .. } => {
                vec!["bookmark".to_string(), "untrack".to_string(), name.clone()]
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
                args.push(change_id.clone());
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommand::GitExport { .. } => vec!["git".to_string(), "export".to_string()],
            JJCommand::GitImport { .. } => vec!["git".to_string(), "import".to_string()],
            JJCommand::Absorb { from, .. } => {
                let mut args = vec!["absorb".to_string()];
                if let Some(id) = from {
                    args.push("--from".to_string());
                    args.push(id.clone());
                }
                args
            }
            JJCommand::Commit { message, .. } => {
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
                args
            }
            JJCommand::Duplicate {
                change_id, onto, ..
            } => {
                let mut args = vec!["duplicate".to_string(), change_id.clone()];
                if let Some(target) = onto {
                    args.push("--onto".to_string());
                    args.push(target.clone());
                }
                args
            }
            JJCommand::Squash {
                change_id,
                target,
                message,
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
                        args.push(change_id.clone());
                    }
                    Some(t) => {
                        args.push("--from".to_string());
                        args.push(change_id.clone());
                        match t {
                            SquashTarget::Into(id) => {
                                args.push("--into".to_string());
                                args.push(id.clone());
                            }
                            SquashTarget::Onto(id) => {
                                args.push("--onto".to_string());
                                args.push(id.clone());
                            }
                            SquashTarget::After(id) => {
                                args.push("--insert-after".to_string());
                                args.push(id.clone());
                            }
                            SquashTarget::Before(id) => {
                                args.push("--insert-before".to_string());
                                args.push(id.clone());
                            }
                        }
                    }
                }
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
            JJCommand::Squash { message, flags, .. } => {
                flags.contains(CommandFlags::INTERACTIVE) || matches!(message, MessageMode::Default)
            }
            JJCommand::Commit { message, flags } => {
                message.is_none() || flags.contains(CommandFlags::INTERACTIVE)
            }
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

/// Push CLI flags for all active global toggles (ignore-immutable, etc.).
/// Called once at the end of `args()` so individual variants don't need to.
fn push_global_flags(args: &mut Vec<String>, flags: CommandFlags) {
    for toggle in GLOBAL_TOGGLES {
        if flags.contains(toggle.flag) {
            args.push(toggle.cli_flag.to_string());
        }
    }
}
