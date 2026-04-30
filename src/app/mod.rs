mod data;
mod fold;
mod navigation;
mod search;
mod selection;
mod types;
mod visual;

pub use types::*;

use std::collections::{HashMap, HashSet};

/// Signals deferred work that should run once after a batch of repo results.
#[derive(Default)]
pub struct DeferredWork {
    pub rebuild: bool,
    pub scroll: bool,
}

impl DeferredWork {
    pub fn merge(&mut self, other: Self) {
        self.rebuild |= other.rebuild;
        self.scroll |= other.scroll;
    }
}

use ratatui::widgets::ListState;

use crate::dag::{DagEntry, DiffLine, EdgeKind, FileChange, LineStats};
use crate::graph;
use crate::idx::{EntryIdx, FileIdx, IndexVec};
use crate::types::SmallVec;

use crate::keymap::CommandFlags;
use crate::repo_service::RepoRequest;
use crate::types::{
    BookmarkName, ChangeId, CommitId, DisplayRow, RepoPath, SearchScopes, SearchState,
    SelectionContext, Str,
};

#[derive(Clone)]
pub enum Loadable<T> {
    NotRequested,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(value) => Some(value),
            _ => None,
        }
    }

    fn should_request(&self) -> bool {
        matches!(self, Self::NotRequested | Self::Failed(_))
    }
}

/// Consolidated per-commit node in the DAG. Holds commit metadata, resolved
/// graph edges, rendering data, and lazily loaded file/diff caches.
pub struct DagNode {
    pub commit: crate::dag::CommitInfo,
    pub graph: crate::graph::GraphLines,
    /// Resolved direct parent entry indices (from DAG edges).
    pub parents: SmallVec<EntryIdx>,
    /// Resolved child entry indices (reverse edges, filled in second pass).
    pub children: SmallVec<EntryIdx>,
    /// Row index of this commit's `CommitNode` in the display rows.
    pub row: usize,
    /// Lazily loaded file changes for this commit.
    pub files: Loadable<Vec<FileChange>>,
    /// Lazily loaded per-commit line stats.
    pub stats: Loadable<LineStats>,
    /// Lazily loaded diff lines, parallel to `files` (indexed by FileIdx).
    pub diffs: Vec<Loadable<Vec<DiffLine>>>,
    /// Lazily loaded conflict hunks, parallel to `files` (for conflicted files).
    pub conflict_hunks: Vec<Loadable<Vec<crate::dag::ConflictHunk>>>,
}

impl DagNode {
    /// Ensure the diffs vector is large enough to hold `n` entries.
    pub fn ensure_diffs(&mut self, n: usize) {
        if self.diffs.len() < n {
            self.diffs.resize_with(n, || Loadable::NotRequested);
        }
    }

