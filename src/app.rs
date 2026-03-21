use std::collections::{HashMap, HashSet};

use compact_str::format_compact;
use tui_input::Input;

use crate::dag::{DagEntry, DiffLine, DiffLineKind, FileChange};
use crate::graph::{self, GraphLines};
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx, GraphLineIdx, IndexVec};

use crate::keymap::{CommandFlags, KeymapNode};
use crate::repo::JjRepo;
use crate::types::{
    ChangeId, DisplayRow, FileRef, FileSelectionState, FollowUpOption, GlobalToggle,
    PendingCommand, PendingSelection, RowKey, SearchFocus, SearchScopes, SearchState, Selection,
    SelectionContext, SelectionKind, TargetOperation, VisualRange,
};

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
    /// Current selection context: implicit commit under cursor, or explicit
    /// homogeneous file/line selection.
    pub selection: SelectionContext,
    /// Visual mode anchor row index. When `Some`, visual selection is actively
    /// being extended. Only valid on DiffLine rows.
    pub visual_anchor: Option<usize>,
    /// Persistent visual range (survives exiting visual mode with `v`).
    /// Cleared on file collapse, refresh, or starting a new visual selection.
    pub visual_range: Option<VisualRange>,
    /// Active search state. Search remains active after closing the input.
    pub search: Option<SearchState>,
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
            selection: SelectionContext::new(),
            visual_anchor: None,
            visual_range: None,
            search: None,
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

    /// Get the entry idx the cursor is on.
    fn selected_entry_idx(&self) -> Option<EntryIdx> {
        let entry_idx = match self.rows.get(self.cursor)? {
            DisplayRow::CommitNode { entry_idx }
            | DisplayRow::GraphLink { entry_idx, .. }
            | DisplayRow::FileChange { entry_idx, .. }
            | DisplayRow::DiffLine { entry_idx, .. } => *entry_idx,
        };
        Some(entry_idx)
    }

    /// Get the change ID (unique prefix) of the commit the cursor is on.
    ///
    /// Works from any row type -- files and diff lines resolve to their
    /// parent commit.
    pub fn selected_change_id(&self) -> Option<ChangeId> {
        let entry_idx = self.selected_entry_idx()?;
        let commit = &self.entries[entry_idx].commit;
        let id = &commit.change_id;
        let prefix = &id.display[..id.prefix_len.min(id.display.len())];
        if let Some(suffix) = commit.change_id_suffix {
            Some(ChangeId::new(format_compact!("{prefix}/{suffix}")))
        } else {
            Some(ChangeId::new(prefix))
        }
    }

    /// Get the bookmarks of the commit the cursor is on.
    pub fn selected_bookmarks(&self) -> Option<&[crate::dag::BookmarkInfo]> {
        let entry_idx = self.selected_entry_idx()?;
        Some(&self.entries[entry_idx].commit.bookmarks)
    }

    /// Get the description of the commit the cursor is on.
    pub fn selected_description(&self) -> Option<&str> {
        let entry_idx = self.selected_entry_idx()?;
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

    pub fn selection_active(&self) -> bool {
        self.selection.is_active()
    }

    pub fn selection_kind(&self) -> SelectionKind {
        self.selection.kind()
    }

    pub fn explicit_selection(&self) -> Option<&HashSet<Selection>> {
        self.selection.explicit()
    }

    /// Toggle file selection. If the file belongs to a different commit than
    /// existing selections, clears the old selections first (selections are
    /// scoped to one commit at a time).
    pub fn toggle_file_selection(&mut self, entry_idx: EntryIdx, file_idx: FileIdx) {
        let change_id: ChangeId = self.entries[entry_idx].commit.change_id.change_id();
        let path = self.file_cache[&entry_idx][file_idx.raw()].path.clone();

        self.clear_other_commits(&change_id);
        self.selection.ensure_kind(SelectionKind::File);

        // Clear any line-level selections for this file (File overrides Lines).
        self.selection
            .retain(|s| !matches!(s, Selection::Line { file_ref: f, .. } if f.path == path));

        let sel = Selection::File(FileRef { change_id, path });
        if !self.selection.remove(&sel) {
            self.selection.insert(SelectionKind::File, sel);
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

        let change_id = self.entries[entry_idx].commit.change_id.change_id();
        self.clear_other_commits(&change_id);
        self.selection.ensure_kind(SelectionKind::File);

        // Collect file paths upfront to avoid borrowing self.file_cache across mutations.
        let file_paths: Vec<String> = self.file_cache[&entry_idx]
            .iter()
            .map(|f| f.path.clone())
            .collect();

        // If all files are already selected (File-level), deselect all.
        let all_selected = file_paths.iter().all(|p| {
            self.selection.contains(&Selection::File(FileRef {
                change_id: change_id.clone(),
                path: p.clone(),
            }))
        });

        if all_selected {
            self.selection.clear();
        } else {
            self.selection.clear();
            for p in file_paths {
                self.selection.insert(
                    SelectionKind::File,
                    Selection::File(FileRef {
                        change_id: change_id.clone(),
                        path: p,
                    }),
                );
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
        let change_id = self.entries[entry_idx].commit.change_id.change_id();
        let file_path = self.file_cache[&entry_idx][file_idx.raw()].path.clone();

        // Extract what we need from the diff line before mutating self.
        let dl = &self.diff_cache[&(entry_idx, file_idx)][line_idx.raw()];
        if dl.kind != DiffLineKind::Added && dl.kind != DiffLineKind::Removed {
            return;
        }
        let old_line = dl.old_line;
        let new_line = dl.new_line;

        self.clear_other_commits(&change_id);
        self.selection.ensure_kind(SelectionKind::Line);

        // If there's a File-level selection for this file, remove it.
        self.selection.remove(&Selection::File(FileRef {
            change_id: change_id.clone(),
            path: file_path.clone(),
        }));

        let sel = Selection::Line {
            file_ref: FileRef {
                change_id,
                path: file_path,
            },
            old_line,
            new_line,
        };
        if !self.selection.remove(&sel) {
            self.selection.insert(SelectionKind::Line, sel);
        }
    }

    /// Toggle all added/removed lines in a hunk (triggered by space on a header line).
    pub fn toggle_hunk_selection(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        header_line_idx: DiffLineIdx,
    ) {
        let change_id = self.entries[entry_idx].commit.change_id.change_id();
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
                        file_ref: FileRef {
                            change_id: change_id.clone(),
                            path: file_path.clone(),
                        },
                        old_line: dl.old_line,
                        new_line: dl.new_line,
                    });
                }
            }
        }

        self.clear_other_commits(&change_id);
        self.selection.ensure_kind(SelectionKind::Line);

        // Remove any File-level selection for this file.
        self.selection.remove(&Selection::File(FileRef {
            change_id: change_id.clone(),
            path: file_path.clone(),
        }));

        // If all hunk lines are already selected, deselect them. Otherwise select all.
        let all_selected = hunk_lines.iter().all(|s| self.selection.contains(s));
        if all_selected {
            for s in &hunk_lines {
                self.selection.remove(s);
            }
        } else {
            for s in hunk_lines {
                self.selection.insert(SelectionKind::Line, s);
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
        let change_id = &self.entries[entry_idx].commit.change_id.change_id();
        let file_path = &self.file_cache[&entry_idx][file_idx.raw()].path;
        let diff_line = &self.diff_cache[&(entry_idx, file_idx)][line_idx.raw()];

        // If the whole file is selected, all lines are implicitly selected.
        if self.selection.contains(&Selection::File(FileRef {
            change_id: change_id.clone(),
            path: file_path.clone(),
        })) {
            return diff_line.kind == DiffLineKind::Added
                || diff_line.kind == DiffLineKind::Removed;
        }

        self.selection.contains(&Selection::Line {
            file_ref: FileRef {
                change_id: change_id.clone(),
                path: file_path.clone(),
            },
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
        let change_id = &self.entries[entry_idx].commit.change_id.change_id();
        let file_path = &self.file_cache[&entry_idx][file_idx.raw()].path;

        // Explicit file-level selection.
        if self.selection.contains(&Selection::File(FileRef {
            change_id: change_id.clone(),
            path: file_path.clone(),
        })) {
            return FileSelectionState::Full;
        }

        // Count line-level selections for this file.
        let selected_count = self
            .selection
            .iter()
            .filter(|s| {
                matches!(s, Selection::Line { file_ref, .. }
                    if file_ref.change_id == *change_id && file_ref.path == *file_path)
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
        self.selection.any(|s| matches!(s, Selection::Line { .. }))
    }

    /// Get the unique file paths from all selections.
    pub fn selected_file_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .selection
            .iter()
            .map(|s| s.path().to_string())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// Number of currently selected items.
    pub fn selection_count(&self) -> usize {
        self.selection.len()
    }

    /// Clear all selections.
    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Clear selections from other commits if switching to a different one.
    fn clear_other_commits(&mut self, change_id: &ChangeId) {
        if self.selection.is_active() {
            let same_commit = self.selection.any(|s| s.change_id() == change_id);
            if !same_commit {
                self.clear_selection();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    pub fn begin_search(&mut self) {
        let restore_cursor = self.cursor;
        match &mut self.search {
            Some(search) => {
                search.restore_cursor = restore_cursor;
                search.focus = SearchFocus::Query;
            }
            None => self.search = Some(SearchState::new(restore_cursor)),
        }
        self.recompute_search_matches();
        self.mode = AppMode::SearchInput;
    }

    pub fn cancel_search(&mut self) {
        if let Some(search) = &self.search {
            self.cursor = search.restore_cursor;
        }
        self.search = None;
        self.mode = AppMode::Normal;
    }

    /// Clear the active search without restoring the cursor.
    ///
    /// Used from plain normal mode, where the current cursor position is the
    /// user's intentional location after navigating matches.
    pub fn clear_search(&mut self) {
        self.search = None;
        self.mode = AppMode::Normal;
    }

    pub fn confirm_search(&mut self) {
        let clear = self
            .search
            .as_ref()
            .is_some_and(|search| search.query().is_empty());
        if clear {
            self.search = None;
        }
        self.mode = AppMode::Normal;
    }

    pub fn search_next(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        let next = match search.current_match {
            Some(i) => (i + 1) % search.matches.len(),
            None => 0,
        };
        search.current_match = Some(next);
        self.cursor = search.matches[next];
    }

    pub fn search_prev(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        let prev = match search.current_match {
            Some(0) | None => search.matches.len() - 1,
            Some(i) => i - 1,
        };
        search.current_match = Some(prev);
        self.cursor = search.matches[prev];
    }

    pub fn is_match(&self, row_idx: usize) -> bool {
        self.search
            .as_ref()
            .is_some_and(|search| search.matches.contains(&row_idx))
    }

    pub fn toggle_search_scope(&mut self, flag: SearchScopes) {
        if let Some(search) = &mut self.search {
            search.scopes.toggle(flag);
            self.recompute_search_matches();
        }
    }

    pub fn reset_search_scopes(&mut self) {
        if let Some(search) = &mut self.search {
            search.scopes = SearchScopes::DEFAULT;
            self.recompute_search_matches();
        }
    }

    pub fn enable_all_search_scopes(&mut self) {
        if let Some(search) = &mut self.search {
            search.scopes = SearchScopes::all();
            self.recompute_search_matches();
        }
    }

    pub fn toggle_search_focus(&mut self) {
        if let Some(search) = &mut self.search {
            search.focus = match search.focus {
                SearchFocus::Query => SearchFocus::Scopes,
                SearchFocus::Scopes => SearchFocus::Query,
            };
        }
    }

    pub fn update_search_input(&mut self, input: Input) {
        if let Some(search) = &mut self.search {
            search.input = input;
        }
        self.recompute_search_matches();
    }

    fn recompute_search_matches(&mut self) {
        let Some(search) = &self.search else {
            return;
        };
        let query = search.query().to_string();
        let scopes = search.scopes;
        let preferred = search.restore_cursor.min(self.rows.len().saturating_sub(1));

        let mut matches = Vec::new();
        if !query.is_empty() && !scopes.is_empty() {
            for row_idx in 0..self.rows.len() {
                if self.row_matches(row_idx, &query, scopes) {
                    matches.push(row_idx);
                }
            }
        }

        let current_match = if matches.is_empty() {
            None
        } else {
            Some(
                matches
                    .iter()
                    .position(|&row| row >= preferred)
                    .unwrap_or(0),
            )
        };

        if let Some(search) = &mut self.search {
            search.matches = matches;
            search.current_match = current_match;
            if let Some(idx) = current_match {
                self.cursor = search.matches[idx];
            }
        }
    }

    fn row_matches(&self, row_idx: usize, query: &str, scopes: SearchScopes) -> bool {
        let case_sensitive = query.chars().any(|c| c.is_ascii_uppercase());
        let contains = |haystack: &str| {
            if case_sensitive {
                haystack.contains(query)
            } else {
                haystack.to_lowercase().contains(&query.to_lowercase())
            }
        };

        match &self.rows[row_idx] {
            DisplayRow::CommitNode { entry_idx } => {
                let commit = &self.entries[*entry_idx].commit;
                (scopes.contains(SearchScopes::CHANGE_ID)
                    && (contains(commit.change_id.display.as_str())
                        || commit.change_id_suffix.is_some_and(|n| {
                            contains(&format!("{}/{n}", commit.change_id.display))
                        })))
                    || (scopes.contains(SearchScopes::COMMIT_ID)
                        && contains(commit.commit_id.display.as_str()))
                    || (scopes.contains(SearchScopes::DESCRIPTION)
                        && commit.description.as_deref().is_some_and(contains))
                    || (scopes.contains(SearchScopes::AUTHOR)
                        && (contains(commit.author.name.as_str())
                            || contains(commit.author.email.as_str())))
                    || (scopes.contains(SearchScopes::BOOKMARK)
                        && (commit.bookmarks.iter().any(|b| contains(b.name.as_str()))
                            || commit
                                .remote_bookmarks
                                .iter()
                                .any(|b| contains(&format!("{}@{}", b.name, b.remote)))))
            }
            DisplayRow::FileChange {
                entry_idx,
                file_idx,
            } => {
                scopes.contains(SearchScopes::PATH)
                    && self
                        .file_cache
                        .get(entry_idx)
                        .and_then(|files| files.get(file_idx.raw()))
                        .is_some_and(|file| contains(file.path.as_str()))
            }
            DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            } => {
                (scopes.contains(SearchScopes::LINE)
                    && self
                        .diff_cache
                        .get(&(*entry_idx, *file_idx))
                        .and_then(|lines| lines.get(line_idx.raw()))
                        .is_some_and(|line| contains(line.content.as_str())))
                    || (scopes.contains(SearchScopes::PATH)
                        && self
                            .file_cache
                            .get(entry_idx)
                            .and_then(|files| files.get(file_idx.raw()))
                            .is_some_and(|file| contains(file.path.as_str())))
            }
            DisplayRow::GraphLink { .. } => false,
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
                    change_id = Some(self.entries[*entry_idx].commit.change_id.change_id());
                    path = Some(self.file_cache[entry_idx][file_idx.raw()].path.clone());
                }
                end_line = Some(*line_idx);
            }
        }

        if let (Some(start), Some(end), Some(change_id), Some(p)) =
            (start_line, end_line, change_id, path)
        {
            self.visual_range = Some(VisualRange {
                change_id,
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
            let cid = self.entries[entry_idx].commit.change_id.change_id();
            if let Some(files) = self.file_cache.get(&entry_idx) {
                if let Some(file) = files.get(file_idx.raw()) {
                    if cid == vr.change_id
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
            let cid = self.entries[*entry_idx].commit.change_id.change_id();
            if let Some(file) = self
                .file_cache
                .get(entry_idx)
                .and_then(|f| f.get(file_idx.raw()))
            {
                return cid == vr.change_id
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
        let line_data: Vec<(ChangeId, String, Option<u32>, Option<u32>)> = self
            .diff_cache
            .iter()
            .filter_map(|((eidx, fidx), diff_lines)| {
                let cid = self.entries[*eidx].commit.change_id.change_id();
                let path = &self.file_cache[eidx][fidx.raw()].path;
                if cid != vr.change_id || path != &vr.path {
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
            let file_ref = FileRef {
                change_id: cid.clone(),
                path: path.clone(),
            };

            self.selection.contains(&Selection::Line {
                file_ref,
                old_line: *ol,
                new_line: *nl,
            })
        });

        if all_selected {
            for (cid, path, ol, nl) in &line_data {
                self.selection.remove(&Selection::Line {
                    file_ref: FileRef {
                        change_id: cid.clone(),
                        path: path.clone(),
                    },
                    old_line: *ol,
                    new_line: *nl,
                });
            }
        } else {
            if let Some((cid, ..)) = line_data.first() {
                self.clear_other_commits(cid);
            }
            self.selection.ensure_kind(SelectionKind::Line);
            for (cid, path, ol, nl) in line_data {
                self.selection.remove(&Selection::File(FileRef {
                    change_id: cid.clone(),
                    path: path.clone(),
                }));
                self.selection.insert(
                    SelectionKind::Line,
                    Selection::Line {
                        file_ref: FileRef {
                            change_id: cid,
                            path,
                        },
                        old_line: ol,
                        new_line: nl,
                    },
                );
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
                let cid = self.entries[entry_idx].commit.change_id.change_id();
                if let Some(file) = self
                    .file_cache
                    .get(&entry_idx)
                    .and_then(|f| f.get(file_idx.raw()))
                {
                    if cid == vr.change_id && file.path == vr.path {
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
