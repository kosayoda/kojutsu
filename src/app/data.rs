use std::collections::{HashMap, HashSet};

use super::{App, DeferredWork, JumpTarget, Loadable};
use crate::dag::{DagEntry, EdgeKind};
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx, RowIdx};
use crate::repo_service::{RepoRequest, RepoResult};
use crate::types::{ChangeId, CommitId, DisplayRow, RepoPath, SmallVec};

/// Cached per-commit data carried across a refresh.
struct DagNodeCache {
    files: Loadable<Vec<crate::dag::FileChange>>,
    stats: Loadable<crate::dag::LineStats>,
    diffs: Vec<Loadable<crate::dag::DiffResult>>,
    conflict_hunks: Vec<Loadable<Vec<crate::conflict::ConflictHunkKind>>>,
}

/// Cursor position captured before a refresh, keyed by stable IDs so it can
/// be restored once its commit reappears in the streamed DAG.
struct CursorContext {
    change_id: ChangeId,
    file_path: Option<RepoPath>,
    diff_line_idx: Option<DiffLineIdx>,
}

/// State of an in-progress streamed revset load.
pub(super) struct DagStreamState {
    /// Graph renderer holding renderdag column state across chunks.
    renderer: crate::graph::DagGraphRenderer,
    /// Caches from the pre-refresh nodes, restored as commits reappear.
    old_caches: HashMap<CommitId, DagNodeCache>,
    /// Cursor context to restore once its commit arrives.
    cursor_restore: Option<CursorContext>,
    /// Direct-edge child entries waiting for their parent commit to arrive
    /// in a later chunk.
    pending_parents: HashMap<CommitId, Vec<EntryIdx>>,
}

impl App {
    pub fn request_revset_load(&mut self, revset: Option<String>) {
        self.revset.load_state = Loadable::Loading;
        self.revset.pending = revset.clone().map(Into::into);
        self.clear_info_status();
        self.pending_repo_requests
            .push(RepoRequest::load_revset(revset));
    }

    /// Reload the current revset. `Snapshot` re-scans the working copy first —
    /// needed only when the user may have edited files since jj last looked.
    pub fn refresh(&mut self, load_kind: crate::repo_service::RevsetLoadKind) {
        let revset = match &self.revset.load_state {
            Loadable::Loading => self.revset.pending.as_ref().map(|s| s.to_string()),
            _ => Some(self.revset.current.to_string()),
        };
        match load_kind {
            crate::repo_service::RevsetLoadKind::Snapshot => self.request_revset_load(revset),
            crate::repo_service::RevsetLoadKind::NoSnapshot => {
                self.request_revset_load_no_snapshot(revset)
            }
        }
    }

    /// Request a revset load without snapshotting the working copy first.
    /// Use when the refresh is purely a revset/UI change (e.g. editing the
    /// revset string, switching presets) and no filesystem mutation occurred.
    pub fn request_revset_load_no_snapshot(&mut self, revset: Option<String>) {
        self.revset.load_state = Loadable::Loading;
        self.revset.pending = revset.clone().map(Into::into);
        self.clear_info_status();
        self.pending_repo_requests
            .push(RepoRequest::load_revset_no_snapshot(revset));
    }

