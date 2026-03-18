use std::collections::HashMap;

use tui_input::Input;

use crate::dag::{DagEntry, DiffLine, FileChange};
use crate::graph::{self, GraphLines};
use crate::jj_command::JJCommand;
use crate::keymap::{CommandFlags, KeymapNode};
use crate::repo::JjRepo;

/// The current interaction mode.
pub enum AppMode {
    /// Normal browsing.
    Normal,
    /// A prefix key was pressed; showing submenu options in the bottom bar.
    /// References point into the leaked `&'static Keymap`.
    Submenu {
        label: &'static str,
        children: &'static [(keymap_parser::Node, KeymapNode)],
        flags: CommandFlags,
    },
    /// Showing the result of a shell command. Dismissed on next keypress.
    CommandOutput {
        /// The command that was run, e.g. `"$ jj abandon xvzwolmw"`.
        command: String,
        /// Raw stdout+stderr bytes (may contain ANSI color codes).
        output: Vec<u8>,
        /// Whether the command succeeded.
        success: bool,
    },
    /// Help overlay showing all keybindings.
    Help,
    /// Single-line text input in the bottom bar.
    TextInput {
        prompt: String,
        input: Input,
        on_submit: PendingCommand,
    },
    /// Navigating to select a target commit for a two-commit operation.
    TargetSelect {
        prompt: &'static str,
        source: String,
        restore_cursor: usize,
        operation: TargetOperation,
        flags: CommandFlags,
    },
    /// Choosing from a set of follow-up options after target selection.
    FollowUp {
        prompt: String,
        options: Vec<FollowUpOption>,
    },
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
}

/// What to do when a TextInput is submitted.
pub enum PendingCommand {
    Describe {
        change_id: String,
        flags: CommandFlags,
    },
    SquashWithMessage {
        builder: ReadyCommand,
        flags: CommandFlags,
    },
    /// The text is a revset expression to evaluate.
    Revset,
}

impl PendingCommand {
    /// Convert to a `JJCommand` given the user's input text.
    ///
    /// Panics if called on `Revset` -- that variant is handled separately.
    pub fn into_jj_command(self, message: String) -> JJCommand {
        match self {
            PendingCommand::Describe { change_id, flags } => JJCommand::Describe {
                change_id,
                message,
                flags,
            },
            PendingCommand::SquashWithMessage { builder, flags } => {
                builder.build_with_message(message, flags)
            }
            PendingCommand::Revset => {
                unreachable!("Revset pending command should not be converted to JJCommand")
            }
        }
    }
}

/// What kind of two-commit target selection we're doing.
#[derive(Debug, Clone, Copy)]
pub enum TargetOperation {
    SquashInto,
    SquashOnto,
    SquashAfter,
    SquashBefore,
    RebaseRevision,
    RebaseSource,
    RebaseBranch,
}

impl TargetOperation {
    pub fn label(self) -> &'static str {
        match self {
            TargetOperation::SquashInto => "squash into",
            TargetOperation::SquashOnto => "squash onto",
            TargetOperation::SquashAfter => "squash after",
            TargetOperation::SquashBefore => "squash before",
            TargetOperation::RebaseRevision => "rebase revision",
            TargetOperation::RebaseSource => "rebase source",
            TargetOperation::RebaseBranch => "rebase branch",
        }
    }

    /// Build follow-up options given source and target.
    pub fn follow_up(
        self,
        source: String,
        target: String,
        flags: CommandFlags,
    ) -> Vec<FollowUpOption> {
        match self {
            TargetOperation::SquashInto
            | TargetOperation::SquashOnto
            | TargetOperation::SquashAfter
            | TargetOperation::SquashBefore => {
                let squash_target = match self {
                    TargetOperation::SquashInto => SquashTarget::Into(target),
                    TargetOperation::SquashOnto => SquashTarget::Onto(target),
                    TargetOperation::SquashAfter => SquashTarget::After(target),
                    TargetOperation::SquashBefore => SquashTarget::Before(target),
                    _ => unreachable!(),
                };
                squash_follow_up(source, Some(squash_target), flags)
            }
            TargetOperation::RebaseRevision => {
                rebase_follow_up(source, target, RebaseSourceMode::Revision, flags)
            }
            TargetOperation::RebaseSource => {
                rebase_follow_up(source, target, RebaseSourceMode::Source, flags)
            }
            TargetOperation::RebaseBranch => {
                rebase_follow_up(source, target, RebaseSourceMode::Branch, flags)
            }
        }
    }
}

