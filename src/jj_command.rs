use std::path::Path;
use std::process::{Command, Output};

/// A typesafe representation of a jj CLI command.
#[derive(Debug, Clone)]
pub enum JJCommand {
    Abandon {
        change_id: String,
        retain_bookmarks: bool,
        restore_descendants: bool,
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
            JJCommand::Abandon {
                change_id,
                retain_bookmarks,
                restore_descendants,
            } => {
                let mut args = vec!["abandon".to_string()];
                if *retain_bookmarks {
                    args.push("--retain-bookmarks".to_string());
                }
                if *restore_descendants {
                    args.push("--restore-descendants".to_string());
                }
                args.push(change_id.clone());
                args
            }
        }
    }

    /// Human-readable display string shown in the command output overlay.
    pub fn display(&self) -> String {
        let args = self.args();
        format!("$ jj {}", args.join(" "))
    }

    /// Execute the command against a repository path.
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
