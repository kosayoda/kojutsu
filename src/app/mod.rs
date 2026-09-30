mod carry;
mod cursor;
mod data;
mod file_tree;
mod fold;
mod navigation;
mod search;
mod selection;
#[cfg(test)]
mod test_support;
mod types;
mod visual;

pub use file_tree::FileTree;
pub use types::*;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// How much of the row list a batch of repo results invalidated.
#[derive(Default)]
pub enum RebuildScope {
    #[default]
    None,
    /// Only these DAG entries' rows changed: rebuilt in place via
    /// [`App::rebuild_entry_rows`].
    Entries(Vec<crate::idx::EntryIdx>),
    /// Rebuild the whole row list.
    Full,
}

impl RebuildScope {
    /// Mark a single DAG entry's rows as changed.
    pub fn add_entry(&mut self, entry_idx: crate::idx::EntryIdx) {
        match self {
            Self::None => *self = Self::Entries(vec![entry_idx]),
            Self::Entries(entries) => {
                if !entries.contains(&entry_idx) {
                    entries.push(entry_idx);
                }
            }
            Self::Full => {}
        }
    }

    /// Mark the whole row list as changed.
    pub fn set_full(&mut self) {
        *self = Self::Full;
    }

    fn merge(&mut self, other: Self) {
        match (&mut *self, other) {
            (_, Self::None) => {}
            (Self::Full, _) => {}
            (_, Self::Full) => *self = Self::Full,
            (Self::None, entries) => *self = entries,
            (Self::Entries(_), Self::Entries(other)) => {
                for entry_idx in other {
                    self.add_entry(entry_idx);
                }
            }
        }
    }
}

/// Signals deferred work that should run once after a batch of repo results.
#[derive(Default)]
pub struct DeferredWork {
    pub rebuild: RebuildScope,
    pub scroll: bool,
    /// A file whose content arrived and is ready to open in `$EDITOR`.
    pub file_view: Option<FileView>,
}

impl DeferredWork {
    pub fn merge(&mut self, other: Self) {
        self.rebuild.merge(other.rebuild);
        self.scroll |= other.scroll;
        if other.file_view.is_some() {
            self.file_view = other.file_view;
        }
    }
}

use crate::conflict::{ConflictPick, ConflictTermKind};
use crate::dag::{DiffFormat, DiffLine, DiffTarget, FileChange};
use crate::idx::{ConflictHunkIdx, EntryIdx, FileIdx, IndexVec, RowIdx};
use crate::types::ActiveView;
use crate::types::SmallVec;

use crate::keymap::CommandFlags;
use crate::repo_service::{RepoError, RepoRequest, RevsetLoadKind};
use crate::types::{
    CommitId, ConflictHunkRef, DisplayRow, FileOwner, JumpTarget, RepoPath, RevisionArg,
    SearchScopes, SearchState, SelectionContext,
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

    /// Move to `Loading` if a request is due (never requested, or failed),
    /// returning whether the caller should send it.
    fn begin(&mut self) -> bool {
        let due = self.should_request();
        if due {
            *self = Self::Loading;
        }
        due
    }
}