/// Build follow-up options for a squash command.
fn squash_follow_up(
    source: String,
    target: Option<SquashTarget>,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    let default_cmd = JJCommand::Squash {
        change_id: source.clone(),
        target: target.clone(),
        message: MessageMode::Default,
        flags,
    };
    let use_dest_cmd = JJCommand::Squash {
        change_id: source.clone(),
        target: target.clone(),
        message: MessageMode::UseDestination,
        flags,
    };
    let builder = ReadyCommand::Squash { source, target };

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
                prompt: "message: ".to_string(),
                pending: PendingCommand::SquashWithMessage { builder, flags },
            },
        },
        FollowUpOption {
            key: 'u',
            label: "use dest message",
            action: FollowUpAction::Execute(use_dest_cmd),
        },
    ]
}

/// How the source was specified for rebase (-r, -s, -b).
#[derive(Debug, Clone, Copy)]
pub enum RebaseSourceMode {
    /// -r: single revision, descendants rebased onto parent.
    Revision,
    /// -s: revision + all descendants.
    Source,
    /// -b: whole branch.
    Branch,
}

/// Where to rebase to (-d, -A, -B).
#[derive(Debug, Clone)]
pub enum RebaseDestMode {
    /// -d/--onto: onto the target.
    Onto(String),
    /// -A/--insert-after: after the target.
    After(String),
    /// -B/--insert-before: before the target.
    Before(String),
}

/// Build follow-up options for a rebase command (destination mode choice).
fn rebase_follow_up(
    source: String,
    target: String,
    source_mode: RebaseSourceMode,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    vec![
        FollowUpOption {
            key: 'd',
            label: "onto",
            action: FollowUpAction::Execute(JJCommand::Rebase {
                change_id: source.clone(),
                source_mode,
                dest: RebaseDestMode::Onto(target.clone()),
                flags,
            }),
        },
        FollowUpOption {
            key: 'a',
            label: "after",
            action: FollowUpAction::Execute(JJCommand::Rebase {
                change_id: source.clone(),
                source_mode,
                dest: RebaseDestMode::After(target.clone()),
                flags,
            }),
        },
        FollowUpOption {
            key: 'b',
            label: "before",
            action: FollowUpAction::Execute(JJCommand::Rebase {
                change_id: source,
                source_mode,
                dest: RebaseDestMode::Before(target),
                flags,
            }),
        },
    ]
}

/// The target type for a targeted squash.
#[derive(Debug, Clone)]
pub enum SquashTarget {
    Into(String),
    Onto(String),
    After(String),
    Before(String),
}

/// A command that's ready to execute, possibly with a message choice.
#[derive(Debug, Clone)]
pub enum ReadyCommand {
    Squash {
        source: String,
        target: Option<SquashTarget>,
    },
}

impl ReadyCommand {
    /// Build with default message behavior (jj handles it).
    pub fn build_default(self, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash { source, target } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::Default,
                flags,
            },
        }
    }

    /// Build with an inline message.
    pub fn build_with_message(self, message: String, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash { source, target } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::Inline(message),
                flags,
            },
        }
    }

    /// Build with --use-destination-message.
    pub fn build_use_dest_message(self, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash { source, target } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::UseDestination,
                flags,
            },
        }
    }
}

/// How to handle the commit message during squash.
#[derive(Debug, Clone)]
pub enum MessageMode {
    /// Let jj handle it (auto-merge, opens editor if needed).
    Default,
    /// Use -m "message".
    Inline(String),
    /// Use --use-destination-message.
    UseDestination,
}

/// Identifies a display row for cursor restore after rebuild.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowKey {
    CommitNode(usize),
    GraphLink(usize, usize),
    FileChange(usize, usize),
    DiffLine(usize, usize, usize),
}

/// One visual row in the list.
pub enum DisplayRow {
    /// A commit node line (graph glyph + commit info).
    CommitNode { entry_idx: usize },
    /// A graph link/pad line between commits.
    GraphLink { entry_idx: usize, line_idx: usize },
    /// A file change line (shown when commit is unfolded).
    FileChange { entry_idx: usize, file_idx: usize },
    /// A diff hunk line (shown when a file is unfolded).
    DiffLine {
        entry_idx: usize,
        file_idx: usize,
        line_idx: usize,
    },
}

impl DisplayRow {
    pub fn key(&self) -> RowKey {
        match *self {
            DisplayRow::CommitNode { entry_idx } => RowKey::CommitNode(entry_idx),
            DisplayRow::GraphLink {
                entry_idx,
                line_idx,
            } => RowKey::GraphLink(entry_idx, line_idx),
            DisplayRow::FileChange {
                entry_idx,
                file_idx,
            } => RowKey::FileChange(entry_idx, file_idx),
            DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            } => RowKey::DiffLine(entry_idx, file_idx, line_idx),
        }
    }
}

