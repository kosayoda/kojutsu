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
use crate::idx::{EntryIdx, EvoLogIdx, FileIdx, IndexVec, RowIdx};
use crate::types::SmallVec;

use crate::keymap::CommandFlags;
use crate::repo_service::{RepoError, RepoRequest};
use crate::types::{
    ChangeId, CommitId, DisplayRow, JumpTarget, RepoPath, SearchScopes, SearchState,
    SelectionContext,
};

#[derive(Clone, Debug)]
pub enum Loadable<T> {
    NotRequested,
    Loading,
    Loaded(T),
    Failed(RepoError),
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
    /// Only valid immediately after `rebuild_rows()`. Use `row_of_commit()` externally.
    pub(super) row: usize,
    /// Lazily loaded file changes for this commit.
    pub files: Loadable<Vec<FileChange>>,
    /// Lazily loaded per-commit line stats.
    pub stats: Loadable<LineStats>,
    /// Lazily loaded diff results (both formats), parallel to `files` (indexed by FileIdx).
    diffs: Vec<Loadable<crate::dag::DiffResult>>,
    /// Lazily loaded conflict hunks, parallel to `files` (for conflicted files).
    conflict_hunks: Vec<Loadable<Vec<crate::dag::ConflictHunkKind>>>,
}

/// Which diff format to display.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiffFormat {
    Git,
    ColorWords,
}

impl std::fmt::Debug for DagNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DagNode")
            .field("commit", &self.commit)
            .field("parents", &self.parents)
            .field("children", &self.children)
            .field("row", &self.row)
            .finish_non_exhaustive()
    }
}

impl DagNode {
    /// Get the loaded diff lines for a file in the given format.
    pub fn diff(&self, fi: FileIdx, format: DiffFormat) -> Option<&Vec<DiffLine>> {
        Some(self.diffs.get(fi.raw())?.loaded()?.lines(format))
    }

    /// Get the raw diff result state for a file.
    pub fn diff_result(&self, fi: FileIdx) -> Option<&Loadable<crate::dag::DiffResult>> {
        self.diffs.get(fi.raw())
    }

    /// Get loaded conflict hunks for a file.
    pub fn conflict_hunks(
        &self,
        fi: FileIdx,
    ) -> Option<&Loadable<Vec<crate::dag::ConflictHunkKind>>> {
        self.conflict_hunks.get(fi.raw())
    }

    /// Get mutable conflict hunks for a file.
    pub fn conflict_hunks_mut(
        &mut self,
        fi: FileIdx,
    ) -> Option<&mut Loadable<Vec<crate::dag::ConflictHunkKind>>> {
        self.conflict_hunks.get_mut(fi.raw())
    }

    /// Set the diff result for a file, growing the vector if needed.
    pub fn set_diff(&mut self, fi: FileIdx, result: crate::dag::DiffResult) {
        let i = fi.raw();
        self.ensure_diffs(i + 1);
        self.diffs[i] = Loadable::Loaded(result);
    }

    /// Set the diff state for a file (e.g. Loading/Failed).
    pub fn set_diff_state(&mut self, fi: FileIdx, state: Loadable<crate::dag::DiffResult>) {
        let i = fi.raw();
        self.ensure_diffs(i + 1);
        self.diffs[i] = state;
    }

    /// Set the conflict hunks state for a file, growing the vector if needed.
    pub fn set_conflict_hunks(
        &mut self,
        fi: FileIdx,
        state: Loadable<Vec<crate::dag::ConflictHunkKind>>,
    ) {
        let i = fi.raw();
        self.ensure_conflict_hunks(i + 1);
        self.conflict_hunks[i] = state;
    }

    /// Extract and take ownership of cached diffs (used during DAG refresh).
    pub fn take_diffs(&mut self) -> Vec<Loadable<crate::dag::DiffResult>> {
        std::mem::take(&mut self.diffs)
    }

    /// Preserve cached diffs from another node (used during DAG refresh).
    pub fn restore_diffs(&mut self, diffs: Vec<Loadable<crate::dag::DiffResult>>) {
        self.diffs = diffs;
    }

    /// Check if a diff should be requested for a file.
    pub fn diff_should_request(&self, fi: FileIdx) -> bool {
        self.diffs
            .get(fi.raw())
            .is_none_or(Loadable::should_request)
    }

