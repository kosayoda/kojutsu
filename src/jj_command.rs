use std::path::Path;
use std::process::{Command, Output};

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
                if a.contains(|c: char| c.is_whitespace() || "\"'\\$`!#&|;(){}".contains(c)) {
                    format!("{:?}", a)
                } else {
                    a.clone()
                }
            })
            .collect();
        format!("$ jj {}", quoted.join(" "))
    }

    /// Whether this command needs an interactive terminal (editor).
    pub fn is_interactive(&self) -> bool {
        matches!(self, JJCommand::DescribeInEditor { .. })
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