/// Application state. Pure data -- no I/O, no rendering.
pub struct App {
    pub entries: Vec<DagEntry>,
    pub graph: Vec<GraphLines>,
    /// Flattened display rows (one per visual line).
    pub rows: Vec<DisplayRow>,
    /// Index into `rows` of the currently selected row.
    pub cursor: usize,
    /// Scroll offset of the list from the last render (set by ui::draw).
    pub last_scroll_offset: usize,
    pub revset: String,
    /// Last failed revset attempt (pre-fills the input on retry).
    pub revset_draft: Option<String>,
    pub repo_root: String,
    /// Current interaction mode.
    pub mode: AppMode,
    /// Per-commit fold state: true = unfolded (showing files).
    pub unfolded: Vec<bool>,
    /// Per-file fold state: (entry_idx, file_idx) -> unfolded.
    pub file_unfolded: HashMap<(usize, usize), bool>,
    /// Lazily loaded file changes, keyed by entry index.
    pub file_cache: HashMap<usize, Vec<FileChange>>,
    /// Lazily loaded diff lines, keyed by (entry_idx, file_idx).
    pub diff_cache: HashMap<(usize, usize), Vec<DiffLine>>,
}

impl App {
    pub fn new(entries: Vec<DagEntry>, revset: String, repo_root: String) -> Self {
        let graph = graph::render(&entries);
        let unfolded = vec![false; entries.len()];

        let mut app = Self {
            entries,
            graph,
            rows: Vec::new(),
            cursor: 0,
            last_scroll_offset: 0,
            revset,
            revset_draft: None,
            repo_root,
            mode: AppMode::Normal,
            unfolded,
            file_unfolded: HashMap::new(),
            file_cache: HashMap::new(),
            diff_cache: HashMap::new(),
        };
        app.rebuild_rows();
        app
    }

    /// Rebuild the flattened row list from current fold state.
    pub fn rebuild_rows(&mut self) {
        // Remember what the cursor was pointing at so we can restore it.
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);

