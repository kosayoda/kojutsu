use std::collections::{HashMap, HashSet};

use tui_input::Input;

use crate::idx::{EntryIdx, RowIdx};

use crate::keymap::{CommandFlags, KeymapNode};
use crate::types::{
    BookmarkName, ChangeId, CommitId, FollowUpOption, GlobalToggle, OperationId, PendingCommand,
    PendingCommitSelect, PendingSelection, RemoteName, RepoPath, SearchScopes, Str, TagName,
    TargetOperation, VisualRange, WorkspaceName,
};

use super::Loadable;

/// Visual selection state.
pub struct VisualState {
    /// Active visual selection mode (line or commit).
    pub mode: Option<VisualMode>,
    /// Persistent visual range (survives exiting visual mode with `v`).
    pub persistent: Option<PersistentVisualRange>,
}

impl VisualState {
    pub fn new() -> Self {
        Self {
            mode: None,
            persistent: None,
        }
    }
}

// ---------------------------------------------------------------------------
// View-specific state sub-structs
// ---------------------------------------------------------------------------

/// Data loaded from the repo for bookmark/tag/workspace views.
pub struct ViewData {
    /// Aggregated bookmark data for the bookmark view.
    pub bookmark_entries: Vec<BookmarkViewEntry>,
    /// Rich detail data per bookmark (conflict targets, remote tracking).
    pub bookmark_details: HashMap<BookmarkName, crate::dag::BookmarkDetails>,
    /// Bookmarks whose detail rows are collapsed (empty = all expanded).
    pub folded_bookmarks: HashSet<BookmarkName>,
    /// All local tag names (including those outside the current revset).
    pub all_tags: Vec<TagName>,
    /// Aggregated tag data for the tag view.
    pub tag_entries: Vec<TagViewEntry>,
    /// Rich detail data per tag (remote tracking).
    pub tag_details: HashMap<TagName, crate::dag::TagDetails>,
    /// Tags whose detail rows are collapsed (empty = all expanded).
    pub folded_tags: HashSet<TagName>,
    /// Aggregated workspace data for the workspace view.
    pub workspace_entries: Vec<WorkspaceViewEntry>,
    /// All remote bookmarks (tracked and untracked, including off-DAG).
    pub remote_bookmarks: Vec<crate::dag::RemoteBookmarkRef>,
    /// Available git remote names.
    pub remotes: Vec<RemoteName>,
}

impl ViewData {
    pub fn new() -> Self {
        Self {
            bookmark_entries: Vec::new(),
            bookmark_details: HashMap::new(),
            folded_bookmarks: HashSet::new(),
            all_tags: Vec::new(),
            tag_entries: Vec::new(),
            tag_details: HashMap::new(),
            folded_tags: HashSet::new(),
            workspace_entries: Vec::new(),
            remote_bookmarks: Vec::new(),
            remotes: Vec::new(),
        }
    }
}

/// Revset configuration and loading state.
pub struct RevsetConfig {
    /// The current revset expression.
    pub current: Str,
    /// Last failed revset attempt (pre-fills the input on retry).
    pub draft: Option<Str>,
    /// Current revset load status.
    pub load_state: Loadable<()>,
    /// Revset currently being requested, if any.
    pub pending: Option<Str>,
    /// Active preset index (into `presets`), or `None` for jj default / manual revset.
    pub active_preset: Option<usize>,
    /// Named revset presets from config.
    pub presets: &'static [crate::theme::Preset],
}

/// State for the operation log view.
pub struct OpLogState {
    pub entries: Vec<OpLogEntry>,
    pub loaded: bool,
    pub has_more: bool,
    pub limit: usize,
    pub workspace_filter: HashSet<WorkspaceName>,
    pub unfolded: HashSet<OperationId>,
    pub details: HashMap<OperationId, Loadable<Vec<OpDetailLine>>>,
}

impl OpLogState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            loaded: false,
            has_more: false,
            limit: super::OP_LOG_BATCH_SIZE,
            workspace_filter: HashSet::from([WorkspaceName::new("default")]),
            unfolded: HashSet::new(),
            details: HashMap::new(),
        }
    }
}

