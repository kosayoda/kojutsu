use std::collections::{HashMap, HashSet};

use tui_input::Input;

use crate::dag::{DagEntry, DiffLine, FileChange};
use crate::graph::{self, GraphLines};
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx, GraphLineIdx, IndexVec};
use crate::jj_command::JJCommand;
use crate::keymap::{CommandFlags, KeymapNode};
use crate::repo::JjRepo;

/// A selected item in the DAG. Tied to commit identity (change ID) and file
/// path, so selections survive DAG refreshes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Selection {
    /// A selected file within a commit's diff.
    File {
        /// The commit's change ID (display prefix).
        change_id: String,
        /// File path within that commit's diff.
        path: String,
    },
    // Future: Line { change_id: String, path: String, line_idx: DiffLineIdx },
}

/// Metadata for a global toggle that persists across commands.
pub struct GlobalToggle {
    /// The `CommandFlags` bit this toggle controls.
    pub flag: CommandFlags,
    /// Short hint character shown in the status bar (e.g., "I").
    pub hint: &'static str,
    /// Human-readable label (e.g., "ignore-immutable").
    pub label: &'static str,
    /// CLI flag appended to jj commands (e.g., "--ignore-immutable").
    pub cli_flag: &'static str,
}

/// All global toggles. Single source of truth for status bar rendering,
/// help display, and CLI arg generation.
pub const GLOBAL_TOGGLES: &[GlobalToggle] = &[
    GlobalToggle {
        flag: CommandFlags::IGNORE_IMMUTABLE,
        hint: "I",
        label: "ignore-immutable",
        cli_flag: "--ignore-immutable",
    },
    GlobalToggle {
        flag: CommandFlags::IGNORE_WORKING_COPY,
        hint: "W",
        label: "ignore-working-copy",
        cli_flag: "--ignore-working-copy",
    },
    GlobalToggle {
        flag: CommandFlags::DEBUG,
        hint: "D",
        label: "debug",
        cli_flag: "--debug",
    },
];

