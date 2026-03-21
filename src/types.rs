use strum::IntoEnumIterator as _;
use tui_input::Input;

use crate::{
    idx::{DiffLineIdx, EntryIdx, FileIdx, GraphLineIdx},
    jj_command::{ChangeSelection, JJCommand},
    keymap::CommandFlags,
};

pub type Str = compact_str::CompactString;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ChangeId(Str);

impl std::fmt::Display for ChangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl ChangeId {
    pub fn new(s: impl Into<Str>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq<String> for ChangeId {
    fn eq(&self, other: &String) -> bool {
        self.as_str().eq(other.as_str())
    }
}

impl PartialEq<str> for ChangeId {
    fn eq(&self, other: &str) -> bool {
        self.as_str().eq(other)
    }
}

/// A persistent visual selection range within one file's diff.
#[derive(Clone)]
pub struct VisualRange {
    pub change_id: ChangeId,
    pub path: String,
    pub start_line: DiffLineIdx,
    pub end_line: DiffLineIdx,
}

/// A reference to a file associated with a change_id
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FileRef {
    pub change_id: ChangeId,
    pub path: String,
}

/// A selected item in the DAG. Tied to commit identity (change ID) and file
/// path, so selections survive DAG refreshes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Selection {
    /// Entire file selected.
    File(FileRef),
    /// Individual diff line selected (added or removed).
    Line {
        file_ref: FileRef,
        /// Line number in the old file (`Some` for removed/context lines).
        old_line: Option<u32>,
        /// Line number in the new file (`Some` for added/context lines).
        new_line: Option<u32>,
    },
}

impl Selection {
    /// Get the file reference from any selection variant.
    pub fn file_ref(&self) -> &FileRef {
        match self {
            Selection::File(file_ref) => file_ref,
            Selection::Line {
                file_ref,
                old_line: _,
                new_line: _,
            } => file_ref,
        }
    }

    /// Get the change ID from any selection variant.
    pub fn change_id(&self) -> &ChangeId {
        &self.file_ref().change_id
    }