/// State for the evolution log view.
pub struct EvoLogState {
    pub entries: Vec<EvoLogEntry>,
    pub loaded: bool,
    pub commit_id: Option<CommitId>,
    pub unfolded: HashSet<CommitId>,
    pub files: HashMap<CommitId, Loadable<Vec<crate::dag::FileChange>>>,
    pub unfolded_files: HashSet<(CommitId, RepoPath)>,
    pub file_diffs: HashMap<(CommitId, RepoPath), Loadable<Vec<crate::dag::DiffLine>>>,
    pub file_diffs_cw: HashMap<(CommitId, RepoPath), Loadable<Vec<crate::dag::DiffLine>>>,
}

impl EvoLogState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            loaded: false,
            commit_id: None,
            unfolded: HashSet::new(),
            files: HashMap::new(),
            unfolded_files: HashSet::new(),
            file_diffs: HashMap::new(),
            file_diffs_cw: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.loaded = false;
        self.unfolded.clear();
        self.files.clear();
        self.unfolded_files.clear();
        self.file_diffs.clear();
        self.file_diffs_cw.clear();
    }
}

/// State for the command log view.
pub struct CommandLogState {
    pub entries: Vec<CommandLogEntry>,
    pub unfolded: HashSet<usize>,
}

impl CommandLogState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            unfolded: HashSet::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandLogKind {
    Command,
    Background,
    Warning,
}

pub struct CommandLogEntry {
    pub kind: CommandLogKind,
    pub summary: String,
    /// Structured command parts for syntax highlighting (None for non-command entries).
    pub command_parts: Option<Vec<crate::jj_command::CommandPart>>,
    pub output: Vec<u8>,
    pub success: bool,
    pub timestamp: jiff::Timestamp,
}

/// Result of picking a conflict side for a hunk.
pub enum ConflictPickResult {
    /// Hunk picked, but other hunks in the file are still unresolved.
    Pending,
    /// All hunks resolved — caller should write content to path and refresh.
    FileResolved {
        path: crate::types::RepoPath,
        content: String,
    },
}

/// Whether the target-select picker allows one or many targets.
#[derive(Debug, Clone)]
pub enum TargetMode {
    Single,
    Multi { targets: HashSet<ChangeId> },
}

// ---------------------------------------------------------------------------
// Persisted state -- saved to ~/.local/state/kojutsu/state.json across app restarts.
// ---------------------------------------------------------------------------

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PersistedState {
    pub show_line_numbers: bool,
    pub ignore_immutable: bool,
    pub ignore_working_copy: bool,
    pub debug: bool,
    /// Per-view search scope bitflags, indexed by `ActiveView::idx()`.
    #[serde(default, alias = "search_scopes")]
    pub view_search_scopes: [u8; <ActiveView as strum::EnumCount>::COUNT],
    pub active_preset: Option<usize>,
    #[serde(default)]
    pub git_diff: bool,
}

pub fn load_persisted_state() -> PersistedState {
    let Some(path) = crate::theme::state_path() else {
        return PersistedState::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => PersistedState::default(),
    }
}

pub fn save_persisted_state(state: &PersistedState) {
    let Some(path) = crate::theme::state_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&path, json);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Error,
}

/// Number of operations to load per batch in the op log view.
pub const OP_LOG_BATCH_SIZE: usize = 200;

#[derive(
    Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, strum::EnumCount,
)]
pub enum ActiveView {
    #[default]
    Dag,
    Bookmarks,
    Tags,
    Operations,
    Workspaces,
    Evolog,
    CommandLog,
}

impl ActiveView {
    pub(super) const fn idx(self) -> usize {
        self as usize
    }

    const fn default_scopes(self) -> SearchScopes {
        match self {
            Self::Dag => SearchScopes::DEFAULT,
            Self::Bookmarks => SearchScopes::DEFAULT_BOOKMARK,
            Self::Tags => SearchScopes::DEFAULT_TAG,
            Self::Operations => SearchScopes::DEFAULT_OP_LOG,
            Self::Workspaces => SearchScopes::DEFAULT,
            Self::Evolog => SearchScopes::DEFAULT,
            Self::CommandLog => SearchScopes::DEFAULT,
        }
    }
}

/// Saved per-view state (cursor position, scroll, horizontal scroll, search scopes).
#[derive(Clone)]
pub struct ViewState {
    pub cursor: RowIdx,
    pub scroll_offset: usize,
    pub h_scroll: usize,
    pub search_scopes: SearchScopes,
}