    /// Check if conflict hunks should be requested for a file.
    pub fn conflict_hunks_should_request(&self, fi: FileIdx) -> bool {
        self.conflict_hunks
            .get(fi.raw())
            .is_none_or(Loadable::should_request)
    }

    fn ensure_diffs(&mut self, n: usize) {
        if self.diffs.len() < n {
            self.diffs.resize_with(n, || Loadable::NotRequested);
        }
    }

    fn ensure_conflict_hunks(&mut self, n: usize) {
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
    /// Interdiff view state.
    pub interdiff: InterdiffState,
    /// Annotate (blame) view state.
    pub annotate: AnnotateState,
    /// Flattened display rows (one per visual line).
    pub rows: Vec<DisplayRow>,
    /// Index into `rows` of the currently selected row.
    pub cursor: RowIdx,
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
    /// Which diff format to display (git vs color-words).
    pub diff_format: DiffFormat,
    /// Transient status notice shown in the status bar.
    pub status_message: Option<(String, StatusLevel)>,
    /// Label of the last dispatched action (for Lua post-hooks).
    pub last_action_label: Option<&'static str>,
    /// Mode to restore after an overlay (search/help) is dismissed.
    /// Used when search or help is entered from TargetSelect/CommitSelect.
    pub pre_overlay_mode: Option<AppMode>,
    /// Whether to show line numbers in diff views.
    pub show_line_numbers: bool,
    /// Whether to underline changed tokens in word-level diffs.
    pub diff_underline: bool,
    /// Current selection context: implicit commit under cursor, or explicit
    /// homogeneous file/line selection.
    pub selection: SelectionContext,
    /// Visual selection state.
    pub visual: VisualState,
    /// Active search state. Search remains active after closing the input.
    pub search: Option<SearchState>,
    /// Where to jump the cursor after the next DAG refresh.
    pub jump_after_refresh: Option<JumpTarget>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("active_view", &self.active_view)
            .field("nodes_len", &self.nodes.len())
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
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
            interdiff: InterdiffState::new(),
            annotate: AnnotateState::new(),
            rows: Vec::new(),
            cursor: RowIdx::new(0),
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
            diff_format: DiffFormat::ColorWords,
            status_message: None,
            last_action_label: None,
            pre_overlay_mode: None,
            show_line_numbers: false,
            diff_underline: true,
            selection: SelectionContext::new(),
            visual: VisualState::new(),
            search: None,
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

    pub fn clear_info_status(&mut self) {
        if matches!(self.status_message, Some((_, StatusLevel::Info))) {
            self.status_message = None;
        }
    }

    /// Log a background error and show it in the status bar.
    pub fn log_background_error(&mut self, summary: impl Into<String>) {
        let msg = summary.into();
        self.push_command_log(CommandLogKind::Background, &msg, None, Vec::new(), false);
        self.set_error(msg);
    }

    /// Log an error and show it as a full-screen CommandOutput overlay.
    pub fn show_error_overlay(
        &mut self,
        summary: impl Into<String>,
        error: crate::repo_service::RepoError,
    ) {
        let summary = summary.into();
        self.push_command_log(
            CommandLogKind::Background,
            &summary,
            None,
            error.message.as_bytes().to_vec(),
            false,
        );
        self.mode = AppMode::CommandOutput {
            command_parts: None,
            command: summary,
            output: error.message.into_bytes(),
            success: false,
            retry: vec![],
        };
    }

    /// Enter an overlay mode (search, help) that may need to restore the
    /// previous mode on exit. Saves TargetSelect/CommitSelect modes so they
    /// survive the overlay; Normal/Submenu are discarded.
    pub fn enter_overlay(&mut self, overlay: AppMode) {
        let old = std::mem::replace(&mut self.mode, overlay);
        self.pre_overlay_mode = match old {
            AppMode::Normal | AppMode::Submenu { .. } => None,
            other => Some(other),
        };
    }

    /// Exit an overlay mode, restoring the previous mode if one was saved.
    pub fn exit_overlay(&mut self) {
        self.mode = self.pre_overlay_mode.take().unwrap_or(AppMode::Normal);
    }

    /// Search scopes for the active view (single source of truth in ViewState).
    pub fn search_scopes(&self) -> SearchScopes {
        self.view_states[self.active_view.idx()].search_scopes
    }

    /// Mutably access search scopes for the active view.
    pub fn search_scopes_mut(&mut self) -> &mut SearchScopes {
        &mut self.view_states[self.active_view.idx()].search_scopes
    }

    pub fn push_command_log(
        &mut self,
        kind: CommandLogKind,
        summary: impl Into<String>,
        command_parts: Option<Vec<crate::jj_command::CommandPart>>,
        output: Vec<u8>,
        success: bool,
    ) {
        use ansi_to_tui::IntoText as _;
        let parsed_lines = output
            .as_slice()
            .into_text()
            .map(|t| t.lines)
            .unwrap_or_default();
        self.command_log.entries.push(CommandLogEntry {
            kind,
            summary: summary.into(),
            command_parts,
            output,
            parsed_lines,
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
    pub fn row_of_commit(&self, entry_idx: EntryIdx) -> Option<RowIdx> {
        Some(RowIdx::new(self.nodes.get(entry_idx)?.row))
    }

    pub fn switch_view(&mut self, view: ActiveView) {
        // Allow evolog/interdiff → same (reload with different commit).
        if self.active_view == view
            && view != ActiveView::Evolog
            && view != ActiveView::Interdiff
            && view != ActiveView::Annotate
        {
            return;
        }
        // Save current view state.
        let offset = self.scroll_offset();
        let vs = &mut self.view_states[self.active_view.idx()];
        vs.cursor = self.cursor;
        vs.scroll_offset = offset;
        vs.h_scroll = self.h_scroll;

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
        self.cursor = RowIdx::new(vs.cursor.raw().min(self.rows.len().saturating_sub(1)));
        *self.list_state.offset_mut() = vs.scroll_offset;
        self.h_scroll = vs.h_scroll;
    }

    /// Enter the interdiff view comparing two commits.
    pub fn enter_interdiff_view(
        &mut self,
        from: CommitId,
        to: CommitId,
        from_label: crate::types::Str,
        to_label: crate::types::Str,
    ) {
        self.interdiff.clear();
        self.interdiff.target = Some(crate::app::types::InterdiffTarget {
            from_commit_id: from.clone(),
            to_commit_id: to.clone(),
            from_label,
            to_label,
        });
        self.interdiff.files = Loadable::Loading;
        self.pending_repo_requests
            .push(crate::repo_service::RepoRequest::load_interdiff_details(
                from, to,
            ));
        self.switch_view(ActiveView::Interdiff);
    }

    /// Get the diff lines for an interdiff file.
    pub fn interdiff_diff_lines(
        &self,
        file_idx: crate::idx::FileIdx,
    ) -> Option<&Vec<crate::dag::DiffLine>> {
        let files = self.interdiff.files.loaded()?;
        let file = files.get(file_idx.raw())?;
        let diff = self.interdiff.file_diffs.get(&file.path)?.loaded()?;
        Some(diff.lines(self.diff_format))
    }

    /// Enter the annotate (blame) view for a file at a specific commit.
    pub fn request_file_list(&mut self, commit_id: CommitId) {
        self.pending_repo_requests
            .push(crate::repo_service::RepoRequest::load_file_list(commit_id));
    }

    pub fn enter_annotate_view(&mut self, commit_id: CommitId, path: crate::types::RepoPath) {
        self.annotate.clear();
        self.annotate.target = Some(crate::app::types::AnnotateTarget {
            commit_id: commit_id.clone(),
            path: path.clone(),
        });
        self.annotate.lines = Loadable::Loading;
        self.pending_repo_requests
            .push(crate::repo_service::RepoRequest::load_file_annotate(
                commit_id, path,
            ));
        self.switch_view(ActiveView::Annotate);
    }

    /// Navigate to a different commit within the annotate view (time travel).
    /// Preserves the history stack for backtracking.
    pub fn annotate_navigate(&mut self, commit_id: CommitId, target_line: usize) {
        let Some(path) = self.annotate.target.as_ref().map(|t| t.path.clone()) else {
            return;
        };
        self.annotate.clear_keep_history();
        self.annotate.target = Some(crate::app::types::AnnotateTarget {
            commit_id: commit_id.clone(),
            path: path.clone(),
        });
        self.annotate.lines = Loadable::Loading;
        self.annotate.target_line = Some(target_line);
        self.pending_repo_requests
            .push(crate::repo_service::RepoRequest::load_file_annotate(
                commit_id, path,
            ));
        self.rebuild_rows();
    }

    /// Get the selected annotate line data (if cursor is on an annotate line).
    pub fn selected_annotate_line(&self) -> Option<&crate::dag::AnnotateLineData> {
        let line_idx = match self.rows.get(self.cursor.raw())? {
            DisplayRow::AnnotateLine { line_idx } => *line_idx,
            _ => return None,
        };
        self.annotate.lines.loaded()?.get(line_idx.raw())
    }

    /// Number of ancestor generations to load when expanding at a terminator.
    const ANCESTOR_EXPAND_COUNT: usize = 10;

    /// Widen the revset to include ancestors of the given commit.
    pub fn expand_ancestors(&mut self, entry_idx: crate::idx::EntryIdx) {
        let change_id = self.change_id(entry_idx);
        let change_str = change_id.to_string();
        let pattern = format!("ancestors({change_str}, ");

        let new_revset = if let Some(pos) = self.revset.current.find(&pattern) {
            let after_prefix = &self.revset.current[pos + pattern.len()..];
            if let Some(end) = after_prefix.find(')') {
                if let Ok(current_count) = after_prefix[..end].parse::<usize>() {
                    let new_count = current_count + Self::ANCESTOR_EXPAND_COUNT;
                    let mut revset = self.revset.current.to_string();
                    let num_start = pos + pattern.len();
                    let num_end = num_start + end;
                    revset.replace_range(num_start..num_end, &new_count.to_string());
                    revset
                } else {
                    format!(
                        "({}) | ancestors({change_str}, {})",
                        self.revset.current,
                        Self::ANCESTOR_EXPAND_COUNT,
                    )
                }
            } else {
                format!(
                    "({}) | ancestors({change_str}, {})",
                    self.revset.current,
                    Self::ANCESTOR_EXPAND_COUNT,
                )
            }
        } else {
            format!(
                "({}) | ancestors({change_str}, {})",
                self.revset.current,
                Self::ANCESTOR_EXPAND_COUNT,
            )
        };

        self.jump_after_refresh = Some(JumpTarget::Prefix(change_str));
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
        let bookmark_idx = self.rows.get(self.cursor.raw())?.bookmark_idx()?;
        self.views.bookmark_entries.get(bookmark_idx.raw())
    }

    /// Get the conflict target under cursor (if on a `BookmarkConflictTarget` row).
    pub fn selected_conflict_target(
        &self,
    ) -> Option<(&BookmarkViewEntry, &crate::dag::BookmarkConflictTarget)> {
        let (bookmark_idx, target_idx) = match self.rows.get(self.cursor.raw())? {
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
        let (bookmark_idx, target_idx) = match self.rows.get(self.cursor.raw())? {
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
        let remote = entry.kind.remote()?.clone();
        Some(crate::dag::BookmarkRef {
            name: entry.name.clone(),
            remote,
        })
    }

    pub fn selected_tag_entry(&self) -> Option<&TagViewEntry> {
        let tag_idx = self.rows.get(self.cursor.raw())?.tag_idx()?;
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
        let hi = hunk_idx.raw();
        let Some(hunks) =
            self.nodes[entry_idx]
                .conflict_hunks_mut(file_idx)
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
        } = hunk
        {
            if side < sides.len() {
                *selected = Some(side);
            }
        }

        // Check if all conflict hunks are now resolved.
        let all_resolved = hunks.iter().all(|h| match h {
            crate::dag::ConflictHunkKind::Resolved { .. } => true,
            crate::dag::ConflictHunkKind::Conflict { selected, .. } => selected.is_some(),
        });

        let result = if all_resolved {
            // Assemble resolved content.
            let mut content = String::new();
            for h in hunks.iter() {
                match h {
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
            // Return the resolved content for the caller to write.
            if let Some(path) = self
                .files_for_entry(entry_idx)
                .and_then(|f| f.get(file_idx.raw()))
                .map(|f| f.path.clone())
            {
                ConflictPickResult::FileResolved { path, content }
            } else {
                ConflictPickResult::Pending
            }
        } else {
            ConflictPickResult::Pending
        };

        self.rebuild_rows();
        result
    }

    pub fn selected_op_log_entry(&self) -> Option<&OpLogEntry> {
        let op_log_idx = self.rows.get(self.cursor.raw())?.op_log_idx()?;
        self.op_log.entries.get(op_log_idx.raw())
    }

    pub fn selected_evolog_entry(&self) -> Option<&EvoLogEntry> {
        let evolog_idx = self.rows.get(self.cursor.raw())?.evolog_idx()?;
        self.evolog.entries.get(evolog_idx.raw())
    }

    pub fn selected_workspace_entry(&self) -> Option<&WorkspaceViewEntry> {
        let workspace_idx = self.rows.get(self.cursor.raw())?.workspace_idx()?;
        self.views.workspace_entries.get(workspace_idx.raw())
    }

    /// Get the entry idx the cursor is on.
    pub fn selected_entry_idx(&self) -> Option<EntryIdx> {
        self.rows.get(self.cursor.raw())?.entry_idx()
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
    pub fn selected_tags(&self) -> Option<&[crate::types::TagName]> {
        let entry_idx = self.selected_entry_idx()?;
        Some(&self.nodes[entry_idx].commit.tags)
    }

    /// Get the description of the commit the cursor is on.
    pub fn selected_description(&self) -> Option<&str> {
        let entry_idx = self.selected_entry_idx()?;
        self.nodes[entry_idx].commit.description.as_deref()
    }

    /// Get the file path under the cursor (if on a file or diff line row).
    pub fn selected_file_path(&self) -> Option<&crate::types::RepoPath> {
        let (entry_idx, file_idx) = self.rows.get(self.cursor.raw())?.dag_file()?;
        Some(&self.files_for_entry(entry_idx)?.get(file_idx.raw())?.path)
    }

    /// Whether the cursor is on a working copy commit.
    pub fn selected_is_working_copy(&self) -> bool {
        self.selected_entry_idx()
            .is_some_and(|idx| self.nodes[idx].commit.is_working_copy())
    }

    /// Whether the cursor's commit has conflicts.
    pub fn selected_has_conflict(&self) -> bool {
        self.selected_entry_idx()
            .is_some_and(|idx| self.nodes[idx].commit.has_conflict)
    }

    /// Whether the cursor's commit is empty.
    pub fn selected_is_empty(&self) -> bool {
        self.selected_entry_idx()
            .is_some_and(|idx| self.nodes[idx].commit.is_empty)
    }

    pub(crate) fn commit_id(&self, entry_idx: EntryIdx) -> &CommitId {
        &self.nodes[entry_idx].commit.graph_id
    }

    pub(crate) fn change_id(&self, entry_idx: EntryIdx) -> ChangeId {
        self.nodes[entry_idx].commit.unique_change_id()
    }

    /// Resolve a change ID (prefix or full) to its commit (graph) ID via the DAG nodes.
    pub fn commit_id_for_change(&self, change_id: &ChangeId) -> Option<CommitId> {
        self.nodes
            .iter()
            .find(|n| {
                let prefix = n.commit.unique_prefix();
                let full = n.commit.unique_change_id();
                prefix == *change_id || full == *change_id
            })
            .map(|n| n.commit.graph_id.clone())
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

    pub fn diff_format(&self) -> DiffFormat {
        self.diff_format
    }

    pub fn diff_lines(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> Option<&Vec<DiffLine>> {
        let format = self.diff_format();
        self.nodes[entry_idx].diff(file_idx, format)
    }

    pub fn evolog_diff_lines(
        &self,
        evolog_idx: EvoLogIdx,
        file_idx: FileIdx,
    ) -> Option<&Vec<DiffLine>> {
        let entry = self.evolog.entries.get(evolog_idx.raw())?;
        let files = self.evolog.files.get(&entry.commit_id)?.loaded()?;
        let file = files.get(file_idx.raw())?;
        let key = (entry.commit_id.clone(), file.path.clone());
        let result = self.evolog.file_diffs.get(&key)?.loaded()?;
        Some(result.lines(self.diff_format))
    }

    pub fn commit_stats(&self, entry_idx: EntryIdx) -> Option<LineStats> {
        self.nodes[entry_idx].stats.loaded().copied()
    }

    /// Find the file index for a given path within a commit's loaded files.
    pub fn file_idx_by_path(&self, entry_idx: EntryIdx, path: &RepoPath) -> Option<FileIdx> {
        let files = self.files_for_entry(entry_idx)?;
        files.iter().position(|f| f.path == *path).map(FileIdx::new)
    }

    /// Enter jump mode: assign labels to visible jumpable rows.
    ///
    /// Navigation-aware keys (`j`, `k`, `J`, `K`, `0`, `$`, `@`) label the
    /// rows those keys would navigate to.  Remaining targets get single-char
    /// labels by proximity; overflow targets get two-char labels.
    pub fn enter_jump(&mut self) {
        const KEYS: &[char] = &[
            'a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'q', 'w', 'e', 'r', 't', 'y', 'u', 'i',
            'o', 'p', 'z', 'x', 'c', 'v', 'b', 'n', 'm',
        ];

        let (offset, end) = self.visible_row_range();
        let cursor = self.cursor.raw();
        let visible = |idx: RowIdx| idx.raw() >= offset && idx.raw() < end;
        let dist = |i: usize| if i >= cursor { i - cursor } else { cursor - i };

        let nav_candidates: &[(&str, Option<RowIdx>)] = &[
            ("j", self.peek_down()),
            ("k", self.peek_up()),
            ("J", self.peek_down_section()),
            ("K", self.peek_up_section()),
            ("H", self.peek_screen_top()),
            ("M", self.peek_screen_middle()),
            ("L", self.peek_screen_bottom()),
            ("0", self.peek_top()),
            ("$", self.peek_bottom()),
            ("@", self.peek_working_copy()),
        ];

        let mut labels: Vec<(String, RowIdx)> = Vec::new();
        let mut nav_rows: HashSet<RowIdx> = HashSet::new();
        let mut used_chars: HashSet<char> = HashSet::new();

        for &(key, target) in nav_candidates {
            if let Some(idx) = target {
                if visible(idx) && idx != self.cursor && !nav_rows.contains(&idx) {
                    labels.push((key.to_string(), idx));
                    nav_rows.insert(idx);
                    for c in key.chars() {
                        used_chars.insert(c);
                    }
                }
            }
        }

        let pool: Vec<char> = KEYS
            .iter()
            .copied()
            .filter(|c| !used_chars.contains(c))
            .collect();

        let mut targets: Vec<RowIdx> = (offset..end)
            .filter_map(|i| {
                let idx = RowIdx::new(i);
                if idx == self.cursor || nav_rows.contains(&idx) {
                    return None;
                }
                match &self.rows[i] {
                    DisplayRow::CommitNode { .. }
                    | DisplayRow::BookmarkItem { .. }
                    | DisplayRow::TagItem { .. }
                    | DisplayRow::OpLogItem { .. }
                    | DisplayRow::EvoLogItem { .. }
                    | DisplayRow::WorkspaceItem { .. }
                    | DisplayRow::CommandLogItem { .. }
                    | DisplayRow::FileChange { .. }
                    | DisplayRow::EvoLogFileChange { .. }
                    | DisplayRow::InterdiffFileChange { .. }
                    | DisplayRow::AnnotateLine { .. } => Some(idx),
                    _ if self.is_hunk_header(idx) => Some(idx),
                    _ => None,
                }
            })
            .collect();
        targets.sort_by_key(|idx| dist(idx.raw()));

        if !pool.is_empty() {
            let overflow = targets.len().saturating_sub(pool.len());
            let n_groups = if overflow == 0 {
                0
            } else {
                (overflow + pool.len() - 1) / pool.len()
            };
            let n_single = pool.len().saturating_sub(n_groups);

            // Single-char labels for the closest targets.
            for (i, &idx) in targets.iter().take(n_single).enumerate() {
                labels.push((String::from(pool[i]), idx));
            }

            // Two-char labels for overflow. Suffix pool excludes group prefix
            // keys to avoid ambiguity with single-char labels.
            if n_groups > 0 {
                let group_keys = &pool[n_single..n_single + n_groups];
                let suffix_pool: Vec<char> = pool[..n_single].to_vec();
                if !suffix_pool.is_empty() {
                    for (i, &idx) in targets.iter().skip(n_single).enumerate() {
                        let gi = i / suffix_pool.len();
                        let si = i % suffix_pool.len();
                        if gi >= group_keys.len() {
                            break;
                        }
                        labels.push((format!("{}{}", group_keys[gi], suffix_pool[si]), idx));
                    }
                }
            }
        }

        if !labels.is_empty() {
            let restore_mode = match &self.mode {
                AppMode::TargetSelect { .. } | AppMode::CommitSelect { .. } => {
                    Some(Box::new(std::mem::replace(&mut self.mode, AppMode::Normal)))
                }
                _ => None,
            };
            self.mode = AppMode::Jump {
                labels,
                input: String::new(),
                restore_mode,
            };
        }
    }

    /// Whether a row is a diff hunk header (`@@` line).
    fn is_hunk_header(&self, row_idx: RowIdx) -> bool {
        match &self.rows[row_idx.raw()] {
            DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            } => self
                .diff_lines(*entry_idx, *file_idx)
                .and_then(|lines| lines.get(line_idx.raw()))
                .is_some_and(|dl| dl.kind == crate::dag::DiffLineKind::Header),
            DisplayRow::EvoLogFileDiffLine {
                evolog_idx,
                file_idx,
                line_idx,
            } => self
                .evolog_diff_lines(*evolog_idx, *file_idx)
                .and_then(|lines| lines.get(line_idx.raw()))
                .is_some_and(|dl| dl.kind == crate::dag::DiffLineKind::Header),
            DisplayRow::InterdiffDiffLine { file_idx, line_idx } => self
                .interdiff_diff_lines(*file_idx)
                .and_then(|lines| lines.get(line_idx.raw()))
                .is_some_and(|dl| dl.kind == crate::dag::DiffLineKind::Header),
            _ => false,
        }
    }

    pub fn take_repo_requests(&mut self) -> Vec<RepoRequest> {
        std::mem::take(&mut self.pending_repo_requests)
    }

    pub fn to_persisted_state(&self) -> PersistedState {
        let mut view_search_scopes = [0u8; <ActiveView as strum::EnumCount>::COUNT];
        for (i, vs) in self.view_states.iter().enumerate() {
            view_search_scopes[i] = vs.search_scopes.bits();
        }
        PersistedState {
            show_line_numbers: self.show_line_numbers,
            ignore_immutable: self.toggles.contains(CommandFlags::IGNORE_IMMUTABLE),
            ignore_working_copy: self.toggles.contains(CommandFlags::IGNORE_WORKING_COPY),
            debug: self.toggles.contains(CommandFlags::DEBUG),
            view_search_scopes,
            active_preset: self.revset.active_preset,
            git_diff: self.diff_format == DiffFormat::Git,
            annotate_separators: self.annotate.show_commit_separators,
            diff_underline: self.diff_underline,
        }
    }

    pub fn apply_persisted_state(&mut self, state: &PersistedState) {
        self.show_line_numbers = state.show_line_numbers;
        self.toggles
            .set(CommandFlags::IGNORE_IMMUTABLE, state.ignore_immutable);
        self.toggles
            .set(CommandFlags::IGNORE_WORKING_COPY, state.ignore_working_copy);
        self.toggles.set(CommandFlags::DEBUG, state.debug);
        self.diff_format = if state.git_diff {
            DiffFormat::Git
        } else {
            DiffFormat::ColorWords
        };
        self.annotate.show_commit_separators = state.annotate_separators;
        self.diff_underline = state.diff_underline;
        self.revset.active_preset = state
            .active_preset
            .filter(|&i| i < self.revset.presets.len());
        for (i, &bits) in state.view_search_scopes.iter().enumerate() {
            if let Some(scopes) = SearchScopes::from_bits(bits) {
                if !scopes.is_empty() {
                    self.view_states[i].search_scopes = scopes;
                }
            }
        }
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