    /// Replace the DAG with the first chunk of a (possibly streamed) revset
    /// load. Cached data, fold state, and the cursor position carry over via
    /// [`DagStreamState`] as their commits reappear in later chunks.
    fn apply_entries(&mut self, entries: Vec<DagEntry>, done: bool) {
        // Capture cursor context using stable ChangeId for restore after rebuild.
        let cursor_restore = self.selected_entry_idx().map(|entry_idx| {
            let change_id = self.change_id(entry_idx);
            let row = self.rows.get(self.cursor.raw());
            let file_path = row.and_then(|r| match r {
                DisplayRow::FileChange {
                    entry_idx: ei,
                    file_idx,
                }
                | DisplayRow::DiffLine {
                    entry_idx: ei,
                    file_idx,
                    ..
                } if *ei == entry_idx => self
                    .files_for_entry(entry_idx)
                    .and_then(|f| f.get(file_idx.raw()))
                    .map(|f| f.path.clone()),
                _ => None,
            });
            let diff_line_idx = row.and_then(|r| match r {
                DisplayRow::DiffLine { line_idx, .. } => Some(*line_idx),
                _ => None,
            });
            CursorContext {
                change_id,
                file_path,
                diff_line_idx,
            }
        });

        // Collect old caches keyed by CommitId before replacing nodes.
        let old_caches: HashMap<CommitId, DagNodeCache> = std::mem::take(&mut self.nodes)
            .into_vec()
            .into_iter()
            .map(|mut n| {
                let diffs = n.take_diffs();
                let conflict_hunks = n.take_conflict_hunks();
                (
                    n.commit.graph_id.clone(),
                    DagNodeCache {
                        files: n.files,
                        stats: n.stats,
                        diffs,
                        conflict_hunks,
                    },
                )
            })
            .collect();

        self.commit_index.clear();
        self.visual.mode = None;
        self.visual.persistent = None;
        self.stream = Some(DagStreamState {
            renderer: crate::graph::DagGraphRenderer::new(),
            old_caches,
            cursor_restore,
            pending_parents: HashMap::new(),
        });
        self.append_entries(entries, done);
    }

    /// Append a chunk of streamed entries to the DAG, resolving cross-chunk
    /// parent/child edges and restoring surviving caches. Finalizes the
    /// stream state when `done`.
    pub(super) fn append_entries(&mut self, entries: Vec<DagEntry>, done: bool) {
        // A chunk without an active stream is stale (e.g. the stream was
        // finalized by an error) — ignore it.
        let Some(mut stream) = self.stream.take() else {
            return;
        };

        let base = self.nodes.len();
        for (i, entry) in entries.iter().enumerate() {
            self.commit_index
                .insert(entry.commit.graph_id.clone(), EntryIdx::new(base + i));
        }
        let graph_lines = stream.renderer.render(&entries, &self.config.glyphs);
        self.nodes.reserve(entries.len());
        for (entry, graph) in entries.into_iter().zip(graph_lines) {
            let idx = EntryIdx::new(self.nodes.len());
            // Resolve direct parents against commits loaded so far; targets
            // arriving in later chunks are linked below as they appear.
            let mut parents = SmallVec::new();
            for edge in entry
                .edges
                .iter()
                .filter(|e| matches!(e.kind, EdgeKind::Direct))
            {
                if let Some(&parent_idx) = self.commit_index.get(&edge.target) {
                    parents.push(parent_idx);
                } else {
                    stream
                        .pending_parents
                        .entry(edge.target.clone())
                        .or_default()
                        .push(idx);
                }
            }
            let mut node = super::DagNode::new(entry.commit, graph, parents);
            // Restore cached data if this commit survived the refresh.
            // Same commit ID means identical content, so conflict hunks
            // (including any picks) are still valid.
            if let Some(cache) = stream.old_caches.remove(&node.commit.graph_id) {
                if !cache.files.should_request() {
                    node.files = cache.files;
                }
                if !cache.stats.should_request() {
                    node.stats = cache.stats;
                }
                if !cache.diffs.is_empty() {
                    node.restore_diffs(cache.diffs);
                }
                if !cache.conflict_hunks.is_empty() {
                    node.restore_conflict_hunks(cache.conflict_hunks);
                }
            }
            self.nodes.push(node);
        }

        // Second pass, once all of the chunk's nodes exist: fill children
        // (parents may point forward within the chunk) and link children
        // from earlier chunks that were waiting on these commits.
        for idx_raw in base..self.nodes.len() {
            let idx = EntryIdx::new(idx_raw);
            for parent_idx in self.nodes[idx].parents.clone() {
                self.nodes[parent_idx].children.push(idx);
            }
            if let Some(waiting) = stream
                .pending_parents
                .remove(&self.nodes[idx].commit.graph_id)
            {
                for child_idx in waiting {
                    self.nodes[child_idx].parents.push(idx);
                    self.nodes[idx].children.push(child_idx);
                }
            }
        }

        // Re-request data for new commits that are still unfolded but whose
        // cached file data didn't survive the refresh (happens after mutation).
        for idx_raw in base..self.nodes.len() {
            let idx = EntryIdx::new(idx_raw);
            let change_id = self.nodes[idx].commit.unique_change_id();
            if self.unfolded_commits.contains(&change_id) && self.nodes[idx].files.should_request()
            {
                let commit_id = self.nodes[idx].commit.graph_id.clone();
                self.nodes[idx].files = Loadable::Loading;
                self.nodes[idx].stats = Loadable::Loading;
                self.pending_repo_requests
                    .push(RepoRequest::load_commit_details(commit_id));
            }
        }

        self.rebuild_bookmark_entries();
        self.rebuild_tag_entries();
        self.rebuild_rows();

        if done {
            // Prune fold state for changes no longer in the DAG.
            let live_change_ids: HashSet<ChangeId> = self
                .nodes
                .iter()
                .map(|n| n.commit.unique_change_id())
                .collect();
            self.unfolded_commits
                .retain(|k| live_change_ids.contains(k));
            self.unfolded_files
                .retain(|k| live_change_ids.contains(&k.change_id));
            self.selection
                .retain(|s| live_change_ids.contains(s.change_id()));
            self.prune_conflict_ui();
            self.clear_info_status();
        } else {
            self.set_status(format!("loading… {} commits", self.nodes.len()));
        }

        // Restore the cursor once its commit has arrived.
        if let Some(ctx) = stream.cursor_restore.take()
            && !self.try_restore_cursor(&ctx)
            && !done
        {
            stream.cursor_restore = Some(ctx);
        }

        // Apply the post-refresh jump target once it can be found; give up
        // when the stream completes.
        if let Some(target) = self.jump_after_refresh.take() {
            let found = match &target {
                JumpTarget::WorkingCopy => self.jump_to_working_copy(),
                JumpTarget::Bookmark(name) => self.jump_to_bookmark(name),
                JumpTarget::Prefix(prefix) => self.jump_to_change_id(prefix),
            };
            if !found {
                if done {
                    self.set_status("jump target not in current revset");
                } else {
                    self.jump_after_refresh = Some(target);
                }
            }
        }

        if !done {
            self.stream = Some(stream);
        }

        self.refresh_search_matches();
    }

