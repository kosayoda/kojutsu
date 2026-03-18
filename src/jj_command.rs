use std::path::Path;
use std::process::{Command, Output};

use crate::app::{MessageMode, RebaseDestMode, RebaseSourceMode, SquashTarget};
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
    /// Build the CLI arguments for `jj`.
    pub fn args(&self) -> Vec<String> {
        match self {
            JJCommand::Abandon { change_id, flags } => {
                let mut args = vec!["abandon".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[
                        (CommandFlags::RETAIN_BOOKMARKS, "--retain-bookmarks"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                        (CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable"),
                    ],
                );
                args.push(change_id.clone());
                args
            }
            JJCommand::Describe {
                change_id,
                message,
                flags,
            } => {
                let mut args = vec!["describe".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args.push("-m".to_string());
                args.push(message.clone());
                args.push(change_id.clone());
                args
            }
            JJCommand::DescribeInEditor { change_id, flags } => {
                let mut args = vec!["describe".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args.push(change_id.clone());
                args
            }
            JJCommand::Edit { change_id, flags } => {
                let mut args = vec!["edit".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args.push(change_id.clone());
                args
            }
            JJCommand::New {
                change_id,
                insert_after,
                insert_before,
                flags,
            } => {
                let mut args = vec!["new".to_string()];
                if *insert_after {
                    args.push("--insert-after".to_string());
                }
                if *insert_before {
                    args.push("--insert-before".to_string());
                }
                args.push(change_id.clone());
                push_flags(
                    &mut args,
                    *flags,
                    &[
                        (CommandFlags::NO_EDIT, "--no-edit"),
                        (CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable"),
                    ],
                );
                args
            }
            JJCommand::Rebase {
                change_id,
                source_mode,
                dest,
                flags,
            } => {
                let mut args = vec!["rebase".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                // Source mode.
                match source_mode {
                    RebaseSourceMode::Revision => {
                        args.push("-r".to_string());
                    }
                    RebaseSourceMode::Source => {
                        args.push("-s".to_string());
                    }
                    RebaseSourceMode::Branch => {
                        args.push("-b".to_string());
                    }
                }
                args.push(change_id.clone());
                // Destination mode.
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
                name,
                change_id,
                flags,
            } => {
                let mut args = vec!["bookmark".to_string(), "create".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args.push("-r".to_string());
                args.push(change_id.clone());
                args.push(name.clone());
                args
            }
            JJCommand::BookmarkSet {
                name,
                change_id,
                flags,
            } => {
                let mut args = vec!["bookmark".to_string(), "set".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[
                        (CommandFlags::ALLOW_BACKWARDS, "--allow-backwards"),
                        (CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable"),
                    ],
                );
                args.push("-r".to_string());
                args.push(change_id.clone());
                args.push(name.clone());
                args
            }
            JJCommand::BookmarkDelete { name, flags } => {
                let mut args = vec!["bookmark".to_string(), "delete".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args.push(name.clone());
                args
            }
            JJCommand::BookmarkForget { name, flags } => {
                let mut args = vec!["bookmark".to_string(), "forget".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args.push(name.clone());
                args
            }
            JJCommand::BookmarkMove {
                name,
                target,
                flags,
            } => {
                let mut args = vec!["bookmark".to_string(), "move".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[
                        (CommandFlags::ALLOW_BACKWARDS, "--allow-backwards"),
                        (CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable"),
                    ],
                );
                args.push("--to".to_string());
                args.push(target.clone());
                args.push(name.clone());
                args
            }
            JJCommand::BookmarkRename {
                old_name,
                new_name,
                flags,
            } => {
                let mut args = vec!["bookmark".to_string(), "rename".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args.push(old_name.clone());
                args.push(new_name.clone());
                args
            }
            JJCommand::BookmarkAdvance { change_id, flags } => {
                let mut args = vec!["bookmark".to_string(), "advance".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                if let Some(id) = change_id {
                    args.push("--to".to_string());
                    args.push(id.clone());
                }
                args
            }
            JJCommand::BookmarkTrack { name, flags } => {
                let mut args = vec!["bookmark".to_string(), "track".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args.push(name.clone());
                args
            }
            JJCommand::BookmarkUntrack { name, flags } => {
                let mut args = vec!["bookmark".to_string(), "untrack".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args.push(name.clone());
                args
            }
            JJCommand::Undo { flags } => {
                let mut args = vec!["undo".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args
            }
            JJCommand::Redo { flags } => {
                let mut args = vec!["redo".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[(CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable")],
                );
                args
            }
            JJCommand::Squash {
                change_id,
                target,
                message,
                flags,
            } => {
                let mut args = vec!["squash".to_string()];
                push_flags(
                    &mut args,
                    *flags,
                    &[
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::KEEP_EMPTIED, "--keep-emptied"),
                        (CommandFlags::IGNORE_IMMUTABLE, "--ignore-immutable"),
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
                        // Squash into parent.
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
        }
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
            JJCommand::Squash { flags, .. } => flags.contains(CommandFlags::INTERACTIVE),
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

        let result = Command::new("jj")
            .args(&args)
            .arg("-R")
            .arg(repo_path)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status();

        match result {
            Ok(status) => JJCommandResult {
                display,
                output: Vec::new(),
                success: status.success(),
            },
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

/// Push CLI flag arguments for any active flags in the bitset.
fn push_flags(args: &mut Vec<String>, flags: CommandFlags, mapping: &[(CommandFlags, &str)]) {
    for (flag, arg) in mapping {
        if flags.contains(*flag) {
            args.push(arg.to_string());
        }
    }
}