/// The current interaction mode.
pub enum AppMode {
    /// Normal browsing.
    Normal,
    /// A prefix key was pressed; showing submenu options in the bottom bar.
    /// References point into the leaked `&'static Keymap`.
    Submenu {
        /// Display string for the prefix key (e.g., "s", "b", "g").
        key: String,
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
    /// Selecting an item from a list (e.g. picking a bookmark).
    SelectFromList {
        title: String,
        items: Vec<String>,
        selected: usize,
        on_select: PendingSelection,
    },
}

/// What to do after selecting an item from a list.
pub enum PendingSelection {
    /// Delete a bookmark on the selected commit.
    BookmarkDelete {
        change_id: String,
        flags: CommandFlags,
    },
    /// Forget a bookmark on the selected commit.
    BookmarkForget {
        change_id: String,
        flags: CommandFlags,
    },
    /// Move a bookmark to a target commit (enters TargetSelect after selection).
    BookmarkMove {
        change_id: String,
        flags: CommandFlags,
    },
    /// Rename a bookmark (enters TextInput after selection).
    BookmarkRename {
        change_id: String,
        flags: CommandFlags,
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
    /// Create a bookmark with the given name.
    BookmarkCreate {
        change_id: String,
        flags: CommandFlags,
    },
    /// Set (create or update) a bookmark.
    BookmarkSet {
        change_id: String,
        flags: CommandFlags,
    },
    /// Rename a bookmark (old name already selected, text is new name).
    BookmarkRename {
        old_name: String,
        flags: CommandFlags,
    },
    /// Track a remote bookmark (text is "name@remote").
    BookmarkTrack { flags: CommandFlags },
    /// Untrack a remote bookmark (text is "name@remote").
    BookmarkUntrack { flags: CommandFlags },
    /// Commit with inline message (text is the message).
    Commit {
        flags: CommandFlags,
        paths: Vec<String>,
    },
}

impl PendingCommand {
    /// Convert to a `JJCommand` given the user's input text.
    ///
    /// Panics if called on `Revset` -- that variant is handled separately.
    pub fn into_jj_command(self, text: String) -> JJCommand {
        match self {
            PendingCommand::Describe { change_id, flags } => JJCommand::Describe {
                change_id,
                message: text,
                flags,
            },
            PendingCommand::SquashWithMessage { builder, flags } => {
                builder.build_with_message(text, flags)
            }
            PendingCommand::Revset => panic!("Revset pending command handled separately"),
            PendingCommand::BookmarkCreate { change_id, flags } => JJCommand::BookmarkCreate {
                name: text,
                change_id,
                flags,
            },
            PendingCommand::BookmarkSet { change_id, flags } => JJCommand::BookmarkSet {
                name: text,
                change_id,
                flags,
            },
            PendingCommand::BookmarkRename { old_name, flags } => JJCommand::BookmarkRename {
                old_name,
                new_name: text,
                flags,
            },
            PendingCommand::BookmarkTrack { flags } => {
                JJCommand::BookmarkTrack { name: text, flags }
            }
            PendingCommand::BookmarkUntrack { flags } => {
                JJCommand::BookmarkUntrack { name: text, flags }
            }
            PendingCommand::Commit { flags, paths } => JJCommand::Commit {
                message: Some(text),
                paths,
                flags,
            },
        }
    }
}

/// What kind of two-commit target selection we're doing.
#[derive(Debug, Clone)]
pub enum TargetOperation {
    SquashInto,
    SquashOnto,
    SquashAfter,
    SquashBefore,
    RebaseRevision,
    RebaseSource,
    RebaseBranch,
    BookmarkMove { bookmark_name: String },
    DuplicateOnto,
}

impl TargetOperation {
    pub fn label(&self) -> &'static str {
        match self {
            TargetOperation::SquashInto => "squash into",
            TargetOperation::SquashOnto => "squash onto",
            TargetOperation::SquashAfter => "squash after",
            TargetOperation::SquashBefore => "squash before",
            TargetOperation::RebaseRevision => "rebase revision",
            TargetOperation::RebaseSource => "rebase source",
            TargetOperation::RebaseBranch => "rebase branch",
            TargetOperation::BookmarkMove { .. } => "move bookmark",
            TargetOperation::DuplicateOnto => "duplicate onto",
        }
    }

    pub fn follow_up(
        self,
        source: String,
        target: String,
        flags: CommandFlags,
        paths: Vec<String>,
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
                squash_follow_up(source, Some(squash_target), paths, flags)
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
            TargetOperation::BookmarkMove { bookmark_name } => {
                // Bookmark move executes immediately -- no follow-up choice.
                vec![FollowUpOption {
                    key: ' ', // won't be shown; auto-executed below
                    label: "move",
                    action: FollowUpAction::Execute(JJCommand::BookmarkMove {
                        name: bookmark_name.clone(),
                        target,
                        flags,
                    }),
                }]
            }
            TargetOperation::DuplicateOnto => {
                // Duplicate onto executes immediately -- single option, auto-executed.
                vec![FollowUpOption {
                    key: ' ',
                    label: "duplicate",
                    action: FollowUpAction::Execute(JJCommand::Duplicate {
                        change_id: source,
                        onto: Some(target),
                        flags,
                    }),
                }]
            }
        }
    }
}