    /// Restore the cursor to the pre-refresh position captured in `ctx`,
    /// with fallback chain: DiffLine → FileChange → CommitNode.
    /// Returns whether the commit was found.
    fn try_restore_cursor(&mut self, ctx: &CursorContext) -> bool {
        let Some((entry_idx, _)) = self
            .nodes
            .iter_enumerated()
            .find(|(_, node)| node.commit.unique_change_id() == ctx.change_id)
        else {
            return false;
        };

        let find_row = |pred: &dyn Fn(&DisplayRow) -> bool| self.rows.iter().position(pred);

        let restored = ctx
            .diff_line_idx
            .and_then(|li| {
                let fp = ctx.file_path.as_ref()?;
                find_row(&|r| match r {
                    DisplayRow::DiffLine {
                        entry_idx: ei,
                        file_idx,
                        line_idx,
                    } if *ei == entry_idx && *line_idx == li => self
                        .files_for_entry(entry_idx)
                        .and_then(|f| f.get(file_idx.raw()))
                        .is_some_and(|f| f.path == *fp),
                    _ => false,
                })
            })
            .or_else(|| {
                let fp = ctx.file_path.as_ref()?;
                find_row(&|r| match r {
                    DisplayRow::FileChange {
                        entry_idx: ei,
                        file_idx,
                    } if *ei == entry_idx => self
                        .files_for_entry(entry_idx)
                        .and_then(|f| f.get(file_idx.raw()))
                        .is_some_and(|f| f.path == *fp),
                    _ => false,
                })
            })
            .or_else(|| {
                find_row(
                    &|r| matches!(r, DisplayRow::CommitNode { entry_idx: ei } if *ei == entry_idx),
                )
            });

        if let Some(row_idx) = restored {
            self.cursor = RowIdx::new(row_idx);
        }
        true
    }