impl ViewState {
    const fn new(scopes: SearchScopes) -> Self {
        Self {
            cursor: RowIdx::new(0),
            scroll_offset: 0,
            h_scroll: 0,
            search_scopes: scopes,
        }
    }
}

pub(super) fn default_view_states() -> [ViewState; <ActiveView as strum::EnumCount>::COUNT] {
    [
        ViewState::new(ActiveView::Dag.default_scopes()),
        ViewState::new(ActiveView::Bookmarks.default_scopes()),
        ViewState::new(ActiveView::Tags.default_scopes()),
        ViewState::new(ActiveView::Operations.default_scopes()),
        ViewState::new(ActiveView::Workspaces.default_scopes()),
        ViewState::new(ActiveView::Evolog.default_scopes()),
        ViewState::new(ActiveView::CommandLog.default_scopes()),
    ]
}

pub struct BookmarkViewEntry {
    pub name: BookmarkName,
    pub commit_id: Option<CommitId>,
    pub change_id: Option<crate::dag::ShortId>,
    pub description: Option<String>,
    pub is_tracked: bool,
    /// Whether this local bookmark tracks a remote (e.g., `main` tracks `main@origin`).
    pub is_tracking: bool,
    pub is_synced: bool,
    pub is_dirty: bool,
    pub remote: Option<RemoteName>,
    /// Whether the bookmark has conflicting targets.
    pub is_conflicted: bool,
}

pub struct TagViewEntry {
    pub name: TagName,
    pub commit_id: Option<CommitId>,
    pub change_id: Option<crate::dag::ShortId>,
    pub description: Option<String>,
    /// Whether the local tag has been deleted (only remote refs remain).
    pub is_deleted: bool,
}

pub enum OpDiffKind {
    Added,
    Removed,
}

pub struct OpDiffCommit {
    pub change_id: crate::dag::ShortId,
    pub commit_id: crate::dag::ShortId,
    pub description: Option<String>,
    pub kind: OpDiffKind,
}

pub struct OpDiffWorkingCopy {
    pub workspace: WorkspaceName,
    pub new_commit: Option<crate::dag::ShortId>,
    pub old_commit: Option<crate::dag::ShortId>,
}

pub struct OpDiffBookmark {
    pub name: Str,
    pub new_target: Option<crate::dag::ShortId>,
    pub old_target: Option<crate::dag::ShortId>,
}

pub enum OpDetailLine {
    SectionHeader(Str),
    Commit(OpDiffCommit),
    WorkingCopy(OpDiffWorkingCopy),
    Bookmark(OpDiffBookmark),
}

pub struct OpLogEntry {
    /// Hex operation ID (truncated for display).
    pub id: OperationId,
    /// Human-readable operation description.
    pub description: Str,
    /// Relative time string (e.g. "5 hours ago").
    pub relative_time: Str,
    /// Workspace name that ran this operation.
    pub workspace: Option<WorkspaceName>,
    /// "user@host" who performed the operation.
    pub user: Str,
    /// The CLI args that produced this operation (from metadata tags).
    pub args: Option<Str>,
    /// Whether this is a pure working-copy snapshot.
    pub is_snapshot: bool,
    /// Whether this is the repo's current operation.
    pub is_current: bool,
    /// Pre-rendered graph lines.
    pub graph: crate::graph::GraphLines,
}

pub struct EvoLogEntry {
    /// Full hex commit ID.
    pub commit_id: CommitId,
    /// Short change ID with unique prefix length.
    pub change_id: crate::dag::ShortId,
    /// First line of commit description.
    pub description: Option<String>,
    /// Author name/email.
    pub author: Str,
    /// Relative time string (e.g. "5 hours ago").
    pub relative_time: Str,
    /// Description of the operation that produced this version.
    pub op_description: Option<Str>,
    /// Whether this is the newest (current) version.
    pub is_current: bool,
    /// Predecessor commit IDs (the version(s) this was rewritten from).
    pub predecessor_ids: Vec<CommitId>,
    /// Pre-rendered graph lines.
    pub graph: crate::graph::GraphLines,
}