    /// Ensure the conflict_hunks vector is large enough to hold `n` entries.
    pub fn ensure_conflict_hunks(&mut self, n: usize) {
        if self.conflict_hunks.len() < n {
            self.conflict_hunks
                .resize_with(n, || Loadable::NotRequested);
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct FileFoldKey {
    pub change_id: ChangeId,
    pub path: RepoPath,
}

/// Application state. Pure data -- no I/O, no rendering.
pub struct App {
    pub active_view: ActiveView,
    pub nodes: IndexVec<EntryIdx, DagNode>,
    /// Lookup from commit graph_id → entry index (needed at event boundary).
    pub commit_index: HashMap<CommitId, EntryIdx>,
    /// Aggregated bookmark data for the bookmark view.
    pub bookmark_entries: Vec<BookmarkViewEntry>,
    /// Rich detail data per bookmark (conflict targets, remote tracking).
    pub bookmark_details: HashMap<BookmarkName, crate::dag::BookmarkDetails>,
    /// All local tag names (including those outside the current revset).
    pub all_tags: Vec<Str>,
    /// Aggregated tag data for the tag view.
    pub tag_entries: Vec<TagViewEntry>,
    /// Rich detail data per tag (remote tracking).
    pub tag_details: HashMap<Str, crate::dag::TagDetails>,
    /// Aggregated workspace data for the workspace view.
    pub workspace_entries: Vec<WorkspaceViewEntry>,
    /// Aggregated operation log data for the operations view.
    pub op_log_entries: Vec<OpLogEntry>,
    /// Whether the operation log has been loaded (lazy).
    pub op_log_loaded: bool,
    /// Whether there are more operations beyond the current batch.
    pub op_log_has_more: bool,
    /// How many operations to request in the next load.
    pub op_log_limit: usize,
    /// Active workspace filter for the op log view (empty = show all).
    pub op_log_workspace_filter: HashSet<Str>,
    /// Set of op IDs that are currently unfolded.
    pub unfolded_ops: HashSet<Str>,
    /// Cached op detail lines per op ID.
    pub op_details: HashMap<Str, Loadable<Vec<OpDetailLine>>>,
    /// Aggregated evolution log data for the evolog view.
    pub evolog_entries: Vec<EvoLogEntry>,
    /// Whether the evolog has been loaded (lazy).
    pub evolog_loaded: bool,
    /// Which commit ID the evolog is loaded for.
    pub evolog_commit_id: Option<CommitId>,
    /// Evolog entries that are unfolded (showing files).
    pub unfolded_evolog: HashSet<CommitId>,
    /// Cached file changes per evolog entry commit ID.
    pub evolog_files: HashMap<CommitId, Loadable<Vec<crate::dag::FileChange>>>,
    /// Evolog files that are unfolded (showing diff lines).
    pub unfolded_evolog_files: HashSet<(CommitId, crate::types::RepoPath)>,
    /// Cached diff lines per (evolog commit ID, file path).
    pub evolog_file_diffs: HashMap<(CommitId, crate::types::RepoPath), Loadable<Vec<crate::dag::DiffLine>>>,
    /// Flattened display rows (one per visual line).
    pub rows: Vec<DisplayRow>,
    /// Index into `rows` of the currently selected row.
    pub cursor: usize,
    /// Saved per-view state (cursor, scroll, h_scroll, search scopes).
    view_states: [ViewState; <ActiveView as strum::EnumCount>::COUNT],
    /// Persisted list widget state (preserves scroll offset across frames).
    pub list_state: ListState,
    /// Header height from the last render (for mouse click translation).
    pub last_header_height: u16,
    /// Viewport height of the main list area (set during render).
    pub last_list_height: u16,
    /// Horizontal scroll offset (display columns).
    pub h_scroll: usize,
    pub revset: String,
    /// Last failed revset attempt (pre-fills the input on retry).
    pub revset_draft: Option<String>,
    /// Named revset presets from config.
    pub presets: &'static [crate::theme::Preset],
    /// Glyph characters for DAG rendering.
    pub glyphs: &'static crate::theme::GlyphChars,
    /// Default search scopes from config.
    pub default_search_scopes: SearchScopes,
    /// Active preset index (into `presets`), or `None` for jj default / manual revset.
    pub active_preset: Option<usize>,
    pub repo_root: String,
    /// Current interaction mode.
    pub mode: AppMode,
    /// Per-commit fold state, keyed by change id (stable across mutations).
    pub unfolded_commits: HashSet<ChangeId>,
    /// Per-file fold state, keyed by (change id, path) (stable across mutations).
    pub(crate) unfolded_files: HashSet<FileFoldKey>,
    /// Remote bookmarks not yet tracked (for bookmark track selection).
    pub untracked_bookmarks: Vec<Str>,
    /// Remote bookmarks that are tracked (for bookmark untrack selection).
    pub tracked_bookmarks: Vec<Str>,
    /// Available git remote names.
    pub remotes: Vec<crate::types::Str>,
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
    pub status_message: Option<(String, StatusLevel)>,
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
        glyphs: &'static crate::theme::GlyphChars,
    ) -> Self {
        let entries = IndexVec::from_vec(entries);
        let commit_index = build_commit_index(&entries);
        let nodes = build_nodes(entries, &commit_index, glyphs);
        let mut app = Self {
            active_view: ActiveView::Dag,
            nodes,
            commit_index,
            bookmark_entries: Vec::new(),
            bookmark_details: HashMap::new(),
            all_tags: Vec::new(),
            tag_entries: Vec::new(),
            tag_details: HashMap::new(),
            workspace_entries: Vec::new(),
            op_log_entries: Vec::new(),
            op_log_loaded: false,
            op_log_has_more: false,
            op_log_limit: OP_LOG_BATCH_SIZE,
            op_log_workspace_filter: HashSet::from([Str::from("default")]),
            unfolded_ops: HashSet::new(),
            op_details: HashMap::new(),
            evolog_entries: Vec::new(),
            evolog_loaded: false,
            evolog_commit_id: None,
            unfolded_evolog: HashSet::new(),
            evolog_files: HashMap::new(),
            unfolded_evolog_files: HashSet::new(),
            evolog_file_diffs: HashMap::new(),
            rows: Vec::new(),
            cursor: 0,
            view_states: default_view_states(),
            list_state: ListState::default(),
            last_header_height: 2,
            last_list_height: 0,
            h_scroll: 0,
            revset,
            revset_draft: None,
            presets,
            glyphs,
            default_search_scopes: SearchScopes::DEFAULT,
            active_preset: None,
            repo_root,
            mode: AppMode::Normal,
            unfolded_commits: HashSet::new(),
            unfolded_files: HashSet::new(),
            untracked_bookmarks: Vec::new(),
            tracked_bookmarks: Vec::new(),
            remotes: Vec::new(),
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
            search_scopes: SearchScopes::DEFAULT, // overwritten by apply_persisted_state or config
            jump_after_refresh: None,
        };
        app.rebuild_rows();
        app
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), StatusLevel::Info));
    }

    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), StatusLevel::Error));
    }

    /// Whether the working copy (`@`) is visible in the current entries.
    pub fn has_working_copy(&self) -> bool {
        self.nodes.iter().any(|n| n.commit.is_working_copy())
    }

    /// Row index of a commit's `CommitNode` in the display rows.
    pub fn row_of_commit(&self, entry_idx: EntryIdx) -> Option<usize> {
        Some(self.nodes.get(entry_idx)?.row)
    }

    pub fn switch_view(&mut self, view: ActiveView) {
        if self.active_view == view {
            return;
        }
        // Save current view state.
        let offset = self.scroll_offset();
        let vs = &mut self.view_states[self.active_view.idx()];
        vs.cursor = self.cursor;
        vs.scroll_offset = offset;
        vs.h_scroll = self.h_scroll;
        vs.search_scopes = self.search_scopes;

        self.active_view = view;
        // Trigger lazy load of operation log data.
        if view == ActiveView::Operations && !self.op_log_loaded {
            self.pending_repo_requests
                .push(RepoRequest::load_operations(self.op_log_limit));
        }
        // Trigger lazy load of evolog data.
        if view == ActiveView::Evolog {
            if let Some(commit_id) = self
                .selected_entry_idx()
                .map(|idx| self.nodes[idx].commit.graph_id.clone())
            {
                let changed = self.evolog_commit_id.as_ref() != Some(&commit_id);
                if changed || !self.evolog_loaded {
                    self.evolog_commit_id = Some(commit_id.clone());
                    self.evolog_loaded = false;
                    self.evolog_entries.clear();
                    self.unfolded_evolog.clear();
                    self.evolog_files.clear();
                    self.unfolded_evolog_files.clear();
                    self.evolog_file_diffs.clear();
                    self.pending_repo_requests
                        .push(RepoRequest::load_evolution_log(commit_id));
                }
            }
        }
        self.rebuild_rows();

        // Restore saved state for new view.
        let vs = &self.view_states[view.idx()];
        self.cursor = vs.cursor.min(self.rows.len().saturating_sub(1));
        *self.list_state.offset_mut() = vs.scroll_offset;
        self.search_scopes = vs.search_scopes;
        self.h_scroll = vs.h_scroll;
    }

    /// Number of ancestor generations to load when expanding at a terminator.
    const ANCESTOR_EXPAND_COUNT: usize = 10;

    /// Widen the revset to include ancestors of the given commit.
    pub fn expand_ancestors(&mut self, entry_idx: crate::idx::EntryIdx) {
        let change_id = self.change_id(entry_idx);
        let new_revset = format!(
            "({}) | ancestors({}, {})",
            self.revset,
            change_id,
            Self::ANCESTOR_EXPAND_COUNT,
        );
        self.jump_after_refresh = Some(JumpTarget::ChangeId(change_id.to_string()));
        self.revset_state = Loadable::Loading;
        self.pending_revset = Some(new_revset.clone());
        self.pending_repo_requests
            .push(RepoRequest::load_revset(Some(new_revset)));
    }

    pub fn request_op_log_load_more(&mut self) {
        self.op_log_limit += OP_LOG_BATCH_SIZE;
        self.op_log_loaded = false;
        self.pending_repo_requests
            .push(RepoRequest::load_operations(self.op_log_limit));
    }

    pub fn selected_bookmark_entry(&self) -> Option<&BookmarkViewEntry> {
        let bookmark_idx = match self.rows.get(self.cursor)? {
            DisplayRow::BookmarkItem { bookmark_idx }
            | DisplayRow::BookmarkConflictTarget { bookmark_idx, .. }
            | DisplayRow::BookmarkRemoteTarget { bookmark_idx, .. } => *bookmark_idx,
            _ => return None,
        };
        self.bookmark_entries.get(bookmark_idx.raw())
    }

    /// Get the conflict target under cursor (if on a `BookmarkConflictTarget` row).
    pub fn selected_conflict_target(
        &self,
    ) -> Option<(&BookmarkViewEntry, &crate::dag::BookmarkConflictTarget)> {
        let (bookmark_idx, target_idx) = match self.rows.get(self.cursor)? {
            DisplayRow::BookmarkConflictTarget {
                bookmark_idx,
                target_idx,
            } => (*bookmark_idx, *target_idx),
            _ => return None,
        };
        let entry = self.bookmark_entries.get(bookmark_idx.raw())?;
        let target = self
            .bookmark_details
            .get(&entry.name)?
            .conflict_targets
            .get(target_idx.raw())?;
        Some((entry, target))
    }

    /// Get the remote target under cursor (if on a `BookmarkRemoteTarget` row).
    pub fn selected_remote_target(
        &self,
    ) -> Option<(&BookmarkViewEntry, &crate::dag::BookmarkRemoteTarget)> {
        let (bookmark_idx, target_idx) = match self.rows.get(self.cursor)? {
            DisplayRow::BookmarkRemoteTarget {
                bookmark_idx,
                target_idx,
            } => (*bookmark_idx, *target_idx),
            _ => return None,
        };
        let entry = self.bookmark_entries.get(bookmark_idx.raw())?;
        let target = self
            .bookmark_details
            .get(&entry.name)?
            .remote_targets
            .get(target_idx.raw())?;
        Some((entry, target))
    }

    /// Resolve a `BookmarkRef` from cursor context: prefer the remote target
    /// row's remote, fall back to the bookmark entry's remote field.
    pub fn selected_bookmark_ref(&self) -> Option<crate::dag::BookmarkRef> {
        if let Some((entry, target)) = self.selected_remote_target() {
            return Some(crate::dag::BookmarkRef {
                name: entry.name.clone(),
                remote: target.remote.clone(),
            });
        }
        let entry = self.selected_bookmark_entry()?;
        let remote = entry.remote.clone()?;
        Some(crate::dag::BookmarkRef {
            name: entry.name.clone(),
            remote,
        })
    }

    pub fn selected_tag_entry(&self) -> Option<&TagViewEntry> {
        let tag_idx = match self.rows.get(self.cursor)? {
            DisplayRow::TagItem { tag_idx } | DisplayRow::TagRemoteTarget { tag_idx, .. } => {
                *tag_idx
            }
            _ => return None,
        };
        self.tag_entries.get(tag_idx.raw())
    }

    /// Pick a side for a conflict hunk. If all hunks are resolved, write the file back.
    pub fn pick_conflict_side(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        hunk_idx: crate::idx::ConflictHunkIdx,
        side: usize,
    ) {
        let fi = file_idx.raw();
        let hi = hunk_idx.raw();
        let Some(hunks) = self.nodes[entry_idx]
            .conflict_hunks
            .get_mut(fi)
            .and_then(|l| match l {
                Loadable::Loaded(h) => Some(h),
                _ => None,
            })
        else {
            return;
        };
        let Some(hunk) = hunks.get_mut(hi) else {
            return;
        };
        if let crate::dag::ConflictHunkKind::Conflict {
            sides, selected, ..
        } = &mut hunk.kind
        {
            if side < sides.len() {
                *selected = Some(side);
            }
        }

        // Check if all conflict hunks are now resolved.
        let all_resolved = hunks.iter().all(|h| match &h.kind {
            crate::dag::ConflictHunkKind::Resolved { .. } => true,
            crate::dag::ConflictHunkKind::Conflict { selected, .. } => selected.is_some(),
        });

        if all_resolved {
            // Assemble resolved content.
            let mut content = String::new();
            for h in hunks.iter() {
                match &h.kind {
                    crate::dag::ConflictHunkKind::Resolved { lines } => {
                        for line in lines {
                            content.push_str(line);
                            content.push('\n');
                        }
                    }
                    crate::dag::ConflictHunkKind::Conflict {
                        sides, selected, ..
                    } => {
                        if let Some(si) = selected {
                            if let Some(side_lines) = sides.get(*si) {
                                for line in side_lines {
                                    content.push_str(line);
                                    content.push('\n');
                                }
                            }
                        }
                    }
                }
            }
            // Write to the working copy file.
            let path = self
                .files_for_entry(entry_idx)
                .and_then(|f| f.get(fi))
                .map(|f| f.path.clone());
            if let Some(file_path) = path {
                let full_path = std::path::Path::new(&self.repo_root).join(file_path.as_str());
                if std::fs::write(&full_path, &content).is_ok() {
                    self.set_status(format!("resolved {}", file_path));
                } else {
                    self.set_error(format!("failed to write {}", file_path));
                }
            }
        }

        self.rebuild_rows();
    }

    pub fn selected_op_log_entry(&self) -> Option<&OpLogEntry> {
        let op_log_idx = match self.rows.get(self.cursor)? {
            DisplayRow::OpLogItem { op_log_idx }
            | DisplayRow::OpLogDetailLine { op_log_idx, .. } => *op_log_idx,
            _ => return None,
        };
        self.op_log_entries.get(op_log_idx.raw())
    }

    pub fn selected_evolog_entry(&self) -> Option<&EvoLogEntry> {
        let evolog_idx = match self.rows.get(self.cursor)? {
            DisplayRow::EvoLogItem { evolog_idx }
            | DisplayRow::EvoLogFileChange { evolog_idx, .. }
            | DisplayRow::EvoLogFileDiffLine { evolog_idx, .. }
            | DisplayRow::EvoLogGraphLink { evolog_idx, .. } => *evolog_idx,
            _ => return None,
        };
        self.evolog_entries.get(evolog_idx.raw())
    }

    pub fn selected_workspace_entry(&self) -> Option<&WorkspaceViewEntry> {
        let workspace_idx = match self.rows.get(self.cursor)? {
            DisplayRow::WorkspaceItem { workspace_idx } => *workspace_idx,
            _ => return None,
        };
        self.workspace_entries.get(workspace_idx.raw())
    }

    /// Get the entry idx the cursor is on.
    pub fn selected_entry_idx(&self) -> Option<EntryIdx> {
        let entry_idx = match self.rows.get(self.cursor)? {
            DisplayRow::CommitNode { entry_idx }
            | DisplayRow::DescriptionLine { entry_idx, .. }
            | DisplayRow::GraphLink { entry_idx, .. }
            | DisplayRow::FileChange { entry_idx, .. }
            | DisplayRow::DiffLine { entry_idx, .. } => *entry_idx,
            DisplayRow::BookmarkItem { .. }
            | DisplayRow::BookmarkConflictTarget { .. }
            | DisplayRow::BookmarkRemoteTarget { .. }
            | DisplayRow::TagItem { .. }
            | DisplayRow::TagRemoteTarget { .. }
            | DisplayRow::OpLogItem { .. }
            | DisplayRow::OpLogDetailLine { .. }
            | DisplayRow::OpLogGraphLink { .. }
            | DisplayRow::OpLogLoadMore
            | DisplayRow::EvoLogItem { .. }
            | DisplayRow::EvoLogFileChange { .. } | DisplayRow::EvoLogFileDiffLine { .. }
            | DisplayRow::EvoLogGraphLink { .. }
            | DisplayRow::WorkspaceItem { .. }
            | DisplayRow::ConflictHeader { .. }
            | DisplayRow::ConflictSide { .. }
            | DisplayRow::ConflictContext { .. } => return None,
        };
        Some(entry_idx)
    }

    /// Get the change ID (unique prefix) of the commit the cursor is on.
    pub fn selected_change_id(&self) -> Option<ChangeId> {
        let entry_idx = self.selected_entry_idx()?;
        Some(self.nodes[entry_idx].commit.unique_prefix())
    }

    /// Whether the commit the cursor is on is a merge (multiple parents).
    pub fn selected_is_merge(&self) -> bool {
        self.selected_entry_idx()
            .is_some_and(|idx| self.nodes[idx].commit.is_merge)
    }

    /// Get the bookmarks of the commit the cursor is on.
    pub fn selected_bookmarks(&self) -> Option<&[crate::dag::BookmarkInfo]> {
        let entry_idx = self.selected_entry_idx()?;
        Some(&self.nodes[entry_idx].commit.bookmarks)
    }

    /// Get the tags of the commit the cursor is on.
    pub fn selected_tags(&self) -> Option<&[crate::types::Str]> {
        let entry_idx = self.selected_entry_idx()?;
        Some(&self.nodes[entry_idx].commit.tags)
    }

    /// Get the description of the commit the cursor is on.
    pub fn selected_description(&self) -> Option<&str> {
        let entry_idx = self.selected_entry_idx()?;
        self.nodes[entry_idx].commit.description.as_deref()
    }

    pub(crate) fn commit_id(&self, entry_idx: EntryIdx) -> &CommitId {
        &self.nodes[entry_idx].commit.graph_id
    }

    pub(crate) fn change_id(&self, entry_idx: EntryIdx) -> ChangeId {
        self.nodes[entry_idx].commit.unique_change_id()
    }

    pub(crate) fn file_fold_key(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> Option<FileFoldKey> {
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
        self.nodes[entry_idx].files.loaded()
    }

    pub fn diff_lines(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> Option<&Vec<DiffLine>> {
        self.nodes[entry_idx].diffs.get(file_idx.raw())?.loaded()
    }

    pub fn commit_stats(&self, entry_idx: EntryIdx) -> Option<LineStats> {
        self.nodes[entry_idx].stats.loaded().copied()
    }

    /// Find the file index for a given path within a commit's loaded files.
    pub fn file_idx_by_path(&self, entry_idx: EntryIdx, path: &RepoPath) -> Option<FileIdx> {
        let files = self.files_for_entry(entry_idx)?;
        files.iter().position(|f| f.path == *path).map(FileIdx::new)
    }

    pub fn take_repo_requests(&mut self) -> Vec<RepoRequest> {
        std::mem::take(&mut self.pending_repo_requests)
    }

    /// Get effective search scopes for a view (live value for active view,
    /// saved value for others).
    fn effective_scopes(&self, view: ActiveView) -> SearchScopes {
        if self.active_view == view {
            self.search_scopes
        } else {
            self.view_states[view.idx()].search_scopes
        }
    }

    pub fn to_persisted_state(&self) -> PersistedState {
        PersistedState {
            show_line_numbers: self.show_line_numbers,
            ignore_immutable: self.toggles.contains(CommandFlags::IGNORE_IMMUTABLE),
            ignore_working_copy: self.toggles.contains(CommandFlags::IGNORE_WORKING_COPY),
            debug: self.toggles.contains(CommandFlags::DEBUG),
            search_scopes: self.effective_scopes(ActiveView::Dag).bits(),
            bookmark_search_scopes: self.effective_scopes(ActiveView::Bookmarks).bits(),
            tag_search_scopes: self.effective_scopes(ActiveView::Tags).bits(),
            op_log_search_scopes: self.effective_scopes(ActiveView::Operations).bits(),
            workspace_search_scopes: self.effective_scopes(ActiveView::Workspaces).bits(),
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
            let scopes = SearchScopes::from_bits_truncate(state.search_scopes);
            self.view_states[ActiveView::Dag.idx()].search_scopes = scopes;
            self.search_scopes = scopes; // DAG is default active view
        }
        if state.bookmark_search_scopes != 0 {
            self.view_states[ActiveView::Bookmarks.idx()].search_scopes =
                SearchScopes::from_bits_truncate(state.bookmark_search_scopes);
        }
        if state.tag_search_scopes != 0 {
            self.view_states[ActiveView::Tags.idx()].search_scopes =
                SearchScopes::from_bits_truncate(state.tag_search_scopes);
        }
        if state.op_log_search_scopes != 0 {
            self.view_states[ActiveView::Operations.idx()].search_scopes =
                SearchScopes::from_bits_truncate(state.op_log_search_scopes);
        }
        if state.workspace_search_scopes != 0 {
            self.view_states[ActiveView::Workspaces.idx()].search_scopes =
                SearchScopes::from_bits_truncate(state.workspace_search_scopes);
        }
        self.active_preset = state.active_preset.filter(|&i| i < self.presets.len());
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

    /// Look up the entry index for a commit by its graph_id.
    pub fn entry_by_commit_id(&self, commit_id: &CommitId) -> Option<EntryIdx> {
        self.commit_index.get(commit_id).copied()
    }
}

fn build_commit_index(entries: &IndexVec<EntryIdx, DagEntry>) -> HashMap<CommitId, EntryIdx> {
    entries
        .iter_enumerated()
        .map(|(idx, e)| (e.commit.graph_id.clone(), idx))
        .collect()
}

/// Build consolidated `DagNode` vec from transport `DagEntry` vec, resolving
/// edges to `EntryIdx` and computing graph lines.
fn build_nodes(
    entries: IndexVec<EntryIdx, DagEntry>,
    commit_index: &HashMap<CommitId, EntryIdx>,
    glyphs: &crate::theme::GlyphChars,
) -> IndexVec<EntryIdx, DagNode> {
    let graph_lines = graph::render(entries.as_slice(), glyphs);

    let node_vec: Vec<DagNode> = entries
        .into_vec()
        .into_iter()
        .zip(graph_lines)
        .map(|(entry, gl)| {
            let parents: SmallVec<EntryIdx> = entry
                .edges
                .iter()
                .filter(|e| matches!(e.kind, EdgeKind::Direct))
                .filter_map(|e| commit_index.get(&e.target).copied())
                .collect();
            DagNode {
                commit: entry.commit,
                graph: gl,
                parents,
                children: SmallVec::new(),
                row: 0,
                files: Loadable::NotRequested,
                stats: Loadable::NotRequested,
                diffs: Vec::new(),
                conflict_hunks: Vec::new(),
            }
        })
        .collect();
    let mut nodes: IndexVec<EntryIdx, DagNode> = IndexVec::from_vec(node_vec);

    // Second pass: fill children from resolved parents.
    for idx_raw in 0..nodes.len() {
        let idx = EntryIdx::new(idx_raw);
        let parents: SmallVec<EntryIdx> = nodes[idx].parents.clone();
        for parent_idx in parents {
            nodes[parent_idx].children.push(idx);
        }
    }

    nodes
}