/// Build follow-up options for a squash command.
fn squash_follow_up(
    source: String,
    target: Option<SquashTarget>,
    paths: Vec<String>,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    let default_cmd = JJCommand::Squash {
        change_id: source.clone(),
        target: target.clone(),
        message: MessageMode::Default,
        paths: paths.clone(),
        flags,
    };
    let use_dest_cmd = JJCommand::Squash {
        change_id: source.clone(),
        target: target.clone(),
        message: MessageMode::UseDestination,
        paths: paths.clone(),
        flags,
    };
    let builder = ReadyCommand::Squash {
        source,
        target,
        paths,
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
    source: String,
    target: String,
    source_mode: RebaseSourceMode,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    vec![
        FollowUpOption {
            key: 'o',
            label: "onto",
            action: FollowUpAction::Execute(JJCommand::Rebase {
                change_id: source.clone(),
                source_mode: source_mode.clone(),
                dest: RebaseDestMode::Onto(target.clone()),
                flags,
            }),
        },
        FollowUpOption {
            key: 'a',
            label: "after",
            action: FollowUpAction::Execute(JJCommand::Rebase {
                change_id: source.clone(),
                source_mode: source_mode.clone(),
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

/// Squash target variants (where to squash into).
#[derive(Debug, Clone)]
pub enum SquashTarget {
    Into(String),
    Onto(String),
    After(String),
    Before(String),
}

/// Rebase source mode.
#[derive(Debug, Clone)]
pub enum RebaseSourceMode {
    Revision,
    Source,
    Branch,
}

/// Rebase destination mode.
#[derive(Debug, Clone)]
pub enum RebaseDestMode {
    Onto(String),
    After(String),
    Before(String),
}

/// A partially-constructed command that needs a message from the user.
pub enum ReadyCommand {
    Squash {
        source: String,
        target: Option<SquashTarget>,
        paths: Vec<String>,
    },
}

impl ReadyCommand {
    /// Build with default message behavior (jj handles it).
    pub fn build_default(self, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash {
                source,
                target,
                paths,
            } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::Default,
                paths,
                flags,
            },
        }
    }

    /// Build with an inline message.
    pub fn build_with_message(self, message: String, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash {
                source,
                target,
                paths,
            } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::Inline(message),
                paths,
                flags,
            },
        }
    }

    /// Build with --use-destination-message.
    pub fn build_use_dest_message(self, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash {
                source,
                target,
                paths,
            } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::UseDestination,
                paths,
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
    CommitNode(EntryIdx),
    GraphLink(EntryIdx, GraphLineIdx),
    FileChange(EntryIdx, FileIdx),
    DiffLine(EntryIdx, FileIdx, DiffLineIdx),
}

/// One visual row in the list.
pub enum DisplayRow {
    /// A commit node line (graph glyph + commit info).
    CommitNode { entry_idx: EntryIdx },
    /// A graph link/pad line between commits.
    GraphLink {
        entry_idx: EntryIdx,
        line_idx: GraphLineIdx,
    },
    /// A file change line (shown when commit is unfolded).
    FileChange {
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    },
    /// A diff hunk line (shown when a file is unfolded).
    DiffLine {
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
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
    pub entries: IndexVec<EntryIdx, DagEntry>,
    pub graph: IndexVec<EntryIdx, GraphLines>,
    /// Flattened display rows (one per visual line).
    pub rows: Vec<DisplayRow>,
    /// Index into `rows` of the currently selected row.
    pub cursor: usize,
    /// Scroll offset of the list from the last render (set by ui::draw).
    pub last_scroll_offset: usize,
    /// Header height from the last render (for mouse click translation).
    pub last_header_height: u16,
    pub revset: String,
    /// Last failed revset attempt (pre-fills the input on retry).
    pub revset_draft: Option<String>,
    pub repo_root: String,
    /// Current interaction mode.
    pub mode: AppMode,
    /// Per-commit fold state: true = unfolded (showing files).
    pub unfolded: IndexVec<EntryIdx, bool>,
    /// Per-file fold state: (entry_idx, file_idx) -> unfolded.
    pub file_unfolded: HashMap<(EntryIdx, FileIdx), bool>,
    /// Lazily loaded file changes, keyed by entry index.
    pub file_cache: HashMap<EntryIdx, Vec<FileChange>>,
    /// Lazily loaded diff lines, keyed by (entry_idx, file_idx).
    pub diff_cache: HashMap<(EntryIdx, FileIdx), Vec<DiffLine>>,
    /// Global toggles that persist across commands.
    pub toggles: CommandFlags,
    /// Display string of the last command executed (shown in status bar).
    pub last_command: Option<String>,
    /// Whether to show line numbers in diff views.
    pub show_line_numbers: bool,
    /// Currently selected files (for partial operations like squash).
    pub selections: HashSet<Selection>,
}

impl App {
    pub fn new(entries: Vec<DagEntry>, revset: String, repo_root: String) -> Self {
        let entries = IndexVec::from_vec(entries);
        let graph = IndexVec::from_vec(graph::render(entries.as_slice()));
        let mut unfolded = IndexVec::new();
        for _ in 0..entries.len() {
            unfolded.push(false);
        }

        let mut app = Self {
            entries,
            graph,
            rows: Vec::new(),
            cursor: 0,
            last_scroll_offset: 0,
            last_header_height: 2,
            revset,
            revset_draft: None,
            repo_root,
            mode: AppMode::Normal,
            unfolded,
            file_unfolded: HashMap::new(),
            file_cache: HashMap::new(),
            diff_cache: HashMap::new(),
            toggles: CommandFlags::empty(),
            last_command: None,
            show_line_numbers: false,
            selections: HashSet::new(),
        };
        app.rebuild_rows();
        app
    }

    /// Rebuild the flattened row list from current fold state.
    pub fn rebuild_rows(&mut self) {
        // Remember what the cursor was pointing at so we can restore it.
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);

        self.rows.clear();
        for (entry_idx, gl) in self.graph.iter_enumerated() {
            self.rows.push(DisplayRow::CommitNode { entry_idx });

            if self.unfolded[entry_idx] {
                if let Some(files) = self.file_cache.get(&entry_idx) {
                    for file_idx_raw in 0..files.len() {
                        let file_idx = FileIdx::new(file_idx_raw);
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
                                for line_idx_raw in 0..diff_lines.len() {
                                    self.rows.push(DisplayRow::DiffLine {
                                        entry_idx,
                                        file_idx,
                                        line_idx: DiffLineIdx::new(line_idx_raw),
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Extra graph lines (link/pad/term) are rendered as separate
            // GraphLink rows between commits.
            for line_idx_raw in 0..gl.extra.len() {
                self.rows.push(DisplayRow::GraphLink {
                    entry_idx,
                    line_idx: GraphLineIdx::new(line_idx_raw),
                });
            }
        }

        // Restore cursor: try exact match, then fall back to parent file,
        // then parent commit. This handles fold scenarios where the cursor
        // was on a diff line that disappeared when the file was folded.
        let fallbacks: [Option<RowKey>; 3] = match prev_cursor {
            Some(RowKey::DiffLine(e, f, l)) => [
                Some(RowKey::DiffLine(e, f, l)),
                Some(RowKey::FileChange(e, f)),
                Some(RowKey::CommitNode(e)),
            ],
            Some(RowKey::FileChange(e, f)) => [
                Some(RowKey::FileChange(e, f)),
                Some(RowKey::CommitNode(e)),
                None,
            ],
            Some(key) => [Some(key), None, None],
            None => [None, None, None],
        };

        self.cursor = fallbacks
            .iter()
            .flatten()
            .find_map(|key| self.rows.iter().position(|r| r.key() == *key))
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

    /// Get the bookmarks of the commit the cursor is on.
    pub fn selected_bookmarks(&self) -> Option<&[crate::dag::BookmarkInfo]> {
        let entry_idx = match self.rows.get(self.cursor)? {
            DisplayRow::CommitNode { entry_idx }
            | DisplayRow::GraphLink { entry_idx, .. }
            | DisplayRow::FileChange { entry_idx, .. }
            | DisplayRow::DiffLine { entry_idx, .. } => *entry_idx,
        };
        Some(&self.entries[entry_idx].commit.bookmarks)
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

    /// Move selection to the first selectable row.
    pub fn move_to_top(&mut self) {
        for j in 0..self.rows.len() {
            if !matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the last selectable row.
    pub fn move_to_bottom(&mut self) {
        for j in (0..self.rows.len()).rev() {
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
            Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                ..
            }) => {
                // Folding on a diff line folds the parent file.
                self.toggle_file_fold(*entry_idx, *file_idx, jj);
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Selection
    // -----------------------------------------------------------------------

    /// Toggle file selection. If the file belongs to a different commit than
    /// existing selections, clears the old selections first (selections are
    /// scoped to one commit at a time).
    pub fn toggle_file_selection(&mut self, entry_idx: EntryIdx, file_idx: FileIdx) {
        let change_id = self.entries[entry_idx].commit.change_id.display.clone();
        let path = self.file_cache[&entry_idx][file_idx.raw()].path.clone();

        // Clear selections if switching to a different commit.
        if !self.selections.is_empty() {
            let same_commit = self.selections.iter().any(|s| match s {
                Selection::File { change_id: cid, .. } => *cid == change_id,
            });
            if !same_commit {
                self.selections.clear();
            }
        }

        let sel = Selection::File { change_id, path };
        if !self.selections.remove(&sel) {
            self.selections.insert(sel);
        }
    }

    /// Toggle selection for all files in a commit (select all / deselect all).
    /// Only works when the commit is unfolded.
    pub fn toggle_commit_selection(&mut self, entry_idx: EntryIdx) {
        if !self.unfolded[entry_idx] {
            return;
        }
        let Some(files) = self.file_cache.get(&entry_idx) else {
            return;
        };

        let change_id = self.entries[entry_idx].commit.change_id.display.clone();

        // Clear selections from other commits.
        if !self.selections.is_empty() {
            let same_commit = self.selections.iter().any(|s| match s {
                Selection::File { change_id: cid, .. } => *cid == change_id,
            });
            if !same_commit {
                self.selections.clear();
            }
        }

        // If all files are already selected, deselect all. Otherwise select all.
        let all_selected = files.iter().all(|f| {
            self.selections.contains(&Selection::File {
                change_id: change_id.clone(),
                path: f.path.clone(),
            })
        });

        if all_selected {
            self.selections.clear();
        } else {
            for f in files {
                self.selections.insert(Selection::File {
                    change_id: change_id.clone(),
                    path: f.path.clone(),
                });
            }
        }
    }

    /// Check if a specific file is selected.
    pub fn is_file_selected(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> bool {
        let change_id = &self.entries[entry_idx].commit.change_id.display;
        if let Some(files) = self.file_cache.get(&entry_idx) {
            if let Some(file) = files.get(file_idx.raw()) {
                return self.selections.iter().any(|s| match s {
                    Selection::File {
                        change_id: cid,
                        path,
                    } => cid == change_id && path == &file.path,
                });
            }
        }
        false
    }

    /// Get the file paths of all current selections.
    pub fn selected_file_paths(&self) -> Vec<String> {
        self.selections
            .iter()
            .map(|s| match s {
                Selection::File { path, .. } => path.clone(),
            })
            .collect()
    }

    /// Number of currently selected items.
    pub fn selection_count(&self) -> usize {
        self.selections.len()
    }

    /// Clear all selections.
    pub fn clear_selection(&mut self) {
        self.selections.clear();
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
        let entries = IndexVec::from_vec(entries);
        self.graph = IndexVec::from_vec(graph::render(entries.as_slice()));
        let mut unfolded = IndexVec::new();
        for _ in 0..entries.len() {
            unfolded.push(false);
        }
        self.unfolded = unfolded;
        self.file_unfolded.clear();
        self.file_cache.clear();
        self.diff_cache.clear();
        self.entries = entries;
        self.cursor = 0;
        self.rebuild_rows();
        Ok(())
    }

    fn toggle_commit_fold(&mut self, entry_idx: EntryIdx, jj: &JjRepo) {
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

    fn toggle_file_fold(&mut self, entry_idx: EntryIdx, file_idx: FileIdx, jj: &JjRepo) {
        let key = (entry_idx, file_idx);
        let currently_unfolded = self.file_unfolded.get(&key).copied().unwrap_or(false);

        if currently_unfolded {
            self.file_unfolded.insert(key, false);
        } else {
            // Lazy load diff lines.
            if !self.diff_cache.contains_key(&key) {
                if let Some(files) = self.file_cache.get(&entry_idx) {
                    if let Some(file) = files.get(file_idx.raw()) {
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
