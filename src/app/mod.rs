mod data;
mod fold;
mod navigation;
mod search;
mod selection;
mod visual;

use std::collections::{HashMap, HashSet};

use ratatui::widgets::ListState;
use tui_input::Input;

use crate::dag::{DagEntry, DiffLine, FileChange, LineStats};
use crate::graph::{self, GraphLines};
use crate::idx::{EntryIdx, FileIdx, IndexVec};

use crate::keymap::{CommandFlags, KeymapNode};
use crate::repo_service::RepoRequest;
use crate::types::{
    ChangeId, CommitId, DisplayRow, FollowUpOption, GlobalToggle, PendingCommand,
    PendingCommitSelect, PendingSelection, SearchScopes, SearchState, SelectionContext,
    TargetOperation, VisualRange,
};

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
    pub search_scopes: u8,
    pub active_preset: Option<usize>,
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

pub enum Loadable<T> {
    NotRequested,
    Loading,
    Loaded(T),
    Failed(String),
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FileDiffCacheKey {
    pub commit_id: CommitId,
    pub path: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FileFoldKey {
    pub change_id: ChangeId,
    pub path: String,
}

impl<T> Loadable<T> {
    fn loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(value) => Some(value),
            _ => None,
        }
    }

    fn should_request(&self) -> bool {
        matches!(self, Self::NotRequested | Self::Failed(_))
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

/// Active visual selection mode.
pub enum VisualMode {
    /// Visual selection of diff lines within one file.
    Lines {
        /// Row index where `v` was pressed.
        anchor: usize,
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
    Bookmark(String),
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
        /// Error message shown in the title bar (e.g., unbound key).
        error: Option<String>,
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
    /// Live search input at the bottom bar.
    SearchInput,
    /// Navigating to select a target commit for a two-commit operation.
    TargetSelect {
        prompt: &'static str,
        source: ChangeId,
        restore_cursor: usize,
        operation: TargetOperation,
        flags: CommandFlags,
    },
    /// Navigating to select a single commit (e.g. for workspace revision).
    CommitSelect {
        restore_cursor: usize,
        pending: PendingCommitSelect,
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
            filtering: false,
            on_select,
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
    /// Persisted list widget state (preserves scroll offset across frames).
    pub list_state: ListState,
    /// Header height from the last render (for mouse click translation).
    pub last_header_height: u16,
    pub revset: String,
    /// Last failed revset attempt (pre-fills the input on retry).
    pub revset_draft: Option<String>,
    /// Named revset presets from config.
    pub presets: &'static [crate::theme::Preset],
    /// Active preset index (into `presets`), or `None` for jj default / manual revset.
    pub active_preset: Option<usize>,
    pub repo_root: String,
    /// Current interaction mode.
    pub mode: AppMode,
    /// Per-commit fold state, keyed by change id (stable across mutations).
    pub unfolded_commits: HashSet<ChangeId>,
    /// Per-file fold state, keyed by (change id, path) (stable across mutations).
    pub unfolded_files: HashSet<FileFoldKey>,
    /// Lazily loaded file changes, keyed by commit graph id.
    pub file_states: HashMap<CommitId, Loadable<Vec<FileChange>>>,
    /// Lazily loaded diff lines, keyed by (commit graph id, path).
    pub diff_states: HashMap<FileDiffCacheKey, Loadable<Vec<DiffLine>>>,
    /// Lazily loaded per-commit line stats, keyed by commit graph id.
    pub commit_stats_states: HashMap<CommitId, Loadable<LineStats>>,
    /// Remote bookmarks not yet tracked (for bookmark track selection).
    pub untracked_bookmarks: Vec<String>,
    /// Remote bookmarks that are tracked (for bookmark untrack selection).
    pub tracked_bookmarks: Vec<String>,
    /// Current revset load status.
    pub revset_state: Loadable<()>,
    /// Revset currently being requested, if any.
    pub pending_revset: Option<String>,
    /// Repo requests waiting to be sent to the background service.
    pending_repo_requests: Vec<RepoRequest>,
    /// Global toggles that persist across commands.
    pub toggles: CommandFlags,
    /// Display string of the last command executed (shown in status bar).
    pub last_command: Option<String>,
    /// Transient status notice shown in the status bar.
    pub status_message: Option<String>,
    /// Mode to restore after an overlay (search/help) is dismissed.
    /// Used when search or help is entered from TargetSelect/CommitSelect.
    pub pre_overlay_mode: Option<AppMode>,
    /// Whether to show line numbers in diff views.
    pub show_line_numbers: bool,
    /// Current selection context: implicit commit under cursor, or explicit
    /// homogeneous file/line selection.
    pub selection: SelectionContext,
    /// Active visual selection mode (line or commit).
    pub visual: Option<VisualMode>,
    /// Persistent visual range (survives exiting visual mode with `v`).
    /// Cleared on refresh, file collapse, or starting a new visual selection.
    pub visual_persistent: Option<PersistentVisualRange>,
    /// Active search state. Search remains active after closing the input.
    pub search: Option<SearchState>,
    /// Last-used search scopes (persisted across restarts).
    pub search_scopes: SearchScopes,
    /// Where to jump the cursor after the next DAG refresh.
    pub jump_after_refresh: Option<JumpTarget>,
}

impl App {
    pub fn new(
        entries: Vec<DagEntry>,
        revset: String,
        repo_root: String,
        presets: &'static [crate::theme::Preset],
    ) -> Self {
        let entries = IndexVec::from_vec(entries);
        let graph = IndexVec::from_vec(graph::render(entries.as_slice()));
        let mut app = Self {
            entries,
            graph,
            rows: Vec::new(),
            cursor: 0,
            list_state: ListState::default(),
            last_header_height: 2,
            revset,
            revset_draft: None,
            presets,
            active_preset: None,
            repo_root,
            mode: AppMode::Normal,
            unfolded_commits: HashSet::new(),
            unfolded_files: HashSet::new(),
            file_states: HashMap::new(),
            diff_states: HashMap::new(),
            commit_stats_states: HashMap::new(),
            untracked_bookmarks: Vec::new(),
            tracked_bookmarks: Vec::new(),
            revset_state: Loadable::NotRequested,
            pending_revset: None,
            pending_repo_requests: Vec::new(),
            toggles: CommandFlags::empty(),
            last_command: None,
            status_message: None,
            pre_overlay_mode: None,
            show_line_numbers: false,
            selection: SelectionContext::new(),
            visual: None,
            visual_persistent: None,
            search: None,
            search_scopes: SearchScopes::DEFAULT,
            jump_after_refresh: None,
        };
        app.rebuild_rows();
        app
    }

    /// Get the entry idx the cursor is on.
    pub fn selected_entry_idx(&self) -> Option<EntryIdx> {
        let entry_idx = match self.rows.get(self.cursor)? {
            DisplayRow::CommitNode { entry_idx }
            | DisplayRow::GraphLink { entry_idx, .. }
            | DisplayRow::FileChange { entry_idx, .. }
            | DisplayRow::DiffLine { entry_idx, .. } => *entry_idx,
        };
        Some(entry_idx)
    }

    /// Get the change ID (unique prefix) of the commit the cursor is on.
    pub fn selected_change_id(&self) -> Option<ChangeId> {
        let entry_idx = self.selected_entry_idx()?;
        Some(self.entries[entry_idx].commit.unique_prefix())
    }

    /// Whether the commit the cursor is on is a merge (multiple parents).
    pub fn selected_is_merge(&self) -> bool {
        self.selected_entry_idx()
            .is_some_and(|idx| self.entries[idx].commit.is_merge)
    }

    /// Get the bookmarks of the commit the cursor is on.
    pub fn selected_bookmarks(&self) -> Option<&[crate::dag::BookmarkInfo]> {
        let entry_idx = self.selected_entry_idx()?;
        Some(&self.entries[entry_idx].commit.bookmarks)
    }

    /// Get the tags of the commit the cursor is on.
    pub fn selected_tags(&self) -> Option<&[crate::types::Str]> {
        let entry_idx = self.selected_entry_idx()?;
        Some(&self.entries[entry_idx].commit.tags)
    }

    /// Get the description of the commit the cursor is on.
    pub fn selected_description(&self) -> Option<&str> {
        let entry_idx = self.selected_entry_idx()?;
        self.entries[entry_idx].commit.description.as_deref()
    }

    pub(crate) fn commit_id(&self, entry_idx: EntryIdx) -> &CommitId {
        &self.entries[entry_idx].commit.graph_id
    }

    pub(crate) fn change_id(&self, entry_idx: EntryIdx) -> ChangeId {
        self.entries[entry_idx].commit.unique_change_id()
    }

    pub(crate) fn change_id_for_commit_key(&self, commit_id: &CommitId) -> Option<ChangeId> {
        self.entries
            .iter_enumerated()
            .find(|(_, entry)| entry.commit.graph_id == *commit_id)
            .map(|(idx, _)| self.change_id(idx))
    }

    pub(crate) fn file_cache_key(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> Option<FileDiffCacheKey> {
        let path = self
            .files_for_entry(entry_idx)?
            .get(file_idx.raw())?
            .path
            .clone();
        Some(FileDiffCacheKey {
            commit_id: self.commit_id(entry_idx).clone(),
            path,
        })
    }

    pub(crate) fn file_fold_key(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> Option<FileFoldKey> {
        let path = self
            .files_for_entry(entry_idx)?
            .get(file_idx.raw())?
            .path
            .clone();
        Some(FileFoldKey {
            change_id: self.change_id(entry_idx),
            path,
        })
    }

    pub fn is_commit_unfolded(&self, entry_idx: EntryIdx) -> bool {
        self.unfolded_commits.contains(&self.change_id(entry_idx))
    }

    pub fn is_file_unfolded(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> bool {
        self.file_fold_key(entry_idx, file_idx)
            .is_some_and(|key| self.unfolded_files.contains(&key))
    }

    pub fn files_for_entry(&self, entry_idx: EntryIdx) -> Option<&Vec<FileChange>> {
        self.file_states
            .get(self.commit_id(entry_idx))
            .and_then(Loadable::loaded)
    }

    pub fn diff_lines(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> Option<&Vec<DiffLine>> {
        let key = self.file_cache_key(entry_idx, file_idx)?;
        self.diff_states.get(&key).and_then(Loadable::loaded)
    }

    pub fn commit_stats(&self, entry_idx: EntryIdx) -> Option<LineStats> {
        self.commit_stats_states
            .get(self.commit_id(entry_idx))
            .and_then(Loadable::loaded)
            .copied()
    }

    pub fn take_repo_requests(&mut self) -> Vec<RepoRequest> {
        std::mem::take(&mut self.pending_repo_requests)
    }

    pub fn to_persisted_state(&self) -> PersistedState {
        PersistedState {
            show_line_numbers: self.show_line_numbers,
            ignore_immutable: self.toggles.contains(CommandFlags::IGNORE_IMMUTABLE),
            ignore_working_copy: self.toggles.contains(CommandFlags::IGNORE_WORKING_COPY),
            debug: self.toggles.contains(CommandFlags::DEBUG),
            search_scopes: self.search_scopes.bits(),
            active_preset: self.active_preset,
        }
    }

    pub fn apply_persisted_state(&mut self, state: &PersistedState) {
        self.show_line_numbers = state.show_line_numbers;
        self.toggles
            .set(CommandFlags::IGNORE_IMMUTABLE, state.ignore_immutable);
        self.toggles
            .set(CommandFlags::IGNORE_WORKING_COPY, state.ignore_working_copy);
        self.toggles.set(CommandFlags::DEBUG, state.debug);
        if state.search_scopes != 0 {
            self.search_scopes = SearchScopes::from_bits_truncate(state.search_scopes);
        }
        self.active_preset = state.active_preset
            .filter(|&i| i < self.presets.len());
    }

    /// Get the scroll offset from the list state.
    pub fn scroll_offset(&self) -> usize {
        self.list_state.offset()
    }

    /// Get the text to pre-fill the revset input with.
    /// Uses the last failed draft if one exists, otherwise the current revset.
    pub fn revset_input_text(&self) -> &str {
        self.revset_draft.as_deref().unwrap_or(&self.revset)
    }
}

