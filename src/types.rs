use strum::{EnumDiscriminants, IntoEnumIterator as _};
use tui_input::Input;

use crate::{
    idx::{DiffLineIdx, EntryIdx, FileIdx, GraphLineIdx},
    jj_command::{ChangeSelection, JJCommand},
    keymap::{CommandFlags, SelectionKindSet},
    pluralize,
};

pub type Str = compact_str::CompactString;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ChangeId(Str);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CommitId(Str);

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

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl CommitId {
    pub fn new(s: impl Into<Str>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq<String> for CommitId {
    fn eq(&self, other: &String) -> bool {
        self.as_str().eq(other.as_str())
    }
}

impl PartialEq<str> for CommitId {
    fn eq(&self, other: &str) -> bool {
        self.as_str().eq(other)
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
#[derive(Clone, PartialEq, Eq, Hash, EnumDiscriminants)]
#[strum_discriminants(name(SelectionKind))]
pub enum Selection {
    /// Commit selected (used implicitly from cursor, not currently in explicit sets).
    Commit(ChangeId),
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

pub struct SelectionSummary {
    pub file_count: usize,
    pub full_file_count: usize,
    pub line_count: usize,
    pub has_full_files: bool,
}

impl SelectionSummary {
    pub fn empty() -> Self {
        Self {
            file_count: 0,
            full_file_count: 0,
            line_count: 0,
            has_full_files: false,
        }
    }

    pub fn display_text(&self) -> Option<String> {
        self.format_summary(" selected")
    }

    pub fn submenu_suffix(&self) -> Option<String> {
        self.format_summary("")
    }

    fn format_summary(&self, suffix: &str) -> Option<String> {
        let file_noun = pluralize!(self.full_file_count, "file", "files");
        let line_noun = pluralize!(self.line_count, "line", "lines");

        if self.file_count == 0 && self.line_count == 0 {
            return None;
        }

        if self.line_count == 0 {
            return Some(format!(
                "{} {}{suffix}",
                self.full_file_count, file_noun
            ));
        }

        if !self.has_full_files && self.file_count == 1 {
            return Some(format!("{} {}{suffix}", self.line_count, line_noun));
        }

        if self.has_full_files && self.line_count > 0 {
            return Some(format!(
                "{} {} + {} {}{suffix}",
                self.full_file_count, file_noun, self.line_count, line_noun
            ));
        }

        Some(format!(
            "{} {} in {} {}{suffix}",
            self.line_count, line_noun, self.file_count, file_noun
        ))
    }
}

impl SelectionKind {
    pub fn as_bitset(&self) -> SelectionKindSet {
        match self {
            SelectionKind::Commit => SelectionKindSet::COMMIT,
            SelectionKind::File => SelectionKindSet::FILE,
            SelectionKind::Line => SelectionKindSet::LINE,
        }
    }
}

pub struct SelectionContext {
    explicit: std::collections::HashSet<Selection>,
    summary: SelectionSummary,
}

impl SelectionContext {
    pub fn new() -> Self {
        Self {
            explicit: std::collections::HashSet::new(),
            summary: SelectionSummary::empty(),
        }
    }

    pub fn is_active(&self) -> bool {
        !self.explicit.is_empty()
    }

    pub fn kind(&self) -> SelectionKind {
        self.explicit
            .iter()
            .next()
            .map(SelectionKind::from)
            .unwrap_or(SelectionKind::Commit)
    }

    pub fn summary(&self) -> &SelectionSummary {
        &self.summary
    }

    pub fn explicit(&self) -> Option<&std::collections::HashSet<Selection>> {
        if self.explicit.is_empty() {
            None
        } else {
            Some(&self.explicit)
        }
    }

    pub fn display_text(&self) -> Option<String> {
        self.summary.display_text()
    }

    pub fn submenu_suffix(&self) -> Option<String> {
        self.summary.submenu_suffix()
    }

    pub fn clear(&mut self) {
        self.explicit.clear();
        self.summary = SelectionSummary::empty();
    }

    pub fn len(&self) -> usize {
        self.explicit.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> Box<dyn Iterator<Item = &Selection> + '_> {
        Box::new(self.explicit.iter())
    }

    pub fn contains(&self, selection: &Selection) -> bool {
        self.explicit.contains(selection)
    }

    pub fn remove(&mut self, selection: &Selection) -> bool {
        let removed = self.explicit.remove(selection);
        if removed {
            self.recompute_summary();
        }
        removed
    }

    pub fn retain(&mut self, f: impl FnMut(&Selection) -> bool) {
        self.explicit.retain(f);
        self.recompute_summary();
    }

    pub fn any(&self, f: impl FnMut(&Selection) -> bool) -> bool {
        self.iter().any(f)
    }

    pub fn ensure_kind(&mut self, kind: SelectionKind) {
        let reset = self.is_active() && self.kind() != kind;
        if reset {
            self.clear();
        }
    }

    pub fn insert(&mut self, kind: SelectionKind, selection: Selection) {
        self.ensure_kind(kind);
        if self.explicit.insert(selection) {
            self.recompute_summary();
        }
    }

    pub fn toggle(&mut self, kind: SelectionKind, selection: Selection) {
        if !self.remove(&selection) {
            self.insert(kind, selection);
        }
    }

    fn recompute_summary(&mut self) {
        let mut files = std::collections::HashSet::new();
        let mut full_files = std::collections::HashSet::new();
        let mut line_count = 0usize;
        let mut has_full_files = false;

        for selection in &self.explicit {
            files.insert(selection.path().to_string());
            match selection {
                Selection::Commit(_) => {}
                Selection::File(file_ref) => {
                    has_full_files = true;
                    full_files.insert(file_ref.path.clone());
                }
                Selection::Line { .. } => line_count += 1,
            }
        }

        self.summary = SelectionSummary {
            file_count: files.len(),
            full_file_count: full_files.len(),
            line_count,
            has_full_files,
        };
    }
}

impl Default for SelectionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Selection {
    /// Get the file reference from any selection variant.
    pub fn file_ref(&self) -> &FileRef {
        match self {
            Selection::Commit(_) => panic!("commit selection has no file_ref"),
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
        match self {
            Selection::Commit(change_id) => change_id,
            _ => &self.file_ref().change_id,
        }
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
    /// Forget a workspace (text is the workspace name from the list).
    WorkspaceForget { flags: CommandFlags },
}

/// What to do after selecting a single commit in CommitSelect mode.
pub enum PendingCommitSelect {
    WorkspaceAdd {
        path: String,
        name: Option<String>,
    },
}

impl PendingCommitSelect {
    pub fn prompt(&self) -> &'static str {
        match self {
            PendingCommitSelect::WorkspaceAdd { .. } => "workspace revision",
        }
    }

    pub fn into_jj_command(self, target: ChangeId, flags: CommandFlags) -> JJCommand {
        match self {
            PendingCommitSelect::WorkspaceAdd { path, name } => JJCommand::WorkspaceAdd {
                path,
                name,
                revision: target,
                flags,
            },
        }
    }
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
    /// Workspace add step 1: collecting path. Text = path.
    WorkspaceAddPath { flags: CommandFlags },
    /// Workspace add step 2: path collected, collecting name. Text = name.
    WorkspaceAddName {
        path: String,
        flags: CommandFlags,
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
            PendingCommand::WorkspaceAddPath { .. }
            | PendingCommand::WorkspaceAddName { .. } => {
                panic!("Workspace add steps handled separately in handle_text_input")
            }
        }
    }
}

/// What kind of two-commit target selection we're doing.
#[derive(Debug, Clone)]
pub enum TargetOperation {
    Squash(SquashKind),
    Split(SplitKind),
    Rebase(RebaseSource),
    RestoreFrom,
    RestoreInto,
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
            TargetOperation::Split(kind) => match kind {
                SplitKind::Onto => "split onto",
                SplitKind::After => "split after",
                SplitKind::Before => "split before",
            },
            TargetOperation::Rebase(source) => match source {
                RebaseSource::Revision => "rebase revision",
                RebaseSource::Source => "rebase source",
                RebaseSource::Branch => "rebase branch",
            },
            TargetOperation::RestoreFrom => "restore from",
            TargetOperation::RestoreInto => "restore into",
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
            TargetOperation::Split(kind) => vec![FollowUpOption {
                key: ' ',
                label: "split",
                action: FollowUpAction::Execute(JJCommand::Split {
                    change_id: source,
                    target: Some(SplitTarget { target, kind }),
                    selection,
                    flags,
                }),
            }],
            TargetOperation::Rebase(source_mode) => {
                rebase_follow_up(source, target, source_mode, flags)
            }
            TargetOperation::RestoreFrom => vec![FollowUpOption {
                key: ' ',
                label: "restore",
                action: FollowUpAction::Execute(JJCommand::Restore {
                    from: Some(target),
                    into: None,
                    changes_in: None,
                    selection,
                    flags,
                }),
            }],
            TargetOperation::RestoreInto => vec![FollowUpOption {
                key: ' ',
                label: "restore",
                action: FollowUpAction::Execute(JJCommand::Restore {
                    from: None,
                    into: Some(target),
                    changes_in: None,
                    selection,
                    flags,
                }),
            }],
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

#[derive(Debug, Clone)]
pub struct SplitTarget {
    pub target: ChangeId,
    pub kind: SplitKind,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitKind {
    Onto,
    After,
    Before,
}

impl SplitKind {
    pub fn flag(&self) -> &'static str {
        match self {
            SplitKind::Onto => "--onto",
            SplitKind::After => "--insert-after",
            SplitKind::Before => "--insert-before",
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
