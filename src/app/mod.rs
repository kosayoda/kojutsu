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
    ChangeId, CommitId, DisplayRow, RepoPath, SearchScopes, SearchState, SelectionContext,
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
    /// Lazily loaded diff lines (git format), parallel to `files` (indexed by FileIdx).
    pub diffs: Vec<Loadable<Vec<DiffLine>>>,
    /// Lazily loaded diff lines (color-words format), parallel to `diffs`.
    pub diffs_cw: Vec<Loadable<Vec<DiffLine>>>,
    /// Lazily loaded conflict hunks, parallel to `files` (for conflicted files).
    pub conflict_hunks: Vec<Loadable<Vec<crate::dag::ConflictHunk>>>,
}

impl DagNode {
    /// Ensure the diffs vector is large enough to hold `n` entries.
    pub fn ensure_diffs(&mut self, n: usize) {
        if self.diffs.len() < n {
            self.diffs.resize_with(n, || Loadable::NotRequested);
        }
        if self.diffs_cw.len() < n {
            self.diffs_cw.resize_with(n, || Loadable::NotRequested);
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// Data for bookmark/tag/workspace views.
    pub views: ViewData,
    /// Operation log view state.
    pub op_log: OpLogState,
    /// Evolution log view state.
    pub evolog: EvoLogState,
    /// Command log view state.
    pub command_log: CommandLogState,
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
    /// Revset configuration and state.
    pub revset: RevsetConfig,
    /// Glyph characters for DAG rendering.
    pub glyphs: &'static crate::theme::GlyphChars,
    /// Default search scopes from config.
    pub default_search_scopes: SearchScopes,
    pub repo_root: String,
    /// Current interaction mode.
    pub mode: AppMode,
    /// Per-commit fold state, keyed by change id (stable across mutations).
    pub unfolded_commits: HashSet<ChangeId>,
    /// Per-file fold state, keyed by (change id, path) (stable across mutations).
    pub(crate) unfolded_files: HashSet<FileFoldKey>,
    /// Repo requests waiting to be sent to the background service.
    pending_repo_requests: Vec<RepoRequest>,
    /// Global toggles that persist across commands.
    pub toggles: CommandFlags,
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
    /// Visual selection state.
    pub visual: VisualState,
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
            views: ViewData::new(),
            op_log: OpLogState::new(),
            evolog: EvoLogState::new(),
            command_log: CommandLogState::new(),
            rows: Vec::new(),
            cursor: 0,
            view_states: default_view_states(),
            list_state: ListState::default(),
            last_header_height: 2,
            last_list_height: 0,
            h_scroll: 0,
            revset: RevsetConfig {
                current: revset.into(),
                draft: None,
                load_state: Loadable::NotRequested,
                pending: None,
                active_preset: None,
                presets,
            },
            glyphs,
            default_search_scopes: SearchScopes::DEFAULT,
            repo_root,
            mode: AppMode::Normal,
            unfolded_commits: HashSet::new(),
            unfolded_files: HashSet::new(),
            pending_repo_requests: Vec::new(),
            toggles: CommandFlags::empty(),
            status_message: None,
            pre_overlay_mode: None,
            show_line_numbers: false,
            selection: SelectionContext::new(),
            visual: VisualState::new(),
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

    pub fn push_command_log(
        &mut self,
        kind: CommandLogKind,
        summary: impl Into<String>,
        command_parts: Option<Vec<crate::jj_command::CommandPart>>,
        output: Vec<u8>,
        success: bool,
    ) {
        self.command_log.entries.push(CommandLogEntry {
            kind,
            summary: summary.into(),
            command_parts,
            output,
            success,
            timestamp: jiff::Timestamp::now(),
        });
        if self.active_view == ActiveView::CommandLog {
            self.rebuild_rows();
        }
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
        // Allow evolog → evolog (reload with different commit).
        if self.active_view == view && view != ActiveView::Evolog {
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
        if view == ActiveView::Operations && !self.op_log.loaded {
            self.pending_repo_requests
                .push(RepoRequest::load_operations(self.op_log.limit));
        }
        // Trigger lazy load of evolog data.
        if view == ActiveView::Evolog {
            // Get the commit ID from the DAG (if switching from DAG) or from
            // the selected evolog entry (if switching from within evolog).
            let commit_id = self
                .selected_entry_idx()
                .map(|idx| self.nodes[idx].commit.graph_id.clone())
                .or_else(|| self.selected_evolog_entry().map(|e| e.commit_id.clone()));
            if let Some(commit_id) = commit_id {
                let changed = self.evolog.commit_id.as_ref() != Some(&commit_id);
                if changed || !self.evolog.loaded {
                    self.evolog.clear();
                    self.evolog.commit_id = Some(commit_id.clone());
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
            self.revset.current,
            change_id,
            Self::ANCESTOR_EXPAND_COUNT,
        );
        self.jump_after_refresh = Some(JumpTarget::ChangeId(change_id.to_string()));
        self.revset.load_state = Loadable::Loading;
        self.revset.pending = Some(new_revset.clone().into());
        self.pending_repo_requests
            .push(RepoRequest::load_revset(Some(new_revset)));
    }

    pub fn request_op_log_load_more(&mut self) {
        self.op_log.limit += OP_LOG_BATCH_SIZE;
        self.op_log.loaded = false;
        self.pending_repo_requests
            .push(RepoRequest::load_operations(self.op_log.limit));
    }

    pub fn selected_bookmark_entry(&self) -> Option<&BookmarkViewEntry> {
        let bookmark_idx = match self.rows.get(self.cursor)? {
            DisplayRow::BookmarkItem { bookmark_idx }
            | DisplayRow::BookmarkConflictTarget { bookmark_idx, .. }
            | DisplayRow::BookmarkRemoteTarget { bookmark_idx, .. } => *bookmark_idx,
            _ => return None,
        };
        self.views.bookmark_entries.get(bookmark_idx.raw())
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
        let entry = self.views.bookmark_entries.get(bookmark_idx.raw())?;
        let target = self
            .views
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
        let entry = self.views.bookmark_entries.get(bookmark_idx.raw())?;
        let target = self
            .views
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
        self.views.tag_entries.get(tag_idx.raw())
    }

    /// Pick a conflict side for a hunk. Returns whether the file was fully
    /// resolved (all hunks picked) and written to disk.
    pub fn pick_conflict_side(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        hunk_idx: crate::idx::ConflictHunkIdx,
        side: usize,
    ) -> ConflictPickResult {
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
            return ConflictPickResult::Pending;
        };
        let Some(hunk) = hunks.get_mut(hi) else {
            return ConflictPickResult::Pending;
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

        let result = if all_resolved {
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
            let written = if let Some(file_path) = path {
                let full_path = std::path::Path::new(&self.repo_root).join(file_path.as_str());
                if std::fs::write(&full_path, &content).is_ok() {
                    self.set_status(format!("resolved {}", file_path));
                    true
                } else {
                    self.set_error(format!("failed to write {}", file_path));
                    false
                }
            } else {
                false
            };
            if written {
                ConflictPickResult::FileResolved
            } else {
                // Revert the selection since the file wasn't written.
                if let Some(Loadable::Loaded(hunks)) =
                    self.nodes[entry_idx].conflict_hunks.get_mut(fi)
                {
                    if let Some(hunk) = hunks.get_mut(hi) {
                        if let crate::dag::ConflictHunkKind::Conflict { selected, .. } =
                            &mut hunk.kind
                        {
                            *selected = None;
                        }
                    }
                }
                ConflictPickResult::Pending
            }
        } else {
            ConflictPickResult::Pending
        };

        self.rebuild_rows();
        result
    }

    pub fn selected_op_log_entry(&self) -> Option<&OpLogEntry> {
        let op_log_idx = match self.rows.get(self.cursor)? {
            DisplayRow::OpLogItem { op_log_idx }
            | DisplayRow::OpLogDetailLine { op_log_idx, .. } => *op_log_idx,
            _ => return None,
        };
        self.op_log.entries.get(op_log_idx.raw())
    }

    pub fn selected_evolog_entry(&self) -> Option<&EvoLogEntry> {
        let evolog_idx = match self.rows.get(self.cursor)? {
            DisplayRow::EvoLogItem { evolog_idx }
            | DisplayRow::EvoLogFileChange { evolog_idx, .. }
            | DisplayRow::EvoLogFileDiffLine { evolog_idx, .. }
            | DisplayRow::EvoLogGraphLink { evolog_idx, .. } => *evolog_idx,
            _ => return None,
        };
        self.evolog.entries.get(evolog_idx.raw())
    }

    pub fn selected_workspace_entry(&self) -> Option<&WorkspaceViewEntry> {
        let workspace_idx = match self.rows.get(self.cursor)? {
            DisplayRow::WorkspaceItem { workspace_idx } => *workspace_idx,
            _ => return None,
        };
        self.views.workspace_entries.get(workspace_idx.raw())
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
            | DisplayRow::EvoLogFileChange { .. }
            | DisplayRow::EvoLogFileDiffLine { .. }
            | DisplayRow::EvoLogGraphLink { .. }
            | DisplayRow::WorkspaceItem { .. }
            | DisplayRow::CommandLogItem { .. }
            | DisplayRow::CommandLogDetail { .. }
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
        let git_diff = self.toggles.contains(crate::keymap::CommandFlags::GIT_DIFF);
        if git_diff {
            self.nodes[entry_idx].diffs.get(file_idx.raw())?.loaded()
        } else {
            self.nodes[entry_idx].diffs_cw.get(file_idx.raw())?.loaded()
        }
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
            active_preset: self.revset.active_preset,
            git_diff: self.toggles.contains(CommandFlags::GIT_DIFF),
        }
    }

    pub fn apply_persisted_state(&mut self, state: &PersistedState) {
        self.show_line_numbers = state.show_line_numbers;
        self.toggles
            .set(CommandFlags::IGNORE_IMMUTABLE, state.ignore_immutable);
        self.toggles
            .set(CommandFlags::IGNORE_WORKING_COPY, state.ignore_working_copy);
        self.toggles.set(CommandFlags::DEBUG, state.debug);
        self.toggles.set(CommandFlags::GIT_DIFF, state.git_diff);
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
        self.revset.active_preset = state
            .active_preset
            .filter(|&i| i < self.revset.presets.len());
    }

    /// Get the scroll offset from the list state.
    pub fn scroll_offset(&self) -> usize {
        self.list_state.offset()
    }

    /// Get the text to pre-fill the revset input with.
    /// Uses the last failed draft if one exists, otherwise the current revset.
    pub fn revset_input_text(&self) -> &str {
        self.revset.draft.as_deref().unwrap_or(&self.revset.current)
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
                diffs_cw: Vec::new(),
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
