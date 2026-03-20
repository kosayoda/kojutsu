use std::collections::{HashMap, HashSet};

use tui_input::Input;

use crate::dag::{DagEntry, DiffLine, DiffLineKind, FileChange};
use crate::graph::{self, GraphLines};
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx, GraphLineIdx, IndexVec};

/// A persistent visual selection range within one file's diff.
#[derive(Clone)]
pub struct VisualRange {
    pub change_id: String,
    pub path: String,
    pub start_line: DiffLineIdx,
    pub end_line: DiffLineIdx,
}
use crate::jj_command::{ChangeSelection, JJCommand};
use crate::keymap::{CommandFlags, KeymapNode};
use crate::repo::JjRepo;

/// A selected item in the DAG. Tied to commit identity (change ID) and file
/// path, so selections survive DAG refreshes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Selection {
    /// Entire file selected.
    File { change_id: String, path: String },
    /// Individual diff line selected (added or removed).
    Line {
        change_id: String,
        path: String,
        /// Line number in the old file (`Some` for removed/context lines).
        old_line: Option<u32>,
        /// Line number in the new file (`Some` for added/context lines).
        new_line: Option<u32>,
    },
}

impl Selection {
    /// Get the change ID from any selection variant.
    pub fn change_id(&self) -> &str {
        match self {
            Selection::File { change_id, .. } | Selection::Line { change_id, .. } => change_id,
        }
    }

    /// Get the file path from any selection variant.
    pub fn path(&self) -> &str {
        match self {
            Selection::File { path, .. } | Selection::Line { path, .. } => path,
        }
    }
}

/// Selection state of a file (for UI display).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileSelectionState {
    /// No selections for this file.
    None,
    /// Some lines selected (but not all, and no File-level selection).
    Partial,
    /// Entire file selected (Selection::File entry exists).
    Full,
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

pub struct SelectionSummary {
    pub file_count: usize,
    pub full_file_count: usize,
    pub line_count: usize,
    pub has_full_files: bool,
}

impl SelectionSummary {
    pub fn display_text(&self) -> Option<String> {
        if self.file_count == 0 && self.line_count == 0 {
            return None;
        }
        if self.line_count == 0 {
            let noun = if self.full_file_count == 1 {
                "file"
            } else {
                "files"
            };
            return Some(format!("{} {} selected", self.full_file_count, noun));
        }
        if !self.has_full_files && self.file_count == 1 {
            let noun = if self.line_count == 1 {
                "line"
            } else {
                "lines"
            };
            return Some(format!("{} {} selected", self.line_count, noun));
        }
        if self.has_full_files && self.line_count > 0 {
            let file_noun = if self.full_file_count == 1 {
                "file"
            } else {
                "files"
            };
            let line_noun = if self.line_count == 1 {
                "line"
            } else {
                "lines"
            };
            return Some(format!(
                "{} {} + {} {} selected",
                self.full_file_count, file_noun, self.line_count, line_noun
            ));
        }
        let line_noun = if self.line_count == 1 {
            "line"
        } else {
            "lines"
        };
        let file_noun = if self.file_count == 1 {
            "file"
        } else {
            "files"
        };
        Some(format!(
            "{} {} in {} {} selected",
            self.line_count, line_noun, self.file_count, file_noun
        ))
    }