        self.rows.clear();
        for (entry_idx, gl) in self.graph.iter().enumerate() {
            self.rows.push(DisplayRow::CommitNode { entry_idx });

            if self.unfolded[entry_idx] {
                if let Some(files) = self.file_cache.get(&entry_idx) {
                    for file_idx in 0..files.len() {
                        self.rows.push(DisplayRow::FileChange {
                            entry_idx,
                            file_idx,
                        });

                        // If this file is unfolded, show diff lines.
                        if self
                            .file_unfolded
                            .get(&(entry_idx, file_idx))
                            .copied()
                            .unwrap_or(false)
                        {
                            if let Some(diff_lines) = self.diff_cache.get(&(entry_idx, file_idx)) {
                                for line_idx in 0..diff_lines.len() {
                                    self.rows.push(DisplayRow::DiffLine {
                                        entry_idx,
                                        file_idx,
                                        line_idx,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Graph lines 0 and 1 are consumed by the 2-line CommitNode
            // ListItem. Remaining lines are rendered as separate GraphLink rows.
            for line_idx in 2..gl.lines.len() {
                self.rows.push(DisplayRow::GraphLink {
                    entry_idx,
                    line_idx,
                });
            }
        }

        // Restore cursor to the exact same row, or fall back to the commit.
        self.cursor = prev_cursor
            .and_then(|key| self.rows.iter().position(|r| r.key() == key))
            .unwrap_or(0);
    }

    /// Get the change ID (unique prefix) of the commit the cursor is on.
    ///
    /// Works from any row type -- files and diff lines resolve to their
    /// parent commit.
    pub fn selected_change_id(&self) -> Option<&str> {
        let entry_idx = match self.rows.get(self.cursor)? {
            DisplayRow::CommitNode { entry_idx }
            | DisplayRow::GraphLink { entry_idx, .. }
            | DisplayRow::FileChange { entry_idx, .. }
            | DisplayRow::DiffLine { entry_idx, .. } => *entry_idx,
        };
        let id = &self.entries[entry_idx].commit.change_id;
        Some(&id.display[..id.prefix_len.min(id.display.len())])
    }

    /// Get the description of the commit the cursor is on.
    pub fn selected_description(&self) -> Option<&str> {
        let entry_idx = match self.rows.get(self.cursor)? {
            DisplayRow::CommitNode { entry_idx }
            | DisplayRow::GraphLink { entry_idx, .. }
            | DisplayRow::FileChange { entry_idx, .. }
            | DisplayRow::DiffLine { entry_idx, .. } => *entry_idx,
        };
        self.entries[entry_idx].commit.description.as_deref()
    }

    /// Get the scroll offset from the last render.
    pub fn scroll_offset(&self) -> usize {
        self.last_scroll_offset
    }

    /// Move selection to the previous selectable row (commit, file, or diff line).
    pub fn move_up(&mut self) {
        for j in (0..self.cursor).rev() {
            if !matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the next selectable row (commit, file, or diff line).
    pub fn move_down(&mut self) {
        for j in (self.cursor + 1)..self.rows.len() {
            if !matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the previous commit node (section jump).
    pub fn move_up_section(&mut self) {
        for j in (0..self.cursor).rev() {
            if matches!(self.rows[j], DisplayRow::CommitNode { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the next commit node (section jump).
    pub fn move_down_section(&mut self) {
        for j in (self.cursor + 1)..self.rows.len() {
            if matches!(self.rows[j], DisplayRow::CommitNode { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Toggle fold on the currently selected row.
    ///
    /// - On a commit row: toggle showing file changes.
    /// - On a file row: toggle showing diff hunks.
    pub fn toggle_fold(&mut self, jj: &JjRepo) {
        match self.rows.get(self.cursor) {
            Some(DisplayRow::CommitNode { entry_idx }) => {
                self.toggle_commit_fold(*entry_idx, jj);
            }
            Some(DisplayRow::FileChange {
                entry_idx,
                file_idx,
            }) => {
                self.toggle_file_fold(*entry_idx, *file_idx, jj);
            }
            _ => {}
        }
    }

    /// Jump to the working copy commit (`@`).
    pub fn jump_to_working_copy(&mut self) {
        if let Some(pos) = self.rows.iter().position(|r| {
            matches!(r, DisplayRow::CommitNode { entry_idx }
                if self.entries[*entry_idx].commit.is_working_copy)
        }) {
            self.cursor = pos;
        }
    }

    /// Move cursor up by `n` selectable rows (commits or files).
    pub fn page_up(&mut self, n: usize) {
        for _ in 0..n {
            let prev = self.cursor;
            self.move_up();
            if self.cursor == prev {
                break;
            }
        }
    }

    /// Move cursor down by `n` selectable rows (commits or files).
    pub fn page_down(&mut self, n: usize) {
        for _ in 0..n {
            let prev = self.cursor;
            self.move_down();
            if self.cursor == prev {
                break;
            }
        }
    }

    /// Select a specific row index (e.g. from mouse click), snapping to the
    /// nearest non-graph-link row at or after `row`.
    pub fn select_row(&mut self, row: usize) {
        let target = row.min(self.rows.len().saturating_sub(1));
        for j in target..self.rows.len() {
            if !matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                self.cursor = j;
                return;
            }
        }
        for j in (0..target).rev() {
            if !matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Get the text to pre-fill the revset input with.
    /// Uses the last failed draft if one exists, otherwise the current revset.
    pub fn revset_input_text(&self) -> &str {
        self.revset_draft.as_deref().unwrap_or(&self.revset)
    }

    /// Reload DAG data from the repo.
    pub fn refresh(&mut self, jj: &JjRepo, revset: &str) {
        let _ = self.try_refresh(jj, revset);
    }

    /// Try to reload DAG data. Returns an error string on failure.
    pub fn try_refresh(&mut self, jj: &JjRepo, revset: &str) -> Result<(), String> {
        let entries = jj.evaluate_revset(revset).map_err(|e| format!("{e:#}"))?;
        self.graph = graph::render(&entries);
        self.unfolded = vec![false; entries.len()];
        self.file_unfolded.clear();
        self.file_cache.clear();
        self.diff_cache.clear();
        self.entries = entries;
        self.cursor = 0;
        self.rebuild_rows();
        Ok(())
    }

    fn toggle_commit_fold(&mut self, entry_idx: usize, jj: &JjRepo) {
        if self.unfolded[entry_idx] {
            self.unfolded[entry_idx] = false;
        } else {
            if !self.file_cache.contains_key(&entry_idx) {
                let graph_id = &self.entries[entry_idx].commit.graph_id;
                let files = jj.file_changes(graph_id).unwrap_or_default();
                self.file_cache.insert(entry_idx, files);
            }
            self.unfolded[entry_idx] = true;
        }
        self.rebuild_rows();
    }

    fn toggle_file_fold(&mut self, entry_idx: usize, file_idx: usize, jj: &JjRepo) {
        let key = (entry_idx, file_idx);
        let currently_unfolded = self.file_unfolded.get(&key).copied().unwrap_or(false);

        if currently_unfolded {
            self.file_unfolded.insert(key, false);
        } else {
            // Lazy load diff lines.
            if !self.diff_cache.contains_key(&key) {
                if let Some(files) = self.file_cache.get(&entry_idx) {
                    if let Some(file) = files.get(file_idx) {
                        let graph_id = &self.entries[entry_idx].commit.graph_id;
                        let diff_lines = jj.file_diff(graph_id, &file.path).unwrap_or_default();
                        self.diff_cache.insert(key, diff_lines);
                    }
                }
            }
            self.file_unfolded.insert(key, true);
        }
        self.rebuild_rows();
    }
}