impl<T> From<Result<T, RepoError>> for Loadable<T> {
    fn from(result: Result<T, RepoError>) -> Self {
        match result {
            Ok(value) => Self::Loaded(value),
            Err(error) => Self::Failed(error),
        }
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
    /// The commit's changed files and their diffs, loaded lazily.
    pub files: FileTree,
    /// Lazily loaded conflict hunks, parallel to the files (for conflicted
    /// files).
    conflict_hunks: Vec<Loadable<Vec<crate::conflict::ConflictHunkKind>>>,
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
    /// Create a node with no lazily-loaded data yet.
    fn new(
        commit: crate::dag::CommitInfo,
        graph: crate::graph::GraphLines,
        parents: SmallVec<EntryIdx>,
    ) -> Self {
        let files = FileTree::new(DiffTarget::Commit(commit.graph_id.clone()));
        Self {
            commit,
            graph,
            parents,
            children: SmallVec::new(),
            row: 0,
            files,
            conflict_hunks: Vec::new(),
        }
    }

    /// Get loaded conflict hunks for a file.
    pub fn conflict_hunks(
        &self,
        fi: FileIdx,
    ) -> Option<&Loadable<Vec<crate::conflict::ConflictHunkKind>>> {
        self.conflict_hunks.get(fi.raw())
    }

    /// Mark a conflicted file's hunks as loading and return their request,
    /// unless the file isn't conflicted or they're loaded or on their way.
    pub fn request_conflict_hunks(&mut self, fi: FileIdx) -> Option<RepoRequest> {
        let file = self.files.file(fi).filter(|f| f.has_conflict)?;
        let request = RepoRequest::ConflictHunks {
            commit_id: self.commit.graph_id.clone(),
            path: file.path.clone(),
        };
        self.conflict_hunks_slot(fi).begin().then_some(request)
    }

    /// Set the conflict hunks state for a file.
    pub fn set_conflict_hunks(
        &mut self,
        fi: FileIdx,
        state: Loadable<Vec<crate::conflict::ConflictHunkKind>>,
    ) {
        *self.conflict_hunks_slot(fi) = state;
    }

    /// Carry a surviving commit's loaded data over from its node before a
    /// refresh. Same commit ID means identical content, so conflict hunks
    /// (including any picks) are still valid.
    fn restore(&mut self, old: DagNode) {
        if !old.files.summary().should_request() {
            self.files = old.files;
            self.conflict_hunks = old.conflict_hunks;
        }
    }

    fn conflict_hunks_slot(
        &mut self,
        fi: FileIdx,
    ) -> &mut Loadable<Vec<crate::conflict::ConflictHunkKind>> {
        let i = fi.raw();
        if self.conflict_hunks.len() <= i {
            self.conflict_hunks
                .resize_with(i + 1, || Loadable::NotRequested);
        }
        &mut self.conflict_hunks[i]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct FileFoldKey {
    pub commit_id: CommitId,
    pub path: RepoPath,
}

/// Application state. Pure data -- no I/O, no rendering.
pub struct App {
    pub active_view: ActiveView,
    /// The DAG view's commits and what is unfolded or picked among them.
    pub dag: DagState,
    /// Data for bookmark/tag/workspace views.
    pub views: ViewData,
    /// Operation log view state.
    pub op_log: OpLogState,
    /// Evolution log view state.
    pub evolog: EvoLogState,
    /// Command log view state.
    pub command_log: CommandLogState,
    /// The interdiff view's comparison, once one has been entered.
    pub interdiff: Option<Interdiff>,
    /// Annotate (blame) view state.
    pub annotate: AnnotateState,
    /// Flattened display rows (one per visual line).
    pub rows: Vec<DisplayRow>,
    /// Index into `rows` of the currently selected row.
    pub cursor: RowIdx,
    /// Saved per-view state (cursor, scroll, h_scroll, search scopes).
    view_states: [ViewState; <ActiveView as strum::EnumCount>::COUNT],
    /// Scroll offset: index into `rows` of the first visible row.
    pub scroll: usize,
    /// Header height from the last render (for mouse click translation).
    pub last_header_height: u16,
    /// Viewport height of the main list area (set during render).
    pub last_list_height: u16,
    /// Horizontal scroll offset (display columns).
    pub h_scroll: usize,
    /// Revset configuration and state.
    pub revset: RevsetConfig,
    /// The loaded configuration. Held by handle rather than borrowed so it
    /// can be swapped wholesale when the config is reloaded.
    pub config: Rc<crate::config::Config>,
    /// Previously run `jj run` commands, most recent first (persisted).
    pub run_history: Vec<String>,
    /// jj's `run.jobs` setting, as of the last repo load.
    pub run_jobs: Option<usize>,
    pub repo_root: String,
    /// Current interaction mode.
    pub mode: AppMode,
    /// Repo requests waiting to be sent to the background service.
    pending_repo_requests: Vec<RepoRequest>,
    /// The file at a revision the user asked to view, while its content
    /// loads. A newer request replaces it.
    pending_file_view: Option<FileViewRequest>,
    /// Global toggles that persist across commands.
    pub toggles: CommandFlags,
    /// Which diff format to display (git vs color-words).
    pub diff_format: DiffFormat,
    /// Transient status notice shown in the status bar.
    pub status_message: Option<(String, StatusLevel)>,
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
    pub last_repeatable: Option<(crate::keymap::AppAction, CommandFlags)>,
}

/// The DAG view's state.
#[derive(Default)]
pub struct DagState {
    pub nodes: IndexVec<EntryIdx, DagNode>,
    /// Lookup from commit graph_id → entry index (needed at event boundary).
    pub commit_index: HashMap<CommitId, EntryIdx>,
    /// In-progress revset stream state (present while chunks are arriving).
    stream: Option<data::DagStreamState>,
    /// Where the cursor goes once the DAG has loaded.
    pending_cursor: Option<cursor::PendingCursor>,
    /// How many loads have started replacing the nodes.
    loads: u64,
    /// How many cursor targets have been set, to tell them apart.
    cursor_targets_set: u64,
    /// Per-commit fold state, keyed by commit ID. A reload carries it to
    /// the commit's rewrite, if there is one.
    pub unfolded_commits: HashSet<CommitId>,
    /// Per-file fold state, keyed and carried the same way.
    pub(crate) unfolded_files: HashSet<FileFoldKey>,
    /// Per-hunk conflict UI state (picks, base-fold, gap-expansion),
    /// authoritative and persisted across reloads. Keyed by commit ID:
    /// identical ID means identical content and thus identical hunk
    /// indices; a rewritten commit gets a new ID, dropping its state.
    /// Loaded `ConflictHunkKind`s stay pure repo data.
    conflict_ui: HashMap<(CommitId, RepoPath), HashMap<ConflictHunkIdx, HunkUiState>>,
}

#[cfg(test)]
mod reload_tests {
    use super::{App, Rc, SearchScopes};
    use crate::config::{Config, DefaultSearchScopes, Preset, RevsetsConfig};

    fn with_presets(count: usize) -> Rc<Config> {
        Rc::new(Config {
            revsets: RevsetsConfig {
                presets: (0..count)
                    .map(|i| Preset {
                        name: i.to_string(),
                        revset: String::new(),
                    })
                    .collect(),
            },
            ..Config::default()
        })
    }

    /// Scopes are seeded from config once and toggled by the user after.
    /// Reloading to change a color must not undo those toggles.
    #[test]
    fn a_reload_keeps_the_scopes_the_user_toggled() {
        let mut app = App::for_test();
        *app.search_scopes_mut() = SearchScopes::AUTHOR;

        app.apply_reloaded_config(Rc::new(Config {
            default_search_scopes: DefaultSearchScopes {
                path: true,
                ..DefaultSearchScopes::default()
            },
            ..Config::default()
        }));

        assert_eq!(app.search_scopes(), SearchScopes::AUTHOR);
    }

    #[test]
    fn a_reload_drops_an_active_preset_the_new_config_no_longer_has() {
        let mut app = App::new(String::new(), String::new(), with_presets(3));
        app.revset.active_preset = Some(2);

        app.apply_reloaded_config(with_presets(1));

        assert_eq!(app.revset.active_preset, None);
    }

    #[test]
    fn a_reload_keeps_an_active_preset_that_still_exists() {
        let mut app = App::new(String::new(), String::new(), with_presets(3));
        app.revset.active_preset = Some(2);

        app.apply_reloaded_config(with_presets(3));

        assert_eq!(app.revset.active_preset, Some(2));
    }
}

/// User-facing interaction state for one conflict hunk. Defaults are the
/// untouched state (no pick, base shown, section trimmed); only deviating
/// hunks get a stored entry.
#[derive(Clone, Default, PartialEq)]
pub struct HunkUiState {
    /// The user's pick (None = unresolved).
    pub pick: Option<ConflictPick>,
    /// Base block collapsed to a one-line stub (Conflict hunks).
    pub base_folded: bool,
    /// Trimmed resolved section expanded to full content (Resolved hunks).
    pub expanded: bool,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("active_view", &self.active_view)
            .field("nodes_len", &self.dag.nodes.len())
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

impl App {
    pub fn new(revset: String, repo_root: String, config: Rc<crate::config::Config>) -> Self {
        let default_search_scopes = config.default_search_scopes.to_flags();
        let mut app = Self {
            active_view: ActiveView::Dag,
            dag: DagState::default(),
            views: ViewData::new(),
            op_log: OpLogState::new(),
            evolog: EvoLogState::new(),
            command_log: CommandLogState::new(),
            interdiff: None,
            annotate: AnnotateState::new(),
            rows: Vec::new(),
            cursor: RowIdx::new(0),
            view_states: default_view_states(default_search_scopes),
            scroll: 0,
            last_header_height: 2,
            last_list_height: 0,
            h_scroll: 0,
            revset: RevsetConfig {
                current: revset.into(),
                draft: None,
                load_state: Loadable::NotRequested,
                pending: None,
                active_preset: None,
                conflicted_prev: None,
            },
            config,
            run_history: Vec::new(),
            run_jobs: None,
            repo_root,
            mode: AppMode::Normal,
            pending_repo_requests: Vec::new(),
            pending_file_view: None,
            toggles: CommandFlags::empty(),
            diff_format: DiffFormat::ColorWords,
            status_message: None,
            pre_overlay_mode: None,
            show_line_numbers: false,
            diff_underline: true,
            selection: SelectionContext::new(),
            visual: VisualState::new(),
            search: None,
            last_repeatable: None,
        };
        app.rebuild_rows();
        app
    }

    /// An app on default config, for tests that don't exercise config at all.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::new(
            String::new(),
            String::new(),
            Rc::new(crate::config::Config::default()),
        )
    }

    /// An app showing one commit in the DAG, with the cursor on it.
    #[cfg(test)]
    pub fn with_test_commit(change_id: &str, commit_id: &str) -> Self {
        let mut app = Self::for_test();
        app.push_test_commit(crate::dag::CommitInfo::for_test(change_id, commit_id));
        app.rebuild_rows();
        app
    }

    /// Add a commit to the DAG the way a load would, indexed by its ID.
    #[cfg(test)]
    pub fn push_test_commit(&mut self, commit: crate::dag::CommitInfo) -> EntryIdx {
        let idx = EntryIdx::new(self.dag.nodes.len());
        self.dag.commit_index.insert(commit.graph_id.clone(), idx);
        self.dag.nodes.push(DagNode::new(
            commit,
            crate::graph::GraphLines::default(),
            SmallVec::new(),
        ));
        idx
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
        self.mode =
            AppMode::command_output(summary, None, error.message.into_bytes(), false, vec![]);
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
        let parsed_lines = parse_ansi_lines(&output);
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

    /// Record a `jj run` command in the persisted history (most recent first,
    /// deduplicated, capped).
    pub fn record_run_command(&mut self, command: &str) {
        const RUN_HISTORY_MAX: usize = 50;
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        self.run_history.retain(|c| c != command);
        self.run_history.insert(0, command.to_string());
        self.run_history.truncate(RUN_HISTORY_MAX);
    }

    /// Append a streamed output chunk from a running background command and
    /// incrementally ANSI-parse any newly completed lines.
    pub fn append_running_output(&mut self, chunk: &[u8]) {
        let AppMode::CommandRunning(state) = &mut self.mode else {
            return;
        };
        state.output.extend_from_slice(chunk);
        let Some(last_newline) = state.output[state.parsed_upto..]
            .iter()
            .rposition(|&b| b == b'\n')
        else {
            return;
        };
        let end = state.parsed_upto + last_newline + 1;
        state
            .parsed_lines
            .extend(parse_ansi_lines(&state.output[state.parsed_upto..end]));
        state.parsed_upto = end;
    }

    /// Whether the working copy (`@`) is visible in the current entries.
    pub fn has_working_copy(&self) -> bool {
        self.dag.nodes.iter().any(|n| n.commit.is_working_copy())
    }

    /// Number of conflicted commits in the current entries.
    pub fn conflicted_commit_count(&self) -> usize {
        self.dag
            .nodes
            .iter()
            .filter(|n| n.commit.has_conflict)
            .count()
    }

    /// Row index of a commit's `CommitNode` in the display rows.
    pub fn row_of_commit(&self, entry_idx: EntryIdx) -> Option<RowIdx> {
        Some(RowIdx::new(self.dag.nodes.get(entry_idx)?.row))
    }

    pub fn switch_view(&mut self, view: ActiveView) {
        if self.active_view == view && !view.reloads_on_reentry() {
            return;
        }
        if self.active_view == ActiveView::Dag && view != ActiveView::Dag {
            self.on_dag_hidden();
        }
        // Save current view state.
        let offset = self.scroll;
        let vs = &mut self.view_states[self.active_view.idx()];
        vs.cursor = self.cursor;
        vs.scroll_offset = offset;
        vs.h_scroll = self.h_scroll;

        self.active_view = view;
        self.on_enter(view);
        self.rebuild_rows();

        // Restore saved state for new view.
        let vs = &self.view_states[view.idx()];
        self.cursor = RowIdx::new(vs.cursor.raw().min(self.rows.len().saturating_sub(1)));
        self.scroll = vs.scroll_offset;
        self.h_scroll = vs.h_scroll;
        if view == ActiveView::Dag {
            self.on_dag_shown();
        }
    }

    /// Start whatever a view needs when it is entered. Runs before its rows
    /// are built, so the rows are still those of the view being left.
    fn on_enter(&mut self, view: ActiveView) {
        match view {
            ActiveView::Operations => {
                if self.op_log.load_state.begin() {
                    self.pending_repo_requests.push(RepoRequest::Operations {
                        limit: self.op_log.limit,
                    });
                }
            }
            ActiveView::Evolog => self.load_evolog_under_cursor(),
            ActiveView::Dag
            | ActiveView::Bookmarks
            | ActiveView::Tags
            | ActiveView::Workspaces
            | ActiveView::CommandLog
            | ActiveView::Interdiff
            | ActiveView::Annotate => {}
        }
    }

    /// Load the evolution of the change under the cursor: a DAG commit, or
    /// another version picked within the evolog itself.
    fn load_evolog_under_cursor(&mut self) {
        let commit_id = self
            .selected_entry_idx()
            .map(|idx| self.dag.nodes[idx].commit.graph_id.clone())
            .or_else(|| self.selected_evolog_entry().map(|e| e.commit_id.clone()));
        let Some(commit_id) = commit_id else {
            return;
        };
        if self.evolog.commit_id.as_ref() != Some(&commit_id) {
            self.evolog.clear();
            self.evolog.commit_id = Some(commit_id.clone());
        }
        if self.evolog.load_state.begin() {
            self.pending_repo_requests
                .push(RepoRequest::EvolutionLog { commit_id });
        }
    }

    /// Enter the interdiff view comparing two commits.
    pub fn enter_interdiff_view(
        &mut self,
        from: CommitId,
        to: CommitId,
        from_label: crate::types::Str,
        to_label: crate::types::Str,
    ) {
        let mut files = FileTree::new(DiffTarget::Interdiff { from, to });
        self.pending_repo_requests.extend(files.request_summary());
        self.interdiff = Some(Interdiff {
            from_label,
            to_label,
            files,
            unfolded_files: HashSet::new(),
        });
        self.switch_view(ActiveView::Interdiff);
    }

    /// Load a commit's file list, to pick a file to annotate from.
    pub fn request_file_list(&mut self, commit_id: CommitId) {
        self.pending_repo_requests
            .push(RepoRequest::FileList { commit_id });
    }

    /// Load a file's content at a commit, to open it read-only in `$EDITOR`
    /// at `line` once it arrives.
    pub fn request_file_view(&mut self, commit_id: CommitId, path: RepoPath, line: usize) {
        self.pending_repo_requests.push(RepoRequest::FileContent {
            commit_id: commit_id.clone(),
            path: path.clone(),
        });
        self.pending_file_view = Some(FileViewRequest {
            commit_id,
            path,
            line,
        });
    }

    /// Enter the annotate (blame) view for a file at a specific commit.
    pub fn enter_annotate_view(&mut self, commit_id: CommitId, path: crate::types::RepoPath) {
        self.annotate.clear();
        self.request_annotation(commit_id, path);
        self.switch_view(ActiveView::Annotate);
    }

    /// Point the annotate view at a target, serving it from the cache when it
    /// has been computed before and requesting it otherwise.
    fn request_annotation(&mut self, commit_id: CommitId, path: crate::types::RepoPath) {
        let target = crate::app::types::AnnotateTarget {
            commit_id: commit_id.clone(),
            path: path.clone(),
        };
        if let Some(cached) = self.annotate.cache.get(&target) {
            self.annotate.lines = Loadable::Loaded(cached.lines.clone());
            self.annotate.commit_info.extend(
                cached
                    .commit_info
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
            self.annotate.target = Some(target);
            return;
        }
        self.annotate.target = Some(target);
        self.annotate.lines = Loadable::Loading;
        self.pending_repo_requests
            .push(RepoRequest::Annotate { commit_id, path });
    }

    /// Navigate to a different commit within the annotate view (time travel).
    /// Preserves the history stack for backtracking.
    pub fn annotate_navigate(&mut self, commit_id: CommitId, target_line: usize) {
        let Some(path) = self.annotate.target.as_ref().map(|t| t.path.clone()) else {
            return;
        };
        self.annotate.clear_keep_history();
        self.annotate.target_line = Some(target_line);
        self.request_annotation(commit_id, path);
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

    /// The revisions an action acts on, in whichever view: the DAG's
    /// selected commits (or the cursor's), or the commit of the bookmark,
    /// tag, workspace, evolog step or annotated line under the cursor.
    pub fn target_revisions(&self) -> SmallVec<RevisionArg> {
        match self.active_view {
            ActiveView::Dag => self.selected_change_ids(),
            _ => self.target_revision().into_iter().collect(),
        }
    }

    /// The single revision an action acts on, as for
    /// [`target_revisions`](Self::target_revisions). `None` in the DAG while
    /// several commits are explicitly selected: a one-commit action there
    /// would silently pick one of them.
    pub fn target_revision(&self) -> Option<RevisionArg> {
        let explicit_commits = self.active_view == ActiveView::Dag
            && self.selection_active()
            && self.selection_kind() == crate::types::SelectionKind::Commit;
        (!explicit_commits)
            .then(|| self.cursor_revision())
            .flatten()
    }

    /// The revision of the commit under the cursor, in whichever view,
    /// regardless of any selection.
    pub fn cursor_revision(&self) -> Option<RevisionArg> {
        let commit_revision = |id: &CommitId| RevisionArg::new(id.as_str());
        match self.active_view {
            ActiveView::Dag => self.selected_change_id(),
            ActiveView::Bookmarks => self.selected_bookmark_entry()?.revision.clone(),
            ActiveView::Tags => self.selected_tag_entry()?.revision.clone(),
            ActiveView::Evolog => Some(commit_revision(&self.selected_evolog_entry()?.commit_id)),
            ActiveView::Workspaces => self
                .selected_workspace_entry()?
                .commit_id
                .as_ref()
                .map(commit_revision),
            ActiveView::Annotate => {
                Some(commit_revision(&self.selected_annotate_line()?.commit_id))
            }
            ActiveView::Operations | ActiveView::CommandLog | ActiveView::Interdiff => None,
        }
    }

    /// The DAG entry of the commit under the cursor, in whichever view, if
    /// the DAG shows that commit.
    pub fn target_entry_idx(&self) -> Option<EntryIdx> {
        match self.active_view {
            ActiveView::Dag => self.selected_entry_idx(),
            _ => self.entry_by_commit_id(&self.target_commit()?.0),
        }
    }

    /// The commit under the cursor outside the DAG, with its change ID for
    /// widening the revset to it if the DAG doesn't show it.
    pub fn target_commit(&self) -> Option<(CommitId, Option<crate::dag::ShortId>)> {
        match self.active_view {
            ActiveView::Bookmarks => {
                if let Some((_, target)) = self.selected_remote_target() {
                    let summary = &target.summary;
                    return Some((summary.commit_id.clone(), Some(summary.change_id.clone())));
                }
                let entry = self.selected_bookmark_entry()?;
                Some((entry.commit_id.clone()?, entry.change_id.clone()))
            }
            ActiveView::Tags => {
                let entry = self.selected_tag_entry()?;
                Some((entry.commit_id.clone()?, entry.change_id.clone()))
            }
            ActiveView::Workspaces => {
                let entry = self.selected_workspace_entry()?;
                Some((entry.commit_id.clone()?, entry.change_id.clone()))
            }
            ActiveView::Evolog => {
                let entry = self.selected_evolog_entry()?;
                Some((entry.commit_id.clone(), Some(entry.change_id.clone())))
            }
            ActiveView::Annotate => {
                let line = self.selected_annotate_line()?;
                Some((line.commit_id.clone(), Some(line.change_id.clone())))
            }
            ActiveView::Dag
            | ActiveView::Operations
            | ActiveView::CommandLog
            | ActiveView::Interdiff => None,
        }
    }

    /// The bookmarks an action acts on: the DAG commit's, or the one under
    /// the cursor in the bookmark view.
    pub fn target_bookmarks(&self) -> SmallVec<crate::types::BookmarkName> {
        match self.active_view {
            ActiveView::Dag => self
                .selected_bookmarks()
                .unwrap_or_default()
                .iter()
                .map(|b| b.name.clone())
                .collect(),
            ActiveView::Bookmarks => self
                .selected_bookmark_entry()
                .map(|e| e.name.clone())
                .into_iter()
                .collect(),
            _ => SmallVec::new(),
        }
    }

    /// The tags an action acts on: the DAG commit's, or the one under the
    /// cursor in the tag view.
    pub fn target_tags(&self) -> SmallVec<crate::types::TagName> {
        match self.active_view {
            ActiveView::Dag => self
                .selected_tags()
                .unwrap_or_default()
                .iter()
                .cloned()
                .collect(),
            ActiveView::Tags => self
                .selected_tag_entry()
                .map(|e| e.name.clone())
                .into_iter()
                .collect(),
            _ => SmallVec::new(),
        }
    }

    /// The workspaces an action acts on: the other workspaces on the DAG
    /// commit, or the one under the cursor in the workspace view.
    pub fn target_workspaces(&self) -> SmallVec<crate::types::WorkspaceName> {
        match self.active_view {
            ActiveView::Dag => self
                .selected_entry_idx()
                .map(|idx| {
                    self.dag.nodes[idx]
                        .commit
                        .workspaces
                        .iter()
                        .filter(|ws| !ws.is_current)
                        .map(|ws| ws.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            ActiveView::Workspaces => self
                .selected_workspace_entry()
                .map(|e| e.name.clone())
                .into_iter()
                .collect(),
            _ => SmallVec::new(),
        }
    }

    const EXPAND_COUNT: usize = 10;

    pub fn expand_ancestors(&mut self, entry_idx: crate::idx::EntryIdx) {
        self.expand_revset(entry_idx, "ancestors");
    }

    pub fn expand_descendants(&mut self, entry_idx: crate::idx::EntryIdx) {
        self.expand_revset(entry_idx, "descendants");
    }

    fn expand_revset(&mut self, entry_idx: crate::idx::EntryIdx, func: &str) {
        // The full revision, which the prefix pass never shortens, so the
        // next expansion finds this one in the revset to widen it.
        let change_str = self.dag.nodes[entry_idx].commit.full_revision().to_string();
        let pattern = format!("{func}({change_str}, ");

        let new_revset = if let Some(pos) = self.revset.current.find(&pattern) {
            let after_prefix = &self.revset.current[pos + pattern.len()..];
            if let Some(end) = after_prefix.find(')') {
                if let Ok(current_count) = after_prefix[..end].parse::<usize>() {
                    let new_count = current_count + Self::EXPAND_COUNT;
                    let mut revset = self.revset.current.to_string();
                    let num_start = pos + pattern.len();
                    let num_end = num_start + end;
                    revset.replace_range(num_start..num_end, &new_count.to_string());
                    revset
                } else {
                    format!(
                        "({}) | {func}({change_str}, {})",
                        self.revset.current,
                        Self::EXPAND_COUNT,
                    )
                }
            } else {
                format!(
                    "({}) | {func}({change_str}, {})",
                    self.revset.current,
                    Self::EXPAND_COUNT,
                )
            }
        } else {
            format!(
                "({}) | {func}({change_str}, {})",
                self.revset.current,
                Self::EXPAND_COUNT,
            )
        };

        self.set_jump_target(JumpTarget::Revision(RevisionArg::new(change_str)));
        // Pure revset change: no filesystem interaction, no snapshot needed.
        self.request_revset_load(Some(new_revset), RevsetLoadKind::NoSnapshot);
    }

    pub fn request_op_log_load_more(&mut self) {
        self.op_log.limit += OP_LOG_BATCH_SIZE;
        self.op_log.load_state = Loadable::Loading;
        self.pending_repo_requests.push(RepoRequest::Operations {
            limit: self.op_log.limit,
        });
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

    /// Look up a term of a conflict hunk by kind.
    /// Loaded conflict hunks for a file, or `None` if not yet loaded.
    pub fn conflict_hunks_loaded(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> Option<&[crate::conflict::ConflictHunkKind]> {
        self.dag
            .nodes
            .get(entry_idx)?
            .conflict_hunks(file_idx)?
            .loaded()
            .map(Vec::as_slice)
    }

    /// A single loaded conflict hunk by index.
    pub fn conflict_hunk(
        &self,
        hunk: ConflictHunkRef,
    ) -> Option<&crate::conflict::ConflictHunkKind> {
        self.conflict_hunks_loaded(hunk.entry_idx, hunk.file_idx)?
            .get(hunk.hunk_idx.raw())
    }

    /// Persistence key for a file's conflict UI state: content-addressed
    /// by commit ID (identical ID means identical hunks) plus path.
    fn conflict_ui_key(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> Option<(CommitId, RepoPath)> {
        let path = self.dag.nodes[entry_idx]
            .files
            .files()
            .and_then(|f| f.get(file_idx.raw()))
            .map(|f| f.path.clone())?;
        Some((self.dag.nodes[entry_idx].commit.graph_id.clone(), path))
    }

    /// The stored UI state for a hunk, if it deviates from the default.
    fn hunk_ui(&self, hunk: ConflictHunkRef) -> Option<&HunkUiState> {
        let key = self.conflict_ui_key(hunk.entry_idx, hunk.file_idx)?;
        self.dag.conflict_ui.get(&key)?.get(&hunk.hunk_idx)
    }

    /// The current pick for a hunk (None = unpicked).
    pub fn hunk_pick(&self, hunk: ConflictHunkRef) -> Option<&ConflictPick> {
        self.hunk_ui(hunk)?.pick.as_ref()
    }

    /// The term kind currently picked for a hunk, if a term (not an edit).
    pub fn hunk_picked_term(&self, hunk: ConflictHunkRef) -> Option<ConflictTermKind> {
        self.hunk_pick(hunk).and_then(ConflictPick::term)
    }

    /// Whether a conflict hunk's base block is collapsed to a stub.
    pub fn hunk_base_folded(&self, hunk: ConflictHunkRef) -> bool {
        self.hunk_ui(hunk).is_some_and(|s| s.base_folded)
    }

    /// Whether a resolved hunk's trimmed section is expanded.
    pub fn hunk_expanded(&self, hunk: ConflictHunkRef) -> bool {
        self.hunk_ui(hunk).is_some_and(|s| s.expanded)
    }

    /// Context trimming for a resolved hunk, deriving the first/last-hunk
    /// edge flags from its position in the file. The single source for those
    /// flags, so the row builder and the gap renderer can't disagree on how
    /// many lines are hidden.
    pub fn hunk_trimmed_context(
        &self,
        hunk: ConflictHunkRef,
    ) -> Option<crate::conflict::TrimmedContext> {
        let hunks = self.conflict_hunks_loaded(hunk.entry_idx, hunk.file_idx)?;
        let idx = hunk.hunk_idx.raw();
        hunks
            .get(idx)?
            .trimmed_context(self.hunk_expanded(hunk), idx == 0, idx == hunks.len() - 1)
    }

    /// Mutate a hunk's UI state, then drop the entry if it returned to the
    /// default (keeps the map sparse and prunable).
    fn update_hunk_ui(&mut self, hunk: ConflictHunkRef, f: impl FnOnce(&mut HunkUiState)) {
        let Some(key) = self.conflict_ui_key(hunk.entry_idx, hunk.file_idx) else {
            return;
        };
        {
            let file_map = self.dag.conflict_ui.entry(key.clone()).or_default();
            let state = file_map.entry(hunk.hunk_idx).or_default();
            f(state);
            if *state != HunkUiState::default() {
                return;
            }
            file_map.remove(&hunk.hunk_idx);
            if !file_map.is_empty() {
                return;
            }
        }
        self.dag.conflict_ui.remove(&key);
    }

    pub fn conflict_term(
        &self,
        hunk: ConflictHunkRef,
        kind: crate::conflict::ConflictTermKind,
    ) -> Option<&crate::conflict::ConflictTerm> {
        match self.conflict_hunk(hunk)? {
            crate::conflict::ConflictHunkKind::Conflict { terms, .. } => {
                terms.iter().find(|t| t.kind == kind)
            }
            _ => None,
        }
    }

    /// Pick a conflict term for a hunk (or unpick it, if already picked).
    /// Pure UI state: nothing is written until the picks are applied.
    /// Absent (deleted) terms cannot be picked here: content assembly
    /// could only produce an empty file, not a deletion; callers route
    /// those to `jj resolve` builtins instead. Returns the hunk's
    /// selection after the toggle (`None` = now unpicked).
    pub fn pick_conflict_term(
        &mut self,
        hunk: ConflictHunkRef,
        pick: ConflictTermKind,
    ) -> Option<ConflictPick> {
        self.set_conflict_pick(hunk, Some(ConflictPick::Term(pick)))
    }

    /// Store a hand-edited resolution for a hunk.
    pub fn set_conflict_edited(
        &mut self,
        hunk: ConflictHunkRef,
        text: crate::conflict::ConflictText,
    ) {
        self.set_conflict_pick(hunk, Some(ConflictPick::Edited(text)));
    }

    /// Clear the pick on a conflict hunk.
    pub fn unpick_conflict(&mut self, hunk: ConflictHunkRef) {
        self.set_conflict_pick(hunk, None);
    }

    fn set_conflict_pick(
        &mut self,
        hunk_ref: ConflictHunkRef,
        pick: Option<ConflictPick>,
    ) -> Option<ConflictPick> {
        // Validate the request against the (pure) hunk, producing an owned
        // pick, before touching the UI-state map.
        let new_pick = {
            let Some(crate::conflict::ConflictHunkKind::Conflict { terms }) =
                self.conflict_hunk(hunk_ref)
            else {
                return None;
            };
            match pick {
                Some(ConflictPick::Term(kind)) => {
                    if !terms.iter().any(|t| t.kind == kind && !t.absent) {
                        // Absent or unknown term: caller routes elsewhere.
                        return None;
                    }
                    // Re-picking the current term unpicks it.
                    if self.hunk_picked_term(hunk_ref) == Some(kind) {
                        None
                    } else {
                        Some(ConflictPick::Term(kind))
                    }
                }
                // An edit always replaces the current pick.
                Some(edited @ ConflictPick::Edited(_)) => Some(edited),
                None => None,
            }
        };
        self.update_hunk_ui(hunk_ref, |s| s.pick = new_pick.clone());
        self.rebuild_rows();
        new_pick
    }

    /// Drop UI state for commits no longer present (rewritten commits get
    /// new IDs, so their state can never match again).
    pub(super) fn prune_conflict_ui(&mut self) {
        let commit_index = &self.dag.commit_index;
        self.dag
            .conflict_ui
            .retain(|(commit_id, _), _| commit_index.contains_key(commit_id));
    }

    /// Owned copy of a file's per-hunk picks, keyed by hunk index.
    fn file_picks(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> HashMap<ConflictHunkIdx, ConflictPick> {
        self.conflict_ui_key(entry_idx, file_idx)
            .and_then(|key| self.dag.conflict_ui.get(&key))
            .map(|m| {
                m.iter()
                    .filter_map(|(&i, s)| s.pick.clone().map(|p| (i, p)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Assemble the file under the picks for a conflicted file. Returns the
    /// path and the assembled [`Resolution`](crate::conflict::Resolution),
    /// or `None` if hunks aren't loaded or nothing has been picked yet.
    pub fn conflict_resolution(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> Option<(RepoPath, crate::conflict::Resolution)> {
        let hunks = self.conflict_hunks_loaded(entry_idx, file_idx)?;
        let picks = self.file_picks(entry_idx, file_idx);
        if picks.is_empty() {
            return None;
        }
        let path = self.dag.nodes[entry_idx]
            .files
            .files()
            .and_then(|f| f.get(file_idx.raw()))
            .map(|f| f.path.clone())?;
        Some((path, crate::repo::assemble_resolution(hunks, &picks)))
    }

    /// Assemble a conflicted file's content for whole-file editing: picks
    /// applied where present, remaining hunks as markers. Unlike
    /// `conflict_resolution`, produced even with no picks yet. `None` if
    /// hunks aren't loaded.
    pub fn conflict_file_content(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> Option<(RepoPath, String)> {
        let hunks = self.conflict_hunks_loaded(entry_idx, file_idx)?;
        let path = self.dag.nodes[entry_idx]
            .files
            .files()
            .and_then(|f| f.get(file_idx.raw()))
            .map(|f| f.path.clone())?;
        let picks = self.file_picks(entry_idx, file_idx);
        Some((
            path,
            crate::repo::assemble_resolution(hunks, &picks).content,
        ))
    }

    pub fn selected_op_log_entry(&self) -> Option<&crate::history::OpLogEntry> {
        let op_log_idx = self.rows.get(self.cursor.raw())?.op_log_idx()?;
        self.op_log.entries.get(op_log_idx.raw()).map(|d| &d.entry)
    }

    pub fn selected_evolog_entry(&self) -> Option<&crate::history::EvoLogEntry> {
        let evolog_idx = self.rows.get(self.cursor.raw())?.evolog_idx()?;
        self.evolog.entries.get(evolog_idx.raw()).map(|d| &d.entry)
    }

    pub fn selected_workspace_entry(&self) -> Option<&crate::dag::WorkspaceInfo> {
        let workspace_idx = self.rows.get(self.cursor.raw())?.workspace_idx()?;
        self.views.workspace_entries.get(workspace_idx.raw())
    }

    /// Get the entry idx the cursor is on.
    pub fn selected_entry_idx(&self) -> Option<EntryIdx> {
        self.rows.get(self.cursor.raw())?.entry_idx()
    }

    /// The revision to hand `jj` for the commit the cursor is on.
    pub fn selected_change_id(&self) -> Option<RevisionArg> {
        let entry_idx = self.selected_entry_idx()?;
        Some(self.dag.nodes[entry_idx].commit.unique_prefix())
    }

    /// Whether the commit the cursor is on is a merge (multiple parents).
    pub fn selected_is_merge(&self) -> bool {
        self.selected_entry_idx()
            .is_some_and(|idx| self.dag.nodes[idx].commit.is_merge)
    }

    /// Get the bookmarks of the commit the cursor is on.
    pub fn selected_bookmarks(&self) -> Option<&[crate::dag::BookmarkInfo]> {
        let entry_idx = self.selected_entry_idx()?;
        Some(&self.dag.nodes[entry_idx].commit.bookmarks)
    }

    /// Get the tags of the commit the cursor is on.
    pub fn selected_tags(&self) -> Option<&[crate::types::TagName]> {
        let entry_idx = self.selected_entry_idx()?;
        Some(&self.dag.nodes[entry_idx].commit.tags)
    }

    /// Get the description of the commit the cursor is on.
    pub fn selected_description(&self) -> Option<&str> {
        let entry_idx = self.selected_entry_idx()?;
        self.dag.nodes[entry_idx].commit.description.as_deref()
    }

    /// Get the file path under the cursor (if on a file or diff line row).
    pub fn selected_file_path(&self) -> Option<&crate::types::RepoPath> {
        let (owner, file_idx) = self.rows.get(self.cursor.raw())?.file()?;
        Some(&self.file(owner, file_idx)?.path)
    }

    pub(crate) fn commit_id(&self, entry_idx: EntryIdx) -> &CommitId {
        &self.dag.nodes[entry_idx].commit.graph_id
    }

    /// The revision to hand `jj` for a commit.
    pub(crate) fn revision(&self, entry_idx: EntryIdx) -> RevisionArg {
        self.dag.nodes[entry_idx].commit.unique_prefix()
    }

    /// Resolve a revision (short prefix or whole change ID) to its commit
    /// (graph) ID via the DAG nodes.
    pub fn commit_id_for_change(&self, revision: &RevisionArg) -> Option<CommitId> {
        self.dag
            .nodes
            .iter()
            .find(|n| {
                n.commit.unique_prefix() == *revision || n.commit.full_revision() == *revision
            })
            .map(|n| n.commit.graph_id.clone())
    }

    pub fn is_commit_unfolded(&self, entry_idx: EntryIdx) -> bool {
        self.dag
            .unfolded_commits
            .contains(self.commit_id(entry_idx))
    }

    /// The file tree listing an owner's changed files.
    pub fn file_tree(&self, owner: FileOwner) -> Option<&FileTree> {
        match owner {
            FileOwner::Dag(entry_idx) => self.dag.nodes.get(entry_idx).map(|n| &n.files),
            FileOwner::EvoLog(evolog_idx) => {
                let entry = self.evolog.entries.get(evolog_idx.raw())?;
                self.evolog.files.get(&entry.commit_id)
            }
            FileOwner::Interdiff => self.interdiff.as_ref().map(|i| &i.files),
        }
    }

    fn file_tree_mut(&mut self, owner: FileOwner) -> Option<&mut FileTree> {
        match owner {
            FileOwner::Dag(entry_idx) => self.dag.nodes.get_mut(entry_idx).map(|n| &mut n.files),
            FileOwner::EvoLog(evolog_idx) => {
                let entry = self.evolog.entries.get(evolog_idx.raw())?;
                self.evolog.files.get_mut(&entry.commit_id)
            }
            FileOwner::Interdiff => self.interdiff.as_mut().map(|i| &mut i.files),
        }
    }

    pub fn file(&self, owner: FileOwner, file_idx: FileIdx) -> Option<&FileChange> {
        self.file_tree(owner)?.file(file_idx)
    }

    /// Loaded diff lines of a file, in the current diff format.
    pub fn diff_lines(&self, owner: FileOwner, file_idx: FileIdx) -> Option<&Vec<DiffLine>> {
        self.file_tree(owner)?
            .diff_lines(file_idx, self.diff_format)
    }

    /// Whether a file is unfolded to show its diff. Each view keys its fold
    /// state by what identifies the file there: the DAG by commit, carried
    /// across rewrites by reloads; the evolog by step; the interdiff by path.
    pub fn is_file_unfolded(&self, owner: FileOwner, file_idx: FileIdx) -> bool {
        let Some(path) = self.file(owner, file_idx).map(|f| &f.path) else {
            return false;
        };
        match owner {
            FileOwner::Dag(entry_idx) => self.dag.unfolded_files.contains(&FileFoldKey {
                commit_id: self.commit_id(entry_idx).clone(),
                path: path.clone(),
            }),
            FileOwner::EvoLog(evolog_idx) => {
                self.evolog.entries.get(evolog_idx.raw()).is_some_and(|e| {
                    self.evolog
                        .unfolded_files
                        .contains(&(e.commit_id.clone(), path.clone()))
                })
            }
            FileOwner::Interdiff => self
                .interdiff
                .as_ref()
                .is_some_and(|i| i.unfolded_files.contains(path)),
        }
    }

    fn set_file_unfolded(&mut self, owner: FileOwner, file_idx: FileIdx, unfolded: bool) {
        let Some(path) = self.file(owner, file_idx).map(|f| f.path.clone()) else {
            return;
        };
        match owner {
            FileOwner::Dag(entry_idx) => {
                let key = FileFoldKey {
                    commit_id: self.commit_id(entry_idx).clone(),
                    path,
                };
                set_membership(&mut self.dag.unfolded_files, key, unfolded);
            }
            FileOwner::EvoLog(evolog_idx) => {
                if let Some(entry) = self.evolog.entries.get(evolog_idx.raw()) {
                    let key = (entry.commit_id.clone(), path);
                    set_membership(&mut self.evolog.unfolded_files, key, unfolded);
                }
            }
            FileOwner::Interdiff => {
                if let Some(interdiff) = &mut self.interdiff {
                    set_membership(&mut interdiff.unfolded_files, path, unfolded);
                }
            }
        }
    }

    /// The file changes the DAG actually emits rows for under a commit:
    /// its loaded files when the commit is unfolded, else `None`. The single
    /// source for "are file rows shown": used both when building rows and
    /// when navigation decides whether a commit node is a conflict's
    /// deepest-visible representation.
    pub fn shown_files(&self, entry_idx: EntryIdx) -> Option<&[FileChange]> {
        self.is_commit_unfolded(entry_idx)
            .then(|| self.dag.nodes[entry_idx].files.files())
            .flatten()
    }

    /// The conflict hunks the DAG actually emits rows for under a file:
    /// its loaded hunks when the file is unfolded, else `None`. Companion to
    /// [`shown_files`](Self::shown_files) for the conflict level.
    pub fn shown_conflict_hunks(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> Option<&[crate::conflict::ConflictHunkKind]> {
        self.is_file_unfolded(FileOwner::Dag(entry_idx), file_idx)
            .then(|| self.conflict_hunks_loaded(entry_idx, file_idx))
            .flatten()
    }

    pub fn diff_format(&self) -> DiffFormat {
        self.diff_format
    }

    /// Enter jump mode: assign labels to visible jumpable rows.
    ///
    /// Navigation-aware keys (`j`, `k`, `J`, `K`, `0`, `$`, `@`) label the
    /// rows those keys would navigate to.  Remaining targets get single-char
    /// labels by proximity; overflow targets get two-char labels.
    /// Label every visible row for ace-style jumping. Rows a movement key
    /// already reaches keep that key as their label, read from `keymap` so a
    /// rebinding relabels the target; the rest draw from a pool of free keys.
    pub fn enter_jump(&mut self, keymap: &crate::keymap::Keymap) {
        use crate::keymap::AppAction;

        const KEYS: &[char] = &[
            'a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'q', 'w', 'e', 'r', 't', 'y', 'u', 'i',
            'o', 'p', 'z', 'x', 'c', 'v', 'b', 'n', 'm',
        ];

        let (offset, end) = self.visible_row_range();
        let cursor = self.cursor.raw();
        let visible = |idx: RowIdx| idx.raw() >= offset && idx.raw() < end;
        let dist = |i: usize| i.abs_diff(cursor);

        let nav_candidates: &[(AppAction, Option<RowIdx>)] = &[
            (AppAction::MoveDown, self.peek_down()),
            (AppAction::MoveUp, self.peek_up()),
            (AppAction::MoveDownSection, self.peek_down_section()),
            (AppAction::MoveUpSection, self.peek_up_section()),
            (AppAction::MoveToScreenTop, self.peek_screen_top()),
            (AppAction::MoveToScreenMiddle, self.peek_screen_middle()),
            (AppAction::MoveToScreenBottom, self.peek_screen_bottom()),
            (AppAction::MoveToTop, self.peek_top()),
            (AppAction::MoveToBottom, self.peek_bottom()),
            (AppAction::JumpToWorkingCopy, self.peek_working_copy()),
            (
                AppAction::NextConflict,
                self.peek_conflict(crate::types::NavDirection::Forward),
            ),
            (
                AppAction::PrevConflict,
                self.peek_conflict(crate::types::NavDirection::Backward),
            ),
        ];

        let mut labels: Vec<(String, RowIdx)> = Vec::new();
        let mut nav_rows: HashSet<RowIdx> = HashSet::new();
        let mut used_chars: HashSet<char> = HashSet::new();

        for &(action, target) in nav_candidates {
            if let Some(idx) = target
                && visible(idx)
                && idx != self.cursor
                && !nav_rows.contains(&idx)
                && let Some(key) = keymap.typeable_keys(action)
            {
                // Only the first character can collide with a pool label:
                // those are one character long, so a longer nav label like
                // `]c` still differs from `c` at the position typed first.
                used_chars.extend(key.chars().next());
                labels.push((key, idx));
                nav_rows.insert(idx);
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
                    | DisplayRow::AnnotateLine { .. }
                    | DisplayRow::ConflictHeader { .. } => Some(idx),
                    // First line of each conflict term: jump onto a side,
                    // then space picks it.
                    DisplayRow::ConflictTerm { line_idx, .. } if line_idx.raw() == 0 => Some(idx),
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
                overflow.div_ceil(pool.len())
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
            self.enter_overlay(AppMode::Jump(JumpState {
                labels,
                input: String::new(),
            }));
        }
    }

    /// Whether a row is a diff hunk header (`@@` line).
    fn is_hunk_header(&self, row_idx: RowIdx) -> bool {
        self.row_diff_line(self.rows[row_idx.raw()])
            .is_some_and(|dl| dl.kind == crate::dag::DiffLineKind::Header)
    }

    /// The diff line a `DiffLine` row shows.
    pub fn row_diff_line(&self, row: DisplayRow) -> Option<&DiffLine> {
        let DisplayRow::DiffLine {
            owner,
            file_idx,
            line_idx,
        } = row
        else {
            return None;
        };
        self.diff_lines(owner, file_idx)?.get(line_idx.raw())
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
            bookmark_separators: self.views.show_bookmark_separators,
            diff_underline: self.diff_underline,
            run_history: self.run_history.clone(),
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
        self.views.show_bookmark_separators = state.bookmark_separators;
        self.diff_underline = state.diff_underline;
        self.run_history = state.run_history.clone();
        for (i, &bits) in state.view_search_scopes.iter().enumerate() {
            if let Some(scopes) = SearchScopes::from_bits(bits)
                && !scopes.is_empty()
            {
                self.view_states[i].search_scopes = scopes;
            }
        }
    }

    /// Point the app at a freshly loaded config, recomputing what derives
    /// from it.
    ///
    /// Search scopes are deliberately not re-seeded: they start from
    /// `default_search_scopes` but the user toggles them at runtime. The
    /// active preset is re-clamped, since the new list may be shorter.
    ///
    /// Does not carry `diff.max_file_size_mib`, which lives in the repo
    /// service; send [`crate::repo_service::RepoRequest::SetDiffSizeLimit`]
    /// alongside this.
    pub fn apply_reloaded_config(&mut self, config: Rc<crate::config::Config>) {
        self.config = config;
        self.revset.active_preset = self
            .revset
            .active_preset
            .filter(|&i| i < self.config.revsets.presets.len());
    }

    /// Get the text to pre-fill the revset input with.
    /// Uses the last failed draft if one exists, otherwise the current revset.
    pub fn revset_input_text(&self) -> &str {
        self.revset.draft.as_deref().unwrap_or(&self.revset.current)
    }

    /// Look up the entry index for a commit by its graph_id.
    pub fn entry_by_commit_id(&self, commit_id: &CommitId) -> Option<EntryIdx> {
        self.dag.commit_index.get(commit_id).copied()
    }

    /// Resolve a stable conflict-hunk address back to row indices. `None`
    /// when the commit is no longer present (e.g. rewritten while the
    /// address crossed a suspend point): same commit ID means identical
    /// content, so a surviving `hunk_idx` is still valid.
    pub fn resolve_conflict_hunk(
        &self,
        commit_id: &CommitId,
        path: &RepoPath,
        hunk_idx: crate::idx::ConflictHunkIdx,
    ) -> Option<ConflictHunkRef> {
        let entry_idx = self.entry_by_commit_id(commit_id)?;
        let file_idx = self.dag.nodes[entry_idx].files.file_idx(path)?;
        Some(ConflictHunkRef {
            entry_idx,
            file_idx,
            hunk_idx,
        })
    }
}

/// Add `key` to `set` if `member`, else remove it.
fn set_membership<T: Eq + std::hash::Hash>(set: &mut HashSet<T>, key: T, member: bool) {
    if member {
        set.insert(key);
    } else {
        set.remove(&key);
    }
}

/// Flip `key`'s membership of `set`, returning whether it is now a member.
fn toggle_membership<T: Eq + std::hash::Hash>(set: &mut HashSet<T>, key: T) -> bool {
    let member = !set.contains(&key);
    set_membership(set, key, member);
    member
}

#[cfg(test)]
mod log_load_tests {
    use super::*;

    /// Entering the op log while it is still loading doesn't ask again.
    #[test]
    fn reentering_a_loading_op_log_requests_it_once() {
        let mut app = App::for_test();
        app.switch_view(ActiveView::Operations);
        app.switch_view(ActiveView::Dag);
        app.switch_view(ActiveView::Operations);
        let requests = app.take_repo_requests();
        assert_eq!(
            requests
                .iter()
                .filter(|r| matches!(r, RepoRequest::Operations { .. }))
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod annotate_request_tests {
    use super::*;
    use crate::app::types::AnnotateTarget;
    use crate::dag::AnnotateResult;
    use crate::types::RepoPath;

    fn target() -> AnnotateTarget {
        AnnotateTarget {
            commit_id: CommitId::new("7bbaa2cb"),
            path: RepoPath::new("a.rs"),
        }
    }

    fn result() -> AnnotateResult {
        AnnotateResult {
            lines: vec![crate::dag::AnnotateLineData {
                commit_id: CommitId::new("7bbaa2cb"),
                change_id: crate::dag::ShortId::new("uunnomkx"),
                author: String::new(),
                relative_time: crate::types::Str::new(""),
                line_number: 1,
                content: "fn main() {}".to_string(),
                syntax_tokens: Vec::new(),
                outside_domain: false,
            }],
            commit_info: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn a_first_visit_asks_the_repo() {
        let mut app = App::for_test();
        let t = target();
        app.enter_annotate_view(t.commit_id.clone(), t.path.clone());

        assert!(matches!(app.annotate.lines, Loadable::Loading));
        assert_eq!(app.take_repo_requests().len(), 1);
    }

    #[test]
    fn a_revisit_is_served_without_asking_the_repo() {
        // Recomputing costs most of a second on a large repo, and time travel
        // walks back and forth over the same targets.
        let mut app = App::for_test();
        let t = target();
        app.annotate.cache.insert(t.clone(), result());

        app.enter_annotate_view(t.commit_id.clone(), t.path.clone());

        assert!(app.take_repo_requests().is_empty());
        let lines = app.annotate.lines.loaded().expect("served from cache");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].content, "fn main() {}");
    }

    #[test]
    fn time_travel_back_to_a_visited_commit_is_free() {
        let mut app = App::for_test();
        let t = target();
        app.annotate.cache.insert(t.clone(), result());
        app.enter_annotate_view(t.commit_id.clone(), t.path.clone());
        let _ = app.take_repo_requests();

        // Hop away (uncached, so it asks) and back (cached, so it does not).
        app.annotate_navigate(CommitId::new("deadbeef"), 1);
        assert_eq!(app.take_repo_requests().len(), 1);

        app.annotate_navigate(t.commit_id.clone(), 1);
        assert!(app.take_repo_requests().is_empty());
        assert!(app.annotate.lines.loaded().is_some());
    }
}