    pub fn submenu_suffix(&self) -> Option<String> {
        if self.file_count == 0 && self.line_count == 0 {
            return None;
        }
        if self.line_count == 0 {
            let noun = if self.full_file_count == 1 {
                "file"
            } else {
                "files"
            };
            return Some(format!("{} {}", self.full_file_count, noun));
        }
        if !self.has_full_files && self.file_count == 1 {
            let noun = if self.line_count == 1 {
                "line"
            } else {
                "lines"
            };
            return Some(format!("{} {}", self.line_count, noun));
        }
        if self.has_full_files && self.line_count > 0 {
            let file_noun = if self.full_file_count == 1 {
                "file"
            } else {
                "files"
            };
            let line_noun = if self.line_count == 1 {
                "line"
            } else {
                "lines"
            };
            return Some(format!(
                "{} {} + {} {}",
                self.full_file_count, file_noun, self.line_count, line_noun
            ));
        }
        let line_noun = if self.line_count == 1 {
            "line"
        } else {
            "lines"
        };
        let file_noun = if self.file_count == 1 {
            "file"
        } else {
            "files"
        };
        Some(format!(
            "{} {} in {} {}",
            self.line_count, line_noun, self.file_count, file_noun
        ))
    }
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
        selection: ChangeSelection,
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
            PendingCommand::Commit { flags, selection } => JJCommand::Commit {
                message: Some(text),
                selection,
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
        selection: ChangeSelection,
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
                squash_follow_up(source, Some(squash_target), selection, flags)
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
    selection: ChangeSelection,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    let default_cmd = JJCommand::Squash {
        change_id: source.clone(),
        target: target.clone(),
        message: MessageMode::Default,
        selection: selection.clone(),
        flags,
    };
    let use_dest_cmd = JJCommand::Squash {
        change_id: source.clone(),
        target: target.clone(),
        message: MessageMode::UseDestination,
        selection: selection.clone(),
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
        selection: ChangeSelection,
    },
}

impl ReadyCommand {
    /// Build with default message behavior (jj handles it).
    pub fn build_default(self, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash {
                source,
                target,
                selection,
            } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::Default,
                selection,
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
                selection,
            } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::Inline(message),
                selection,
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
                selection,
            } => JJCommand::Squash {
                change_id: source,
                target,
                message: MessageMode::UseDestination,
                selection,
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
    /// Visual mode anchor row index. When `Some`, visual selection is actively
    /// being extended. Only valid on DiffLine rows.
    pub visual_anchor: Option<usize>,
    /// Persistent visual range (survives exiting visual mode with `v`).
    /// Cleared on file collapse, refresh, or starting a new visual selection.
    pub visual_range: Option<VisualRange>,
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
            visual_anchor: None,
            visual_range: None,
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

    /// Whether J/K should jump to the next commit (vs next file).
    ///
    /// - On `CommitNode`: always commit-level.
    /// - On collapsed `FileChange`: commit-level (j already moves between files).
    /// - On expanded `FileChange`: file-level (skip diff lines to next file).
    /// - On `DiffLine`: file-level (escape the current diff).
    fn is_commit_level_jump(&self) -> bool {
        match self.rows.get(self.cursor) {
            Some(DisplayRow::CommitNode { .. }) => true,
            Some(DisplayRow::FileChange {
                entry_idx,
                file_idx,
            }) => {
                // Collapsed file → commit-level. Expanded → file-level.
                !self
                    .file_unfolded
                    .get(&(*entry_idx, *file_idx))
                    .copied()
                    .unwrap_or(false)
            }
            _ => false, // DiffLine, GraphLink → file-level
        }
    }

    /// Context-aware section jump upward.
    ///
    /// - Commit-level: jump to the previous commit.
    /// - File-level: jump to the previous file or commit.
    pub fn move_up_section(&mut self) {
        let commit_level = self.is_commit_level_jump();

        for j in (0..self.cursor).rev() {
            let target = if commit_level {
                matches!(self.rows[j], DisplayRow::CommitNode { .. })
            } else {
                matches!(
                    self.rows[j],
                    DisplayRow::FileChange { .. } | DisplayRow::CommitNode { .. }
                )
            };
            if target {
                self.cursor = j;
                return;
            }
        }
    }

    /// Context-aware section jump downward.
    ///
    /// - Commit-level: jump to the next commit.
    /// - File-level: jump to the next file or commit.
    pub fn move_down_section(&mut self) {
        let commit_level = self.is_commit_level_jump();

        for j in (self.cursor + 1)..self.rows.len() {
            let target = if commit_level {
                matches!(self.rows[j], DisplayRow::CommitNode { .. })
            } else {
                matches!(
                    self.rows[j],
                    DisplayRow::FileChange { .. } | DisplayRow::CommitNode { .. }
                )
            };
            if target {
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

        self.clear_other_commits(&change_id);

        // Clear any line-level selections for this file (File overrides Lines).
        self.selections
            .retain(|s| !matches!(s, Selection::Line { path: p, .. } if *p == path));

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
        if !self.file_cache.contains_key(&entry_idx) {
            return;
        }

        let change_id = self.entries[entry_idx].commit.change_id.display.clone();
        self.clear_other_commits(&change_id);

        // Collect file paths upfront to avoid borrowing self.file_cache across mutations.
        let file_paths: Vec<String> = self.file_cache[&entry_idx]
            .iter()
            .map(|f| f.path.clone())
            .collect();

        // If all files are already selected (File-level), deselect all.
        let all_selected = file_paths.iter().all(|p| {
            self.selections.contains(&Selection::File {
                change_id: change_id.clone(),
                path: p.clone(),
            })
        });

        if all_selected {
            self.selections.clear();
        } else {
            self.selections.clear();
            for p in file_paths {
                self.selections.insert(Selection::File {
                    change_id: change_id.clone(),
                    path: p,
                });
            }
        }
    }

    /// Toggle a single diff line selection (added/removed only).
    pub fn toggle_line_selection(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) {
        let change_id = self.entries[entry_idx].commit.change_id.display.clone();
        let file_path = self.file_cache[&entry_idx][file_idx.raw()].path.clone();

        // Extract what we need from the diff line before mutating self.
        let dl = &self.diff_cache[&(entry_idx, file_idx)][line_idx.raw()];
        if dl.kind != DiffLineKind::Added && dl.kind != DiffLineKind::Removed {
            return;
        }
        let old_line = dl.old_line;
        let new_line = dl.new_line;

        self.clear_other_commits(&change_id);

        // If there's a File-level selection for this file, remove it.
        self.selections.remove(&Selection::File {
            change_id: change_id.clone(),
            path: file_path.clone(),
        });

        let sel = Selection::Line {
            change_id,
            path: file_path,
            old_line,
            new_line,
        };
        if !self.selections.remove(&sel) {
            self.selections.insert(sel);
        }
    }

    /// Toggle all added/removed lines in a hunk (triggered by space on a header line).
    pub fn toggle_hunk_selection(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        header_line_idx: DiffLineIdx,
    ) {
        let change_id = self.entries[entry_idx].commit.change_id.display.clone();
        let file_path = self.file_cache[&entry_idx][file_idx.raw()].path.clone();

        // Collect hunk line data before mutating self.
        let mut hunk_lines = Vec::new();
        {
            let diff_lines = &self.diff_cache[&(entry_idx, file_idx)];
            for dl in diff_lines.iter().skip(header_line_idx.raw() + 1) {
                if dl.kind == DiffLineKind::Header {
                    break;
                }
                if dl.kind == DiffLineKind::Added || dl.kind == DiffLineKind::Removed {
                    hunk_lines.push(Selection::Line {
                        change_id: change_id.clone(),
                        path: file_path.clone(),
                        old_line: dl.old_line,
                        new_line: dl.new_line,
                    });
                }
            }
        }

        self.clear_other_commits(&change_id);

        // Remove any File-level selection for this file.
        self.selections.remove(&Selection::File {
            change_id: change_id.clone(),
            path: file_path.clone(),
        });

        // If all hunk lines are already selected, deselect them. Otherwise select all.
        let all_selected = hunk_lines.iter().all(|s| self.selections.contains(s));
        if all_selected {
            for s in &hunk_lines {
                self.selections.remove(s);
            }
        } else {
            for s in hunk_lines {
                self.selections.insert(s);
            }
        }
    }

    /// Check if a specific diff line is selected.
    pub fn is_line_selected(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) -> bool {
        let change_id = &self.entries[entry_idx].commit.change_id.display;
        let file_path = &self.file_cache[&entry_idx][file_idx.raw()].path;
        let diff_line = &self.diff_cache[&(entry_idx, file_idx)][line_idx.raw()];

        // If the whole file is selected, all lines are implicitly selected.
        if self.selections.contains(&Selection::File {
            change_id: change_id.clone(),
            path: file_path.clone(),
        }) {
            return diff_line.kind == DiffLineKind::Added
                || diff_line.kind == DiffLineKind::Removed;
        }

        self.selections.contains(&Selection::Line {
            change_id: change_id.clone(),
            path: file_path.clone(),
            old_line: diff_line.old_line,
            new_line: diff_line.new_line,
        })
    }

    /// Get the selection state of a file for UI display.
    pub fn file_selection_state(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> FileSelectionState {
        let change_id = &self.entries[entry_idx].commit.change_id.display;
        let file_path = &self.file_cache[&entry_idx][file_idx.raw()].path;

        // Explicit file-level selection.
        if self.selections.contains(&Selection::File {
            change_id: change_id.clone(),
            path: file_path.clone(),
        }) {
            return FileSelectionState::Full;
        }

        // Count line-level selections for this file.
        let selected_count = self
            .selections
            .iter()
            .filter(|s| {
                matches!(s, Selection::Line { change_id: cid, path, .. }
                    if cid == change_id && path == file_path)
            })
            .count();

        if selected_count == 0 {
            return FileSelectionState::None;
        }

        // Count selectable lines (added/removed) in the diff.
        // If all are selected, promote to Full.
        let selectable_count = self
            .diff_cache
            .get(&(entry_idx, file_idx))
            .map(|diff_lines| {
                diff_lines
                    .iter()
                    .filter(|dl| dl.kind == DiffLineKind::Added || dl.kind == DiffLineKind::Removed)
                    .count()
            })
            .unwrap_or(0);

        if selectable_count > 0 && selected_count >= selectable_count {
            FileSelectionState::Full
        } else {
            FileSelectionState::Partial
        }
    }

    /// Check if there are any line-level selections (vs only file-level).
    pub fn has_line_selections(&self) -> bool {
        self.selections
            .iter()
            .any(|s| matches!(s, Selection::Line { .. }))
    }

    /// Get the unique file paths from all selections.
    pub fn selected_file_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .selections
            .iter()
            .map(|s| s.path().to_string())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// Number of currently selected items.
    pub fn selection_count(&self) -> usize {
        self.selections.len()
    }

    pub fn selection_summary(&self) -> SelectionSummary {
        let mut files = HashSet::new();
        let mut full_files = HashSet::new();
        let mut line_count = 0usize;
        let mut has_full_files = false;

        for selection in &self.selections {
            files.insert(selection.path().to_string());
            match selection {
                Selection::File(file_ref) => {
                    has_full_files = true;
                    full_files.insert(file_ref.path.clone());
                }
                Selection::Line { .. } => line_count += 1,
            }
        }

        SelectionSummary {
            file_count: files.len(),
            full_file_count: full_files.len(),
            line_count,
            has_full_files,
        }
    }

    /// Clear all selections.
    pub fn clear_selection(&mut self) {
        self.selections.clear();
    }

    /// Clear selections from other commits if switching to a different one.
    fn clear_other_commits(&mut self, change_id: &str) {
        if !self.selections.is_empty() {
            let same_commit = self.selections.iter().any(|s| s.change_id() == change_id);
            if !same_commit {
                self.selections.clear();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Visual mode
    // -----------------------------------------------------------------------

    /// Toggle visual mode.
    ///
    /// - If not in visual mode: start visual selection on current line (must be
    ///   an Added/Removed diff line). Clears any persistent visual range.
    /// - If in visual mode: exit and persist the current range as a `VisualRange`.
    pub fn toggle_visual_mode(&mut self) {
        if self.visual_anchor.is_some() {
            // Exit visual mode → persist the range.
            self.persist_visual_range();
            self.visual_anchor = None;
        } else if let Some(DisplayRow::DiffLine {
            entry_idx,
            file_idx,
            line_idx,
        }) = self.rows.get(self.cursor)
        {
            let dl = &self.diff_cache[&(*entry_idx, *file_idx)][line_idx.raw()];
            if dl.kind == DiffLineKind::Added || dl.kind == DiffLineKind::Removed {
                self.visual_range = None; // clear any old persistent range
                self.visual_anchor = Some(self.cursor);
            }
        }
    }

    /// Exit visual mode, discarding the range (no persistence).
    pub fn cancel_visual_mode(&mut self) {
        self.visual_anchor = None;
    }

    /// Whether visual mode is actively selecting (anchor set).
    pub fn in_visual_mode(&self) -> bool {
        self.visual_anchor.is_some()
    }

    /// Get the active visual range as (lo, hi) row indices (inclusive).
    /// Only valid while `in_visual_mode()` is true.
    fn active_visual_row_range(&self) -> Option<(usize, usize)> {
        self.visual_anchor
            .map(|anchor| (anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// Get the (entry_idx, file_idx) of the visual mode anchor.
    fn visual_file(&self) -> Option<(EntryIdx, FileIdx)> {
        let anchor = self.visual_anchor?;
        match self.rows.get(anchor) {
            Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                ..
            }) => Some((*entry_idx, *file_idx)),
            _ => None,
        }
    }

    /// Persist the current active visual range as a `VisualRange`.
    fn persist_visual_range(&mut self) {
        let Some((lo, hi)) = self.active_visual_row_range() else {
            return;
        };

        // Find the DiffLineIdx bounds from the row range.
        let mut start_line = None;
        let mut end_line = None;
        let mut change_id = None;
        let mut path = None;

        for idx in lo..=hi {
            if let Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            }) = self.rows.get(idx)
            {
                if start_line.is_none() {
                    start_line = Some(*line_idx);
                    change_id = Some(self.entries[*entry_idx].commit.change_id.display.clone());
                    path = Some(self.file_cache[entry_idx][file_idx.raw()].path.clone());
                }
                end_line = Some(*line_idx);
            }
        }

        if let (Some(start), Some(end), Some(cid), Some(p)) =
            (start_line, end_line, change_id, path)
        {
            self.visual_range = Some(VisualRange {
                change_id: cid,
                path: p,
                start_line: start,
                end_line: end,
            });
        }
    }

    /// Check if a diff line is within the visual range (active or persistent).
    pub fn is_in_visual_range(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) -> bool {
        // Check active visual range first (while selecting).
        if let Some((lo, hi)) = self.active_visual_row_range() {
            // Check by row index (the cursor range).
            if let Some(row_idx) = self.rows.iter().position(|r| {
                matches!(r, DisplayRow::DiffLine { entry_idx: e, file_idx: f, line_idx: l }
                    if *e == entry_idx && *f == file_idx && *l == line_idx)
            }) {
                return row_idx >= lo && row_idx <= hi;
            }
        }

        // Check persistent visual range.
        if let Some(vr) = &self.visual_range {
            let cid = &self.entries[entry_idx].commit.change_id.display;
            if let Some(files) = self.file_cache.get(&entry_idx) {
                if let Some(file) = files.get(file_idx.raw()) {
                    if cid == &vr.change_id
                        && file.path == vr.path
                        && line_idx >= vr.start_line
                        && line_idx <= vr.end_line
                    {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Check if the cursor is on a line within the persistent visual range.
    pub fn cursor_in_persistent_visual_range(&self) -> bool {
        let Some(vr) = &self.visual_range else {
            return false;
        };
        if let Some(DisplayRow::DiffLine {
            entry_idx,
            file_idx,
            line_idx,
        }) = self.rows.get(self.cursor)
        {
            let cid = &self.entries[*entry_idx].commit.change_id.display;
            if let Some(file) = self
                .file_cache
                .get(entry_idx)
                .and_then(|f| f.get(file_idx.raw()))
            {
                return cid == &vr.change_id
                    && file.path == vr.path
                    && *line_idx >= vr.start_line
                    && *line_idx <= vr.end_line;
            }
        }
        false
    }

    /// Move cursor down, constrained to the same file's diff lines (visual mode).
    pub fn visual_move_down(&mut self) {
        let Some((anchor_entry, anchor_file)) = self.visual_file() else {
            return;
        };
        for j in (self.cursor + 1)..self.rows.len() {
            if matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                continue;
            }
            match &self.rows[j] {
                DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    ..
                } if *entry_idx == anchor_entry && *file_idx == anchor_file => {
                    self.cursor = j;
                    return;
                }
                _ => return, // hit file/commit boundary, stop
            }
        }
    }

    /// Move cursor up, constrained to the same file's diff lines (visual mode).
    pub fn visual_move_up(&mut self) {
        let Some((anchor_entry, anchor_file)) = self.visual_file() else {
            return;
        };
        for j in (0..self.cursor).rev() {
            if matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                continue;
            }
            match &self.rows[j] {
                DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    ..
                } if *entry_idx == anchor_entry && *file_idx == anchor_file => {
                    self.cursor = j;
                    return;
                }
                _ => return, // hit file/commit boundary, stop
            }
        }
    }

    /// Toggle all selectable lines in a visual range (active or persistent).
    /// If active visual mode, persists the range first.
    /// Clears the persistent range after toggling.
    pub fn toggle_visual_selection(&mut self) {
        // If actively selecting, persist first.
        if self.visual_anchor.is_some() {
            self.persist_visual_range();
            self.visual_anchor = None;
        }

        let Some(vr) = self.visual_range.clone() else {
            return;
        };

        // Collect line data from the persistent range before mutating.
        let line_data: Vec<(String, String, Option<u32>, Option<u32>)> = self
            .diff_cache
            .iter()
            .filter_map(|((eidx, fidx), diff_lines)| {
                let cid = &self.entries[*eidx].commit.change_id.display;
                let path = &self.file_cache[eidx][fidx.raw()].path;
                if cid != &vr.change_id || path != &vr.path {
                    return None;
                }
                let lines: Vec<_> = diff_lines
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| {
                        let idx = DiffLineIdx::new(*i);
                        idx >= vr.start_line && idx <= vr.end_line
                    })
                    .filter(|(_, dl)| {
                        dl.kind == DiffLineKind::Added || dl.kind == DiffLineKind::Removed
                    })
                    .map(|(_, dl)| {
                        (
                            vr.change_id.clone(),
                            vr.path.clone(),
                            dl.old_line,
                            dl.new_line,
                        )
                    })
                    .collect();
                Some(lines)
            })
            .flatten()
            .collect();

        if line_data.is_empty() {
            return;
        }

        // If all are already selected, deselect. Otherwise select.
        let all_selected = line_data.iter().all(|(cid, path, ol, nl)| {
            self.selections.contains(&Selection::Line {
                change_id: cid.clone(),
                path: path.clone(),
                old_line: *ol,
                new_line: *nl,
            })
        });

        if all_selected {
            for (cid, path, ol, nl) in &line_data {
                self.selections.remove(&Selection::Line {
                    change_id: cid.clone(),
                    path: path.clone(),
                    old_line: *ol,
                    new_line: *nl,
                });
            }
        } else {
            if let Some((cid, ..)) = line_data.first() {
                self.clear_other_commits(cid);
            }
            for (cid, path, ol, nl) in line_data {
                self.selections.remove(&Selection::File {
                    change_id: cid.clone(),
                    path: path.clone(),
                });
                self.selections.insert(Selection::Line {
                    change_id: cid,
                    path,
                    old_line: ol,
                    new_line: nl,
                });
            }
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
        self.visual_anchor = None;
        self.visual_range = None;
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
            // Clear visual range if it's for this file.
            if let Some(vr) = &self.visual_range {
                let cid = &self.entries[entry_idx].commit.change_id.display;
                if let Some(file) = self
                    .file_cache
                    .get(&entry_idx)
                    .and_then(|f| f.get(file_idx.raw()))
                {
                    if cid == &vr.change_id && file.path == vr.path {
                        self.visual_range = None;
                    }
                }
            }
            self.visual_anchor = None;
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
