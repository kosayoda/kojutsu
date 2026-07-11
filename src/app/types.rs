use std::collections::{HashMap, HashSet};

use tui_input::Input;

use crate::idx::{EntryIdx, RowIdx};

use std::sync::Arc;

use compact_str::CompactString;

use crate::keymap::{CommandFlags, TrieNode};
use crate::types::{
    BookmarkName, ChangeId, CommitId, FollowUpOption, OperationId, PendingCommand,
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
    pub file_diffs: HashMap<(CommitId, RepoPath), Loadable<crate::dag::DiffResult>>,
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
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.loaded = false;
        self.unfolded.clear();
        self.files.clear();
        self.unfolded_files.clear();
        self.file_diffs.clear();
    }
}

/// State for the command log view.
pub struct CommandLogState {
    pub entries: Vec<CommandLogEntry>,
    pub unfolded: HashSet<crate::idx::CommandLogIdx>,
}

impl CommandLogState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            unfolded: HashSet::new(),
        }
    }
}

/// The two commits being compared in the interdiff view.
pub struct InterdiffTarget {
    pub from_commit_id: CommitId,
    pub to_commit_id: CommitId,
    pub from_label: Str,
    pub to_label: Str,
}

/// State for the interdiff view.
pub struct InterdiffState {
    pub target: Option<InterdiffTarget>,
    pub files: Loadable<Vec<crate::dag::FileChange>>,
    pub unfolded_files: HashSet<RepoPath>,
    pub file_diffs: HashMap<RepoPath, Loadable<crate::dag::DiffResult>>,
}