    /// Process a repo result, deferring `rebuild_rows` and `scroll_to_show_children`.
    /// The caller is responsible for applying deferred work after the batch.
    pub fn handle_repo_result_deferred(&mut self, result: RepoResult) -> DeferredWork {
        let mut deferred = DeferredWork::default();
        match result {
            RepoResult::Revset { revset, result } => match result {
                Ok(data) => {
                    self.clear_info_status();
                    self.revset.current = data.revset.into();
                    self.revset.draft = None;
                    self.revset.pending = None;
                    // Invalidate op log; re-request if currently viewing.
                    self.op_log.loaded = false;
                    self.op_log.limit = super::OP_LOG_BATCH_SIZE;
                    self.op_log.details.clear();
                    self.op_log.unfolded.clear();
                    if self.active_view == super::ActiveView::Operations {
                        self.pending_repo_requests
                            .push(RepoRequest::load_operations(self.op_log.limit));
                    }
                    self.repo_root = data.repo_root;
                    self.views.remote_bookmarks = data.remote_bookmarks;
                    self.views.remotes = data.remotes;
                    self.views.all_tags = data.all_tags;
                    self.views.tag_details = data.tag_details;
                    self.views.bookmark_details = data.bookmark_details;
                    self.views.workspace_entries = data.workspace_entries;
                    self.revset.load_state = Loadable::Loaded(());
                    for w in &data.warnings {
                        self.push_command_log(
                            super::CommandLogKind::Warning,
                            w,
                            None,
                            Vec::new(),
                            false,
                        );
                    }
                    if !data.warnings.is_empty() {
                        self.set_error(data.warnings.join("; "));
                    }
                    // apply_entries does its own rebuild_rows (needed for cursor restoration).
                    self.apply_entries(data.entries, data.done);
                }
                Err(error) => {
                    self.revset.pending = None;
                    self.revset.draft = Some(revset.into());
                    self.revset.load_state = Loadable::Failed(error.clone());
                    self.set_error("failed to load revset");
                    self.show_error_overlay("revset error", error);
                }
            },
            // append_entries does its own rebuild_rows (cursor/jump restore).
            RepoResult::RevsetChunk { entries, done } => {
                self.append_entries(entries, done);
            }
            RepoResult::CommitDetails { commit_id, result } => match result {
                Ok(details) => {
                    self.clear_info_status();
                    let Some(idx) = self.entry_by_commit_id(&commit_id) else {
                        return deferred;
                    };
                    // Update is_empty for this commit.
                    self.nodes[idx].commit.is_empty = details.is_empty;

                    // Re-request diffs for files that were previously unfolded.
                    let change_id = self.change_id(idx);
                    for (fi, file) in details.files.iter().enumerate() {
                        let file_idx = FileIdx::new(fi);
                        let fold_key = super::FileFoldKey {
                            change_id: change_id.clone(),
                            path: file.path.clone(),
                        };
                        if self.unfolded_files.contains(&fold_key) {
                            if self.nodes[idx].diff_should_request(file_idx) {
                                self.nodes[idx].set_diff_state(file_idx, Loadable::Loading);
                                self.pending_repo_requests.push(RepoRequest::load_file_diff(
                                    commit_id.clone(),
                                    file.path.clone(),
                                    file.old_path.clone(),
                                ));
                            }
                            // Conflicted files show hunks instead of the
                            // diff — re-request those too (a rewritten
                            // commit gets a fresh node with no hunk cache).
                            if file.has_conflict
                                && self.nodes[idx].conflict_hunks_should_request(file_idx)
                            {
                                self.nodes[idx].set_conflict_hunks(file_idx, Loadable::Loading);
                                self.pending_repo_requests
                                    .push(RepoRequest::load_conflict_hunks(
                                        commit_id.clone(),
                                        file.path.clone(),
                                    ));
                            }
                        }
                    }

                    // Store loaded files and stats.
                    self.nodes[idx].files = Loadable::Loaded(details.files);
                    self.nodes[idx].stats = Loadable::Loaded(details.stats);
                    deferred.rebuild.add_entry(idx);
                    deferred.scroll = true;
                }
                Err(error) => {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        self.nodes[idx].files = Loadable::Failed(error.clone());
                        self.nodes[idx].stats = Loadable::Failed(error.clone());
                        deferred.rebuild.add_entry(idx);
                    }
                    self.show_error_overlay(format!("load files for {commit_id}"), error);
                }
            },
            RepoResult::FileDiff {
                commit_id,
                path,
                result,
            } => match result {
                Ok(diff_result) => {
                    self.clear_info_status();
                    if let Some(idx) = self.entry_by_commit_id(&commit_id)
                        && let Some(file_idx) = self.file_idx_by_path(idx, &path)
                    {
                        self.nodes[idx].set_diff(file_idx, diff_result);
                        deferred.rebuild.add_entry(idx);
                        deferred.scroll = true;
                    }
                }
                Err(error) => {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id)
                        && let Some(file_idx) = self.file_idx_by_path(idx, &path)
                    {
                        self.nodes[idx].set_diff_state(file_idx, Loadable::Failed(error.clone()));
                        deferred.rebuild.add_entry(idx);
                    }
                    self.show_error_overlay(format!("load diff for {path}"), error);
                }
            },
            RepoResult::WorkspaceUpdatedStale { message } => {
                self.set_status(message);
            }
            RepoResult::CommitEmpty { commit_id } => {
                if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                    self.nodes[idx].commit.is_empty = true;
                }
            }
            RepoResult::DivergenceInfo { updates } => {
                for (commit_id, update) in updates {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        self.nodes[idx].commit.divergence = Some(update);
                    }
                }
            }
            RepoResult::PrefixLengths { updates } => {
                for (commit_id, update) in updates {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        self.nodes[idx].commit.change_id.display = update.change_display;
                        self.nodes[idx].commit.change_id.prefix_len = update.change_prefix_len;
                        self.nodes[idx].commit.commit_id.display = update.commit_display;
                        self.nodes[idx].commit.commit_id.prefix_len = update.commit_prefix_len;
                    }
                }
                // Refresh bookmark entries so ShortId prefix_len is up to date.
                self.rebuild_bookmark_entries();
            }
            RepoResult::BookmarkDetailPrefixLengths { updates } => {
                let update_map: std::collections::HashMap<_, _> = updates.into_iter().collect();
                for details in self.views.bookmark_details.values_mut() {
                    for ct in &mut details.conflict_targets {
                        if let Some(u) = update_map.get(&ct.summary.commit_id) {
                            u.apply(&mut ct.summary);
                        }
                    }
                    for rt in &mut details.remote_targets {
                        if let Some(u) = update_map.get(&rt.summary.commit_id) {
                            u.apply(&mut rt.summary);
                        }
                    }
                }
                for details in self.views.tag_details.values_mut() {
                    if let Some(lt) = &mut details.local_target
                        && let Some(u) = update_map.get(&lt.summary.commit_id)
                    {
                        u.apply(&mut lt.summary);
                    }
                    for rt in &mut details.remote_targets {
                        if let Some(u) = update_map.get(&rt.summary.commit_id) {
                            u.apply(&mut rt.summary);
                        }
                    }
                }
                for ws in &mut self.views.workspace_entries {
                    if let (Some(cid), Some(change_id)) = (&ws.commit_id, &mut ws.change_id)
                        && let Some(u) = update_map.get(cid)
                    {
                        change_id.display.clone_from(&u.change_display);
                        change_id.prefix_len = u.change_prefix_len;
                    }
                }
                self.rebuild_tag_entries();
            }
            RepoResult::Operations { limit, result } => match result {
                // Result for a superseded limit (load-more raced) - ignore.
                _ if limit != self.op_log.limit => {}
                Ok((entries, has_more)) => {
                    self.op_log.entries = entries;
                    self.op_log.loaded = true;
                    self.op_log.has_more = has_more;
                    if self.active_view == super::ActiveView::Operations {
                        deferred.rebuild.set_full();
                    }
                }
                Err(error) => {
                    self.op_log.loaded = false;
                    self.op_log.entries.clear();
                    self.op_log.details.clear();
                    self.op_log.unfolded.clear();
                    let msg = format!("failed to load operation log: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::OpDiff { op_id, result } => match result {
                Ok(lines) => {
                    self.op_log
                        .details
                        .insert(op_id, super::Loadable::Loaded(lines));
                    if self.active_view == super::ActiveView::Operations {
                        deferred.rebuild.set_full();
                        deferred.scroll = true;
                    }
                }
                Err(error) => {
                    self.op_log
                        .details
                        .insert(op_id, super::Loadable::Failed(error.clone()));
                    let msg = format!("failed to load op diff: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::ConflictHunks {
                commit_id,
                path,
                result,
            } => match result {
                Ok(hunks) => {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id)
                        && let Some(file_idx) = self.file_idx_by_path(idx, &path)
                    {
                        self.nodes[idx]
                            .set_conflict_hunks(file_idx, super::Loadable::Loaded(hunks));
                        deferred.rebuild.add_entry(idx);
                        deferred.scroll = true;
                    }
                }
                Err(error) => {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id)
                        && let Some(file_idx) = self.file_idx_by_path(idx, &path)
                    {
                        self.nodes[idx]
                            .set_conflict_hunks(file_idx, super::Loadable::Failed(error.clone()));
                        deferred.rebuild.add_entry(idx);
                    }
                    let msg = format!("failed to load conflict hunks for {path}: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::EvoLog { commit_id, result } => match result {
                // The evolog target changed while this result was in flight.
                _ if self.evolog.commit_id.as_ref() != Some(&commit_id) => {}
                Ok(entries) => {
                    self.evolog.entries = entries;
                    self.evolog.loaded = true;
                    if self.active_view == super::ActiveView::Evolog {
                        deferred.rebuild.set_full();
                    }
                }
                Err(error) => {
                    self.evolog.clear();
                    let msg = format!("failed to load evolog: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::EvoLogDetails { commit_id, result } => match result {
                Ok(files) => {
                    self.evolog
                        .files
                        .insert(commit_id, super::Loadable::Loaded(files));
                    if self.active_view == super::ActiveView::Evolog {
                        deferred.rebuild.set_full();
                        deferred.scroll = true;
                    }
                }
                Err(error) => {
                    self.evolog
                        .files
                        .insert(commit_id, super::Loadable::Failed(error.clone()));
                    let msg = format!("failed to load evolog details: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::EvoLogFileDiff {
                commit_id,
                path,
                result,
            } => match result {
                Ok(diff_result) => {
                    let key = (commit_id, path);
                    self.evolog
                        .file_diffs
                        .insert(key, super::Loadable::Loaded(diff_result));
                    if self.active_view == super::ActiveView::Evolog {
                        deferred.rebuild.set_full();
                        deferred.scroll = true;
                    }
                }
                Err(error) => {
                    self.evolog
                        .file_diffs
                        .insert((commit_id, path), super::Loadable::Failed(error.clone()));
                    let msg = format!("failed to load evolog file diff: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::InterdiffDetails {
                from_commit_id,
                to_commit_id,
                result,
            } => match result {
                // The interdiff target changed while this result was in flight.
                _ if !self.interdiff.target_is(&from_commit_id, &to_commit_id) => {}
                Ok(files) => {
                    self.interdiff.files = Loadable::Loaded(files);
                    if self.active_view == super::ActiveView::Interdiff {
                        deferred.rebuild.set_full();
                    }
                }
                Err(error) => {
                    self.interdiff.files = Loadable::Failed(error.clone());
                    let msg = format!("failed to load interdiff: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::InterdiffFileDiff {
                from_commit_id,
                to_commit_id,
                path,
                result,
            } => match result {
                // The interdiff target changed while this result was in flight.
                _ if !self.interdiff.target_is(&from_commit_id, &to_commit_id) => {}
                Ok(diff_result) => {
                    self.interdiff
                        .file_diffs
                        .insert(path, Loadable::Loaded(diff_result));
                    if self.active_view == super::ActiveView::Interdiff {
                        deferred.rebuild.set_full();
                        deferred.scroll = true;
                    }
                }
                Err(error) => {
                    self.interdiff
                        .file_diffs
                        .insert(path, Loadable::Failed(error.clone()));
                    let msg = format!("failed to load interdiff file diff: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::Annotate {
                commit_id,
                path,
                result,
            } => match result {
                // The annotate target changed while this result was in flight.
                _ if self
                    .annotate
                    .target
                    .as_ref()
                    .is_none_or(|t| t.commit_id != commit_id || t.path != path) => {}
                Ok(annotate_result) => {
                    self.annotate.lines = Loadable::Loaded(annotate_result.lines);
                    self.annotate.commit_info = annotate_result.commit_info;
                    if self.active_view == super::ActiveView::Annotate {
                        deferred.rebuild.set_full();
                    }
                }
                Err(error) => {
                    self.annotate.lines = Loadable::Failed(error.clone());
                    let msg = format!("failed to annotate file: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::FileList { commit_id, result } => match result {
                Ok(files) => {
                    let items: Vec<String> = files.iter().map(|p| p.as_str().to_string()).collect();
                    self.mode = super::AppMode::select_from_list(
                        "files",
                        items,
                        false,
                        crate::types::PendingSelection::FileListAnnotate { commit_id },
                        true,
                    );
                }
                Err(error) => {
                    self.set_error(format!("file list: {}", error.message));
                }
            },
            RepoResult::BackgroundError { error } => {
                let msg = format!("background task failed: {}", error.message);
                self.log_background_error(msg);
            }
        }
        deferred
    }

    /// Process a repo result immediately (convenience wrapper).
    pub fn handle_repo_result(&mut self, result: RepoResult) {
        let deferred = self.handle_repo_result_deferred(result);
        self.apply_rebuild(deferred.rebuild);
        if deferred.scroll {
            self.scroll_to_show_children();
        }
    }

    /// Aggregate bookmark data from DAG nodes into a flat list for the bookmark view.
    pub fn rebuild_bookmark_entries(&mut self) {
        use super::{BookmarkKind, BookmarkViewEntry};
        use crate::types::BookmarkName;
        use std::collections::HashSet as HS;

        let mut entries: Vec<BookmarkViewEntry> = Vec::new();
        let mut seen: HS<(BookmarkName, Option<crate::types::RemoteName>)> = HS::new();

        // Local + remote bookmarks from DAG nodes in a single pass.
        for node in self.nodes.iter() {
            for bm in &node.commit.bookmarks {
                if seen.insert((bm.name.clone(), None)) {
                    let kind = if bm.is_tracking {
                        BookmarkKind::Tracking {
                            is_dirty: bm.is_dirty,
                            is_conflicted: bm.is_conflicted,
                        }
                    } else {
                        BookmarkKind::Local {
                            is_dirty: bm.is_dirty,
                            is_conflicted: bm.is_conflicted,
                        }
                    };
                    entries.push(BookmarkViewEntry {
                        name: bm.name.clone(),
                        commit_id: Some(node.commit.graph_id.clone()),
                        change_id: Some(node.commit.change_id.clone()),
                        short_commit_id: Some(node.commit.commit_id.clone()),
                        description: node.commit.description.clone(),
                        kind,
                    });
                }
            }
            for rb in &node.commit.remote_bookmarks {
                if seen.insert((rb.name.clone(), Some(rb.remote.clone()))) {
                    let kind = if rb.is_tracked {
                        BookmarkKind::TrackedRemote {
                            remote: rb.remote.clone(),
                        }
                    } else {
                        BookmarkKind::UntrackedRemote {
                            remote: rb.remote.clone(),
                        }
                    };
                    entries.push(BookmarkViewEntry {
                        name: rb.name.clone(),
                        commit_id: Some(node.commit.graph_id.clone()),
                        change_id: Some(node.commit.change_id.clone()),
                        short_commit_id: Some(node.commit.commit_id.clone()),
                        description: node.commit.description.clone(),
                        kind,
                    });
                }
            }
        }

        // Remote bookmarks not attached to any visible DAG node.
        for rb in &self.views.remote_bookmarks {
            if seen.insert((rb.name.clone(), Some(rb.remote.clone()))) {
                let kind = if rb.is_tracked {
                    BookmarkKind::TrackedRemote {
                        remote: rb.remote.clone(),
                    }
                } else {
                    BookmarkKind::UntrackedRemote {
                        remote: rb.remote.clone(),
                    }
                };
                entries.push(BookmarkViewEntry {
                    name: rb.name.clone(),
                    commit_id: rb.commit_id.clone(),
                    change_id: None,
                    short_commit_id: None,
                    description: None,
                    kind,
                });
            }
        }

        // Sort: pure local → local tracking remote → tracked remote → untracked remote.
        // Within each group, alphabetical by name.
        entries
            .sort_unstable_by(|a, b| a.kind.rank().cmp(&b.kind.rank()).then(a.name.cmp(&b.name)));

        self.views.bookmark_entries = entries;
    }

    /// Aggregate tag data from DAG nodes + tag_details into a flat list for the tag view.
    pub fn rebuild_tag_entries(&mut self) {
        use super::TagViewEntry;
        use std::collections::HashSet;

        let mut entries: Vec<TagViewEntry> = Vec::new();
        let mut seen: HashSet<crate::types::TagName> = HashSet::new();

        // Tags on visible commits (have full commit info).
        for node in self.nodes.iter() {
            for tag in &node.commit.tags {
                if seen.insert(tag.clone()) {
                    let is_deleted = self
                        .views
                        .tag_details
                        .get(tag)
                        .is_some_and(|d| d.is_deleted);
                    entries.push(TagViewEntry {
                        name: tag.clone(),
                        commit_id: Some(node.commit.graph_id.clone()),
                        change_id: Some(node.commit.change_id.clone()),
                        short_commit_id: Some(node.commit.commit_id.clone()),
                        description: node.commit.description.clone(),
                        is_deleted,
                    });
                }
            }
        }

        // Tags not attached to any visible commit (from all_tags or remote-only from tag_details).
        for tag in &self.views.all_tags {
            if seen.insert(tag.clone()) {
                let details = self.views.tag_details.get(tag);
                // Use local target info from tag_details if available.
                let (commit_id, change_id, short_commit_id, description) =
                    if let Some(lt) = details.and_then(|d| d.local_target.as_ref()) {
                        (
                            Some(lt.summary.commit_id.clone()),
                            Some(lt.summary.change_id.clone()),
                            Some(lt.summary.short_commit_id.clone()),
                            lt.summary.description.clone(),
                        )
                    } else {
                        (None, None, None, None)
                    };
                entries.push(TagViewEntry {
                    name: tag.clone(),
                    commit_id,
                    change_id,
                    short_commit_id,
                    description,
                    is_deleted: false,
                });
            }
        }

        // Remote-only tags (deleted locally, not in all_tags).
        for (name, details) in &self.views.tag_details {
            if details.is_deleted && seen.insert(name.clone()) {
                entries.push(TagViewEntry {
                    name: name.clone(),
                    commit_id: None,
                    change_id: None,
                    short_commit_id: None,
                    description: None,
                    is_deleted: true,
                });
            }
        }

        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        self.views.tag_entries = entries;
    }
}

#[cfg(test)]
mod repo_result_tests {
    use super::super::{App, Loadable};
    use crate::repo_service::{RepoError, RepoErrorKind, RepoResult};
    use crate::types::{CommitId, RepoPath};

    fn failure() -> RepoError {
        RepoError::new(RepoErrorKind::Operation, "boom")
    }

    /// A failed load has to land in `Failed`, not stay in `Loading`:
    /// `should_request` retries `Failed` and never retries `Loading`, so a
    /// transient error would otherwise wedge the entry forever.
    #[test]
    fn a_failed_evolog_detail_load_is_retryable() {
        let mut app = App::for_test();
        let commit_id = CommitId::new("abc123");
        app.evolog
            .files
            .insert(commit_id.clone(), Loadable::Loading);

        app.handle_repo_result(RepoResult::EvoLogDetails {
            commit_id: commit_id.clone(),
            result: Err(failure()),
        });

        assert!(matches!(
            app.evolog.files.get(&commit_id),
            Some(Loadable::Failed(_))
        ));
    }

    #[test]
    fn a_failed_evolog_file_diff_is_retryable() {
        let mut app = App::for_test();
        let commit_id = CommitId::new("abc123");
        let path = RepoPath::new("a.rs");
        let key = (commit_id.clone(), path.clone());
        app.evolog.file_diffs.insert(key.clone(), Loadable::Loading);

        app.handle_repo_result(RepoResult::EvoLogFileDiff {
            commit_id,
            path,
            result: Err(failure()),
        });

        assert!(matches!(
            app.evolog.file_diffs.get(&key),
            Some(Loadable::Failed(_))
        ));
    }
}