pub struct WorkspaceViewEntry {
    pub name: WorkspaceName,
    pub commit_id: Option<CommitId>,
    pub change_id: Option<crate::dag::ShortId>,
    pub description: Option<String>,
    pub is_current: bool,
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

/// Active visual selection mode.
pub enum VisualMode {
    /// Visual selection of diff lines within one file.
    Lines {
        /// Row index where `v` was pressed.
        anchor: RowIdx,
    },
    /// Visual selection of commits along a branch.
    Commits {
        /// Entry where `v` was pressed.
        anchor: EntryIdx,
        /// Ordered path from newest to oldest along Direct edges (inclusive).
        path: Vec<EntryIdx>,
    },
}

/// Persistent visual range (survives exiting visual mode with `v`).
pub enum PersistentVisualRange {
    Lines(VisualRange),
    Commits(Vec<EntryIdx>),
}

/// Where to jump the cursor after the next DAG refresh.
pub enum JumpTarget {
    /// Jump to the working copy commit (@).
    WorkingCopy,
    /// Jump to the commit that has this local bookmark.
    Bookmark(BookmarkName),
    /// Jump to a commit by change ID prefix.
    ChangeId(String),
}

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
        /// Structured command parts for syntax highlighting.
        command_parts: Option<Vec<crate::jj_command::CommandPart>>,
        /// Raw stdout+stderr bytes (may contain ANSI color codes).
        output: Vec<u8>,
        /// Whether the command succeeded.
        success: bool,
        /// Follow-up options offered when the overlay is dismissed (e.g. retry
        /// with `--ignore-immutable`). Empty means no retry available.
        retry: Vec<crate::types::FollowUpOption>,
    },
    /// Help overlay showing all keybindings.
    Help { scroll: u16 },
    /// Single-line text input in the bottom bar.
    TextInput {
        prompt: String,
        input: Input,
        on_submit: PendingCommand,
    },
    /// Live search input at the bottom bar.
    SearchInput,
    /// Navigating to select a target commit for a two-commit operation.
    TargetSelect {
        prompt: &'static str,
        source: ChangeId,
        restore_cursor: RowIdx,
        operation: TargetOperation,
        flags: CommandFlags,
        target_mode: TargetMode,
    },
    /// Navigating to select a single commit (e.g. for workspace revision).
    CommitSelect {
        restore_cursor: RowIdx,
        pending: PendingCommitSelect,
        flags: CommandFlags,
    },
    /// Choosing from a set of follow-up options after target selection.
    FollowUp {
        prompt: String,
        options: Vec<FollowUpOption>,
    },
    /// Jump mode: labels visible on jumpable rows, type label chars to jump.
    Jump {
        /// (label_string, row_index) for each visible jumpable row.
        labels: Vec<(String, RowIdx)>,
        /// Characters typed so far (for multi-char label matching).
        input: String,
        /// Mode to restore on exit (e.g. TargetSelect). None → Normal.
        restore_mode: Option<Box<AppMode>>,
    },
    /// Selecting an item from a list (e.g. picking a bookmark).
    SelectFromList {
        title: String,
        items: Vec<String>,
        /// Indices into `items` that match the current filter.
        filtered_indices: Vec<usize>,
        /// Cursor position within `filtered_indices`.
        cursor: usize,
        /// Scroll offset for the list viewport.
        scroll_offset: usize,
        /// Indices of toggled items in original `items` (multiselect only).
        marked: HashSet<usize>,
        /// Whether multiselect is enabled.
        multi: bool,
        /// Current filter text (empty = no filter active).
        filter: String,
        /// Whether the filter input is focused.
        filtering: bool,
        on_select: PendingSelection,
    },
}

impl AppMode {
    /// Construct a `TextInput` mode with the given prompt, prefill, and submit handler.
    pub fn text_input(
        prompt: impl Into<String>,
        prefill: impl Into<String>,
        on_submit: PendingCommand,
    ) -> Self {
        AppMode::TextInput {
            prompt: prompt.into(),
            input: Input::new(prefill.into()),
            on_submit,
        }
    }

    /// Construct a `SelectFromList` mode with default cursor/filter state.
    pub fn select_from_list(
        title: impl Into<String>,
        items: Vec<String>,
        multi: bool,
        on_select: PendingSelection,
        focus_filter: bool,
    ) -> Self {
        let filtered_indices = (0..items.len()).collect();
        AppMode::SelectFromList {
            title: title.into(),
            items,
            filtered_indices,
            cursor: 0,
            scroll_offset: 0,
            marked: HashSet::new(),
            multi,
            filter: String::new(),
            filtering: focus_filter,
            on_select,
        }
    }
}