    /// Get the file path from any selection variant.
    pub fn path(&self) -> &str {
        &self.file_ref().path
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

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SearchScopes: u8 {
        const CHANGE_ID   = 1 << 0;
        const COMMIT_ID   = 1 << 1;
        const DESCRIPTION = 1 << 2;
        const BOOKMARK    = 1 << 3;
        const AUTHOR      = 1 << 4;
        const PATH        = 1 << 5;
        const LINE        = 1 << 6;
    }
}

impl SearchScopes {
    pub const DEFAULT: Self = Self::CHANGE_ID;
}

pub struct SearchScopeSpec {
    pub flag: SearchScopes,
    pub hint: &'static str,
    pub label: &'static str,
}

pub const SEARCH_SCOPE_SPECS: &[SearchScopeSpec] = &[
    SearchScopeSpec {
        flag: SearchScopes::CHANGE_ID,
        hint: "c",
        label: "change-id",
    },
    SearchScopeSpec {
        flag: SearchScopes::COMMIT_ID,
        hint: "i",
        label: "commit-id",
    },
    SearchScopeSpec {
        flag: SearchScopes::DESCRIPTION,
        hint: "d",
        label: "description",
    },
    SearchScopeSpec {
        flag: SearchScopes::BOOKMARK,
        hint: "b",
        label: "bookmark",
    },
    SearchScopeSpec {
        flag: SearchScopes::AUTHOR,
        hint: "a",
        label: "author",
    },
    SearchScopeSpec {
        flag: SearchScopes::PATH,
        hint: "p",
        label: "path",
    },
    SearchScopeSpec {
        flag: SearchScopes::LINE,
        hint: "l",
        label: "line",
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchFocus {
    Query,
    Scopes,
}

pub struct SearchState {
    pub input: Input,
    pub matches: Vec<usize>,
    pub current_match: Option<usize>,
    pub restore_cursor: usize,
    pub scopes: SearchScopes,
    pub focus: SearchFocus,
}

impl SearchState {
    pub fn new(restore_cursor: usize) -> Self {
        Self {
            input: Input::new(String::new()),
            matches: Vec::new(),
            current_match: None,
            restore_cursor,
            scopes: SearchScopes::DEFAULT,
            focus: SearchFocus::Query,
        }
    }

    pub fn query(&self) -> &str {
        self.input.value()
    }
}

/// What to do after selecting an item from a list.
pub enum PendingSelection {
    /// Delete a bookmark on the selected commit.
    BookmarkDelete {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Forget a bookmark on the selected commit.
    BookmarkForget {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Move a bookmark to a target commit (enters TargetSelect after selection).
    BookmarkMove {
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Rename a bookmark (enters TextInput after selection).
    BookmarkRename {
        change_id: ChangeId,
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
        change_id: ChangeId,
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
        change_id: ChangeId,
        flags: CommandFlags,
    },
    /// Set (create or update) a bookmark.
    BookmarkSet {
        change_id: ChangeId,
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
    Squash(SquashKind),
    Rebase(RebaseSource),
    BookmarkMove { bookmark_name: String },
    DuplicateOnto,
}

impl TargetOperation {
    pub fn label(&self) -> &'static str {
        match self {
            TargetOperation::Squash(kind) => match kind {
                SquashKind::Into => "squash into",
                SquashKind::Onto => "squash onto",
                SquashKind::After => "squash after",
                SquashKind::Before => "squash before",
            },
            TargetOperation::Rebase(source) => match source {
                RebaseSource::Revision => "rebase revision",
                RebaseSource::Source => "rebase source",
                RebaseSource::Branch => "rebase branch",
            },
            TargetOperation::BookmarkMove { .. } => "move bookmark",
            TargetOperation::DuplicateOnto => "duplicate onto",
        }
    }

    pub fn follow_up(
        self,
        source: ChangeId,
        target: ChangeId,
        flags: CommandFlags,
        selection: ChangeSelection,
    ) -> Vec<FollowUpOption> {
        match self {
            TargetOperation::Squash(kind) => squash_follow_up(
                source,
                Some(SquashTarget { target, kind }),
                selection,
                flags,
            ),
            TargetOperation::Rebase(source_mode) => {
                rebase_follow_up(source, target, source_mode, flags)
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
    source: ChangeId,
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
    source: ChangeId,
    target: ChangeId,
    source_mode: RebaseSource,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    RebaseKind::iter()
        .map(|kind| FollowUpOption {
            key: kind.key(),
            label: kind.label(),
            action: FollowUpAction::Execute(JJCommand::Rebase {
                change_id: source.clone(),
                source_mode: source_mode.clone(),
                dest: RebaseTarget {
                    target: target.clone(),
                    kind,
                },
                flags,
            }),
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct SquashTarget {
    pub target: ChangeId,
    pub kind: SquashKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SquashKind {
    Into,
    Onto,
    After,
    Before,
}

impl SquashKind {
    pub fn flag(&self) -> &'static str {
        match self {
            SquashKind::Into => "--into",
            SquashKind::Onto => "--onto",
            SquashKind::After => "--insert-after",
            SquashKind::Before => "--insert-before",
        }
    }
}

/// Rebase source mode.
#[derive(Debug, Clone)]
pub enum RebaseSource {
    Revision,
    Source,
    Branch,
}

/// Rebase source mode.
#[derive(Debug, Clone, strum::EnumIter)]
pub enum RebaseKind {
    Onto,
    After,
    Before,
}

impl RebaseKind {
    pub fn key(&self) -> char {
        match self {
            RebaseKind::Onto => 'o',
            RebaseKind::After => 'a',
            RebaseKind::Before => 'b',
        }
    }

    pub fn flag(&self) -> &'static str {
        match self {
            RebaseKind::Onto => "-d",
            RebaseKind::After => "-A",
            RebaseKind::Before => "-B",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            RebaseKind::Onto => "onto",
            RebaseKind::After => "after",
            RebaseKind::Before => "before",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RebaseTarget {
    pub target: ChangeId,
    pub kind: RebaseKind,
}

/// A partially-constructed command that needs a message from the user.
pub enum ReadyCommand {
    Squash {
        source: ChangeId,
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