impl InterdiffState {
    pub fn new() -> Self {
        Self {
            target: None,
            files: Loadable::NotRequested,
            unfolded_files: HashSet::new(),
            file_diffs: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Whether the current target matches the given from/to commit pair.
    pub fn target_is(&self, from: &CommitId, to: &CommitId) -> bool {
        self.target
            .as_ref()
            .is_some_and(|t| t.from_commit_id == *from && t.to_commit_id == *to)
    }
}

/// The commit + path being annotated.
pub struct AnnotateTarget {
    pub commit_id: CommitId,
    pub path: RepoPath,
}

/// State for the annotate (blame) view.
pub struct AnnotateState {
    /// What is being annotated (None when view is inactive/cleared).
    pub target: Option<AnnotateTarget>,
    /// Annotation lines.
    pub lines: Loadable<Vec<crate::dag::AnnotateLineData>>,
    /// Per-commit detail metadata for expansion.
    pub commit_info: HashMap<CommitId, crate::dag::AnnotateCommitInfo>,
    /// Line indices that are currently unfolded (showing details).
    pub unfolded_lines: HashSet<crate::idx::AnnotateLineIdx>,
    /// After reload, jump cursor to this 1-based line number.
    pub target_line: Option<usize>,
    /// Time-travel history stack: (commit_id, line_number) for backtracking with `f`.
    pub history: Vec<(CommitId, usize)>,
    /// Show separator lines between groups of lines from different commits.
    pub show_commit_separators: bool,
}

impl AnnotateState {
    pub fn new() -> Self {
        Self {
            target: None,
            lines: Loadable::NotRequested,
            commit_info: HashMap::new(),
            unfolded_lines: HashSet::new(),
            target_line: None,
            history: Vec::new(),
            show_commit_separators: false,
        }
    }

    /// Clear state for a fresh annotation, but preserve history stack and
    /// commit_info (needed for breadcrumb rendering of past commits).
    pub fn clear_keep_history(&mut self) {
        let history = std::mem::take(&mut self.history);
        let commit_info = std::mem::take(&mut self.commit_info);
        let show_commit_separators = self.show_commit_separators;
        *self = Self::new();
        self.history = history;
        self.commit_info = commit_info;
        self.show_commit_separators = show_commit_separators;
    }

    pub fn clear(&mut self) {
        let show_commit_separators = self.show_commit_separators;
        *self = Self::new();
        self.show_commit_separators = show_commit_separators;
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
    /// ANSI-parsed output lines, cached to avoid re-parsing every frame.
    pub parsed_lines: Vec<ratatui::text::Line<'static>>,
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

pub struct SubmenuToggle {
    pub node: keymap_parser::Node,
    pub flag: CommandFlags,
    pub description: CompactString,
}

/// Parse ANSI-colored bytes into styled lines, falling back to plain text
/// on malformed escape sequences.
pub fn parse_ansi_lines(output: &[u8]) -> Vec<ratatui::text::Line<'static>> {
    use ansi_to_tui::IntoText as _;
    match output.into_text() {
        Ok(text) => text.lines,
        Err(_) => String::from_utf8_lossy(output)
            .lines()
            .map(|line| ratatui::text::Line::from(line.to_string()))
            .collect(),
    }
}

/// State for the command output overlay.
pub struct CommandOutputState {
    /// The command that was run, e.g. `"$ jj abandon xvzwolmw"`.
    pub command: String,
    /// Structured command parts for syntax highlighting.
    pub command_parts: Option<Vec<crate::jj_command::CommandPart>>,
    /// Raw stdout+stderr bytes (kept for consumers that need the bytes,
    /// e.g. Lua post-hooks).
    pub output: Vec<u8>,
    /// ANSI-parsed output lines; parsed once at construction, refreshed via
    /// [`Self::reparse`] after `output` is mutated.
    pub parsed_lines: Vec<ratatui::text::Line<'static>>,
    /// Whether the command succeeded.
    pub success: bool,
    /// Follow-up options offered when the overlay is dismissed (e.g. retry
    /// with `--ignore-immutable`). Empty means no retry available.
    pub retry: Vec<crate::types::FollowUpOption>,
    /// Scroll offset in lines from the top; clamped during draw.
    pub scroll: u16,
}

impl CommandOutputState {
    pub fn new(
        command: String,
        command_parts: Option<Vec<crate::jj_command::CommandPart>>,
        output: Vec<u8>,
        success: bool,
        retry: Vec<crate::types::FollowUpOption>,
    ) -> Self {
        let parsed_lines = parse_ansi_lines(&output);
        Self {
            command,
            command_parts,
            output,
            parsed_lines,
            success,
            retry,
            scroll: 0,
        }
    }

    /// Re-parse `parsed_lines` after mutating `output`.
    pub fn reparse(&mut self) {
        self.parsed_lines = parse_ansi_lines(&self.output);
    }

    /// Total displayed lines: the command line plus the parsed output.
    pub fn line_count(&self) -> usize {
        1 + self.parsed_lines.len()
    }

    /// Overlay height for the given overlay-base height (content plus the
    /// top border, capped at half the base).
    pub fn overlay_height(&self, base_height: u16) -> u16 {
        (self.parsed_lines.len().max(1) as u16 + 3)
            .min(base_height / 2)
            .max(3)
    }

    /// How far the content can scroll at the given overlay-base height
    /// (0 = fits entirely; the overlay has a top border, so one row of the
    /// overlay is not content).
    pub fn max_scroll(&self, base_height: u16) -> u16 {
        let visible = self.overlay_height(base_height).saturating_sub(1);
        (self.line_count() as u16).saturating_sub(visible)
    }
}

/// State for a background jj command with live output streaming.
pub struct CommandRunningState {
    pub command: String,
    pub command_parts: Vec<crate::jj_command::CommandPart>,
    pub kill: crate::jj_command::KillHandle,
    /// Raw stdout+stderr bytes accumulated so far (chronologically interleaved).
    pub output: Vec<u8>,
    /// ANSI-parsed complete lines, cached to avoid re-parsing every frame.
    pub parsed_lines: Vec<ratatui::text::Line<'static>>,
    /// Byte offset into `output` up to which lines have been parsed
    /// (always just past a newline).
    pub parsed_upto: usize,
    /// Scroll offset in lines from the bottom; 0 follows the tail as
    /// output arrives. Clamped against content during draw.
    pub scroll_from_bottom: usize,
}

impl CommandRunningState {
    pub fn new(
        command: String,
        command_parts: Vec<crate::jj_command::CommandPart>,
        kill: crate::jj_command::KillHandle,
    ) -> Self {
        Self {
            command,
            command_parts,
            kill,
            output: Vec::new(),
            parsed_lines: Vec::new(),
            parsed_upto: 0,
            scroll_from_bottom: 0,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
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
    pub git_diff: bool,
    pub annotate_separators: bool,
    pub bookmark_separators: bool,
    pub diff_underline: bool,
    /// Previously run `jj run` commands, most recent first.
    pub run_history: Vec<String>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            show_line_numbers: false,
            ignore_immutable: false,
            ignore_working_copy: false,
            debug: false,
            view_search_scopes: [0; <ActiveView as strum::EnumCount>::COUNT],
            active_preset: None,
            git_diff: false,
            annotate_separators: false,
            bookmark_separators: false,
            diff_underline: true,
            run_history: Vec::new(),
        }
    }
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
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    strum::EnumCount,
    strum::FromRepr,
    strum::Display,
    strum::EnumString,
)]
#[repr(usize)]
#[strum(serialize_all = "snake_case")]
pub enum ActiveView {
    #[default]
    Dag,
    Bookmarks,
    Tags,
    Operations,
    Workspaces,
    Evolog,
    CommandLog,
    Interdiff,
    Annotate,
}

impl ActiveView {
    pub const fn idx(self) -> usize {
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
            Self::Interdiff => SearchScopes::DEFAULT,
            Self::Annotate => SearchScopes::DEFAULT_ANNOTATE,
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
    std::array::from_fn(|i| ViewState::new(ActiveView::from_repr(i).unwrap().default_scopes()))
}

/// Discriminant for the 4 mutually exclusive bookmark states.
pub enum BookmarkKind {
    /// Pure local bookmark (no remote tracking).
    Local { is_dirty: bool, is_conflicted: bool },
    /// Local bookmark that tracks a remote.
    Tracking { is_dirty: bool, is_conflicted: bool },
    /// Tracked remote-only bookmark.
    TrackedRemote { remote: RemoteName },
    /// Untracked remote-only bookmark.
    UntrackedRemote { remote: RemoteName },
}

impl BookmarkKind {
    /// The remote name, if this is a remote bookmark.
    pub fn remote(&self) -> Option<&RemoteName> {
        match self {
            Self::TrackedRemote { remote } | Self::UntrackedRemote { remote } => Some(remote),
            _ => None,
        }
    }

    /// Whether this bookmark has conflicting targets.
    pub fn is_conflicted(&self) -> bool {
        matches!(
            self,
            Self::Local {
                is_conflicted: true,
                ..
            } | Self::Tracking {
                is_conflicted: true,
                ..
            }
        )
    }

    /// Whether the local bookmark differs from its tracked remote.
    pub fn is_dirty(&self) -> bool {
        matches!(
            self,
            Self::Local { is_dirty: true, .. } | Self::Tracking { is_dirty: true, .. }
        )
    }

    /// Sort rank: local(0) < tracking(1) < tracked-remote(2) < untracked-remote(3).
    pub fn rank(&self) -> u8 {
        match self {
            Self::Local { .. } => 0,
            Self::Tracking { .. } => 1,
            Self::TrackedRemote { .. } => 2,
            Self::UntrackedRemote { .. } => 3,
        }
    }
}

pub struct BookmarkViewEntry {
    pub name: BookmarkName,
    pub commit_id: Option<CommitId>,
    pub change_id: Option<crate::dag::ShortId>,
    pub short_commit_id: Option<crate::dag::ShortId>,
    pub description: Option<String>,
    pub kind: BookmarkKind,
}

pub struct TagViewEntry {
    pub name: TagName,
    pub commit_id: Option<CommitId>,
    pub change_id: Option<crate::dag::ShortId>,
    pub short_commit_id: Option<crate::dag::ShortId>,
    pub description: Option<String>,
    /// Whether the local tag has been deleted (only remote refs remain).
    pub is_deleted: bool,
}

pub struct OpDiffCommit {
    pub change_id: crate::dag::ShortId,
    pub commit_id: crate::dag::ShortId,
    pub description: Option<String>,
    pub kind: crate::dag::DiffKind,
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
    /// Visual selection of files within a single commit.
    Files {
        /// Row index where `v` was pressed.
        anchor: RowIdx,
        /// Which commit the anchor belongs to (single-commit constraint).
        entry_idx: EntryIdx,
    },
}

/// Persistent visual range (survives exiting visual mode with `v`).
pub enum PersistentVisualRange {
    Lines(VisualRange),
    Commits(Vec<EntryIdx>),
    /// A contiguous range of files within one commit.
    Files {
        entry_idx: EntryIdx,
        lo: crate::idx::FileIdx,
        hi: crate::idx::FileIdx,
    },
}

/// State for the select-from-list overlay (e.g. picking a bookmark).
pub struct SelectFromListState {
    pub title: String,
    pub items: Vec<String>,
    pub filtered_indices: Vec<usize>,
    /// Matched character positions for each entry in `filtered_indices`.
    pub match_positions: Vec<Vec<usize>>,
    pub cursor: usize,
    pub scroll_offset: usize,
    pub marked: HashSet<usize>,
    pub multi: bool,
    pub filter: String,
    pub filtering: bool,
    /// When present, `items[0]` is a free-input affordance rather than data:
    /// it is pinned to the top through filtering, cannot be marked, and
    /// selecting it opens the text input this payload describes instead of
    /// resolving to an item.
    pub custom_entry: Option<CustomEntry>,
    pub on_select: PendingSelection,
}

/// The free-input affordance of a select list: what selecting the pinned
/// custom row does. Carrying the prompt and submit handler here makes the
/// row's behavior total — a list cannot be built with a custom row that
/// does nothing.
pub struct CustomEntry {
    /// Prompt for the text input (e.g. `"run: "`).
    pub prompt: String,
    /// Submit handler for the text input.
    pub on_submit: PendingCommand,
}

impl SelectFromListState {
    pub fn new(
        title: impl Into<String>,
        items: Vec<String>,
        multi: bool,
        on_select: PendingSelection,
        focus_filter: bool,
    ) -> Self {
        let count = items.len();
        Self {
            title: title.into(),
            items,
            filtered_indices: (0..count).collect(),
            match_positions: vec![vec![]; count],
            cursor: 0,
            scroll_offset: 0,
            marked: HashSet::new(),
            multi,
            filter: String::new(),
            filtering: focus_filter,
            custom_entry: None,
            on_select,
        }
    }
}

/// The current interaction mode.
pub enum AppMode {
    /// Normal browsing.
    Normal,
    /// A prefix key was pressed; showing submenu options in the bottom bar.
    Submenu {
        /// Display string for the prefix key (e.g., "s", "b", "g").
        key: String,
        label: CompactString,
        children: Arc<[(keymap_parser::Node, TrieNode)]>,
        flags: CommandFlags,
    },
    /// Showing the result of a shell command. Scrolls when the output
    /// overflows; dismissed on any non-scroll keypress.
    CommandOutput(CommandOutputState),
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
        toggles: Vec<SubmenuToggle>,
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
    SelectFromList(SelectFromListState),
    /// A background jj command is running with live output; the UI stays
    /// live. Esc/Ctrl-C cancels, j/k scrolls the output.
    CommandRunning(CommandRunningState),
}

impl AppMode {
    /// Construct a `CommandOutput` mode scrolled to the top.
    pub fn command_output(
        command: String,
        command_parts: Option<Vec<crate::jj_command::CommandPart>>,
        output: Vec<u8>,
        success: bool,
        retry: Vec<crate::types::FollowUpOption>,
    ) -> Self {
        AppMode::CommandOutput(CommandOutputState::new(
            command,
            command_parts,
            output,
            success,
            retry,
        ))
    }

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
        AppMode::SelectFromList(SelectFromListState::new(
            title,
            items,
            multi,
            on_select,
            focus_filter,
        ))
    }

    /// Construct a single-select `SelectFromList` whose first row is a
    /// free-input affordance labelled `custom_label` (see
    /// [`SelectFromListState::custom_entry`]).
    pub fn select_from_list_with_custom(
        title: impl Into<String>,
        custom_label: &str,
        custom: CustomEntry,
        mut items: Vec<String>,
        on_select: PendingSelection,
    ) -> Self {
        items.insert(0, custom_label.to_string());
        let mut state = SelectFromListState::new(title, items, false, on_select, false);
        state.custom_entry = Some(custom);
        AppMode::SelectFromList(state)
    }

    /// Take the `retry` field out of a `CommandOutput` mode, replacing `self` with `Normal`.
    pub fn take_command_retry(&mut self) -> Vec<crate::types::FollowUpOption> {
        match std::mem::replace(self, AppMode::Normal) {
            AppMode::CommandOutput(state) => state.retry,
            other => {
                *self = other;
                Vec::new()
            }
        }
    }

    /// Take the fields out of a `Jump` mode, replacing `self` with `Normal`.
    pub fn take_jump(&mut self) -> Option<(Vec<(String, RowIdx)>, String, Option<Box<AppMode>>)> {
        match std::mem::replace(self, AppMode::Normal) {
            AppMode::Jump {
                labels,
                input,
                restore_mode,
            } => Some((labels, input, restore_mode)),
            other => {
                *self = other;
                None
            }
        }
    }
}
