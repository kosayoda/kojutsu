use std::collections::{HashMap, HashSet};

use super::{App, AppMode, DeferredWork, JumpTarget, Loadable};
use crate::dag::DagEntry;
use crate::idx::{EntryIdx, IndexVec};
use crate::repo_service::{RepoRequest, RepoResult};
use crate::types::{ChangeId, CommitId, DisplayRow};

impl App {
    pub fn request_revset_load(&mut self, revset: Option<String>) {
        self.revset.load_state = Loadable::Loading;
        self.revset.pending = revset.clone();
        self.status_message = None;
        self.pending_repo_requests
            .push(RepoRequest::load_revset(revset));
    }

    fn apply_entries(&mut self, entries: Vec<DagEntry>) {
        // Capture cursor context using stable ChangeId for restore after rebuild.
        let cursor_context = self.selected_entry_idx().map(|entry_idx| {
            let change_id = self.change_id(entry_idx);
            let row = self.rows.get(self.cursor);
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
            (change_id, file_path, diff_line_idx)
        });

        // Build new nodes from entries, preserving cached data from old nodes.
        let entries = IndexVec::from_vec(entries);
        let new_commit_index = super::build_commit_index(&entries);

        type NodeCache = (
            Loadable<Vec<crate::dag::FileChange>>,
            Loadable<crate::dag::LineStats>,
            Vec<Loadable<Vec<crate::dag::DiffLine>>>,
            Vec<Loadable<Vec<crate::dag::DiffLine>>>,
        );
        // Collect old caches keyed by CommitId before replacing nodes.
        let old_caches: HashMap<CommitId, NodeCache> = std::mem::take(&mut self.nodes)
            .into_vec()
            .into_iter()
            .map(|n| {
                (
                    n.commit.graph_id.clone(),
                    (n.files, n.stats, n.diffs, n.diffs_cw),
                )
            })
            .collect();

        let mut nodes = super::build_nodes(entries, &new_commit_index, self.glyphs);

        // Restore cached data for commits that survived the refresh.
        for node in nodes.iter_mut() {
            if let Some((files, stats, diffs, diffs_cw)) = old_caches.get(&node.commit.graph_id) {
                if !files.should_request() {
                    node.files = files.clone();
                }
                if !stats.should_request() {
                    node.stats = stats.clone();
                }
                if !diffs.is_empty() {
                    node.diffs = diffs.clone();
                    node.diffs_cw = diffs_cw.clone();
                }
            }
        }

        self.visual.mode = None;
        self.visual.persistent = None;
        self.commit_index = new_commit_index;
        self.nodes = nodes;

        // Re-request data for commits that are still unfolded but whose
        // cached file data didn't survive the refresh (happens after mutation).
        for idx_raw in 0..self.nodes.len() {
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

        self.rebuild_bookmark_entries();
        self.rebuild_tag_entries();
        self.rebuild_rows();

        // Restore cursor using stable ChangeId, with fallback chain:
        // DiffLine → FileChange → CommitNode.
        if let Some((change_id, file_path, diff_line_idx)) = cursor_context {
            if let Some((entry_idx, _)) = self
                .nodes
                .iter_enumerated()
                .find(|(_, node)| node.commit.unique_change_id() == change_id)
            {
                let find_row = |pred: &dyn Fn(&DisplayRow) -> bool| self.rows.iter().position(pred);

                let restored = diff_line_idx
                    .and_then(|li| {
                        let fp = file_path.as_ref()?;
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
                        let fp = file_path.as_ref()?;
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
                        find_row(&|r| {
                            matches!(r, DisplayRow::CommitNode { entry_idx: ei } if *ei == entry_idx)
                        })
                    });

                if let Some(row_idx) = restored {
                    self.cursor = row_idx;
                }
            }
        }

        // Apply post-refresh jump target if set.
        if let Some(target) = self.jump_after_refresh.take() {
            match target {
                JumpTarget::WorkingCopy => self.jump_to_working_copy(),
                JumpTarget::Bookmark(ref name) => self.jump_to_bookmark(name),
                JumpTarget::ChangeId(ref prefix) => self.jump_to_change_id(prefix),
            }
        }

        self.refresh_search_matches();
    }

    /// Process a repo result, deferring `rebuild_rows` and `scroll_to_show_children`.
    /// The caller is responsible for applying deferred work after the batch.
    pub fn handle_repo_result_deferred(&mut self, result: RepoResult) -> DeferredWork {
        let mut deferred = DeferredWork::default();
        match result {
            RepoResult::RevsetLoaded(data) => {
                self.status_message = None;
                self.revset.current = data.revset;
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
                self.views.untracked_bookmarks = data.untracked_bookmarks;
                self.views.tracked_bookmarks = data.tracked_bookmarks;
                self.views.remotes = data.remotes;
                self.views.all_tags = data.all_tags;
                self.views.tag_details = data.tag_details;
                self.views.bookmark_details = data.bookmark_details;
                self.views.workspace_entries = data.workspace_entries;
                self.revset.load_state = Loadable::Loaded(());
                // apply_entries does its own rebuild_rows (needed for cursor restoration).
                self.apply_entries(data.entries);
            }
            RepoResult::RevsetFailed { revset, error } => {
                self.revset.pending = None;
                self.revset.draft = Some(revset);
                self.revset.load_state = Loadable::Failed(error.clone());
                self.set_error("failed to load revset");
                self.mode = AppMode::CommandOutput {
                    command: "revset error".to_string(),
                    output: error.into_bytes(),
                    success: false,
                };
            }
            RepoResult::CommitDetailsLoaded { commit_id, details } => {
                self.status_message = None;
                let Some(idx) = self.entry_by_commit_id(&commit_id) else {
                    return deferred;
                };
                // Update is_empty for this commit.
                self.nodes[idx].commit.is_empty = details.is_empty;

                // Re-request diffs for files that were previously unfolded.
                let change_id = self.change_id(idx);
                for (fi, file) in details.files.iter().enumerate() {
                    let fold_key = super::FileFoldKey {
                        change_id: change_id.clone(),
                        path: file.path.clone(),
                    };
                    if self.unfolded_files.contains(&fold_key) {
                        self.nodes[idx].ensure_diffs(fi + 1);
                        if self.nodes[idx].diffs[fi].should_request() {
                            self.nodes[idx].diffs[fi] = Loadable::Loading;
                            self.pending_repo_requests.push(RepoRequest::load_file_diff(
                                commit_id.clone(),
                                file.path.clone(),
                                file.old_path.clone(),
                            ));
                        }
                    }
                }

                // Store loaded files and stats.
                self.nodes[idx].files = Loadable::Loaded(details.files);
                self.nodes[idx].stats = Loadable::Loaded(details.stats);
                // Ensure diffs vec is sized to match files.
                let nfiles = self.nodes[idx].files.loaded().map_or(0, |f| f.len());
                self.nodes[idx].ensure_diffs(nfiles);
                deferred.rebuild = true;
                deferred.scroll = true;
            }
            RepoResult::CommitDetailsFailed { commit_id, error } => {
                if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                    self.nodes[idx].files = Loadable::Failed(error.clone());
                    self.nodes[idx].stats = Loadable::Failed(error.clone());
                }
                self.mode = AppMode::CommandOutput {
                    command: format!("load files for {commit_id}"),
                    output: error.into_bytes(),
                    success: false,
                };
                deferred.rebuild = true;
            }
            RepoResult::FileDiffLoaded {
                commit_id,
                path,
                result,
            } => {
                self.status_message = None;
                if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                    if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                        let fi = file_idx.raw();
                        self.nodes[idx].ensure_diffs(fi + 1);
                        self.nodes[idx].diffs[fi] = Loadable::Loaded(result.git);
                        self.nodes[idx].diffs_cw[fi] = Loadable::Loaded(result.color_words);
                    }
                }
                deferred.rebuild = true;
                deferred.scroll = true;
            }
            RepoResult::FileDiffFailed {
                commit_id,
                path,
                error,
            } => {
                if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                    if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                        let fi = file_idx.raw();
                        self.nodes[idx].ensure_diffs(fi + 1);
                        self.nodes[idx].diffs[fi] = Loadable::Failed(error.clone());
                    }
                }
                self.mode = AppMode::CommandOutput {
                    command: format!("load diff for {path}"),
                    output: error.into_bytes(),
                    success: false,
                };
                deferred.rebuild = true;
            }
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
                        self.nodes[idx].commit.is_divergent = update.is_divergent;
                        self.nodes[idx].commit.is_hidden = update.is_hidden;
                        self.nodes[idx].commit.change_id_suffix = update.change_id_suffix;
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
                        if let Some(u) = update_map.get(&ct.commit_id) {
                            u.apply(&mut ct.change_id, &mut ct.short_commit_id);
                        }
                    }
                    for rt in &mut details.remote_targets {
                        if let Some(u) = update_map.get(&rt.commit_id) {
                            u.apply(&mut rt.change_id, &mut rt.short_commit_id);
                        }
                    }
                }
                for details in self.views.tag_details.values_mut() {
                    if let Some(lt) = &mut details.local_target {
                        if let Some(u) = update_map.get(&lt.commit_id) {
                            u.apply(&mut lt.change_id, &mut lt.short_commit_id);
                        }
                    }
                    for rt in &mut details.remote_targets {
                        if let Some(u) = update_map.get(&rt.commit_id) {
                            u.apply(&mut rt.change_id, &mut rt.short_commit_id);
                        }
                    }
                }
                self.rebuild_tag_entries();
            }
            RepoResult::OperationsLoaded { entries, has_more } => {
                self.op_log.entries = entries;
                self.op_log.loaded = true;
                self.op_log.has_more = has_more;
                if self.active_view == super::ActiveView::Operations {
                    deferred.rebuild = true;
                }
            }
            RepoResult::OperationsFailed { error } => {
                self.op_log.loaded = false;
                self.set_error(format!("failed to load operation log: {error}"));
            }
            RepoResult::OpDiffLoaded { op_id, lines } => {
                self.op_log
                    .details
                    .insert(op_id, super::Loadable::Loaded(lines));
                if self.active_view == super::ActiveView::Operations {
                    deferred.rebuild = true;
                    deferred.scroll = true;
                }
            }
            RepoResult::OpDiffFailed { op_id, error } => {
                self.op_log
                    .details
                    .insert(op_id, super::Loadable::Failed(error.clone()));
                self.set_error(format!("failed to load op diff: {error}"));
            }
            RepoResult::ConflictHunksLoaded {
                commit_id,
                path,
                hunks,
            } => {
                if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                    if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                        let fi = file_idx.raw();
                        self.nodes[idx].ensure_conflict_hunks(fi + 1);
                        self.nodes[idx].conflict_hunks[fi] = super::Loadable::Loaded(hunks);
                    }
                }
                deferred.rebuild = true;
                deferred.scroll = true;
            }
            RepoResult::ConflictHunksFailed {
                commit_id,
                path,
                error,
            } => {
                if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                    if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                        let fi = file_idx.raw();
                        self.nodes[idx].ensure_conflict_hunks(fi + 1);
                        self.nodes[idx].conflict_hunks[fi] = super::Loadable::Failed(error.clone());
                    }
                }
                self.set_error(format!("failed to load conflict hunks for {path}: {error}"));
            }
            RepoResult::EvoLogLoaded { entries } => {
                self.evolog.entries = entries;
                self.evolog.loaded = true;
                if self.active_view == super::ActiveView::Evolog {
                    deferred.rebuild = true;
                }
            }
            RepoResult::EvoLogDetailsLoaded { commit_id, files } => {
                self.evolog
                    .files
                    .insert(commit_id, super::Loadable::Loaded(files));
                if self.active_view == super::ActiveView::Evolog {
                    deferred.rebuild = true;
                    deferred.scroll = true;
                }
            }
            RepoResult::EvoLogDetailsFailed { error } => {
                self.set_error(format!("failed to load evolog details: {error}"));
            }
            RepoResult::EvoLogFileDiffLoaded {
                commit_id,
                path,
                result,
            } => {
                let key = (commit_id, path);
                self.evolog
                    .file_diffs
                    .insert(key.clone(), super::Loadable::Loaded(result.git));
                self.evolog
                    .file_diffs_cw
                    .insert(key, super::Loadable::Loaded(result.color_words));
                if self.active_view == super::ActiveView::Evolog {
                    deferred.rebuild = true;
                    deferred.scroll = true;
                }
            }
            RepoResult::EvoLogFileDiffFailed { error } => {
                self.set_error(format!("failed to load evolog file diff: {error}"));
            }
            RepoResult::EvoLogFailed { error } => {
                self.evolog.loaded = false;
                self.set_error(format!("failed to load evolog: {error}"));
            }
            RepoResult::BackgroundError { error } => {
                self.set_error(format!("background task failed: {error}"));
            }
        }
        deferred
    }

    /// Process a repo result immediately (convenience wrapper).
    pub fn handle_repo_result(&mut self, result: RepoResult) {
        let deferred = self.handle_repo_result_deferred(result);
        if deferred.rebuild {
            self.rebuild_rows();
        }
        if deferred.scroll {
            self.scroll_to_show_children();
        }
    }

    /// Aggregate bookmark data from DAG nodes into a flat list for the bookmark view.
    pub fn rebuild_bookmark_entries(&mut self) {
        use super::BookmarkViewEntry;
        use crate::types::{BookmarkName, RemoteName};
        use std::collections::HashSet as HS;

        let mut entries: Vec<BookmarkViewEntry> = Vec::new();
        let mut seen: HS<BookmarkName> = HS::new();

        // Local + remote bookmarks from DAG nodes in a single pass.
        for node in self.nodes.iter() {
            for bm in &node.commit.bookmarks {
                if seen.insert(bm.name.clone()) {
                    entries.push(BookmarkViewEntry {
                        name: bm.name.clone(),
                        commit_id: Some(node.commit.graph_id.clone()),
                        change_id: Some(node.commit.change_id.clone()),
                        description: node.commit.description.clone(),
                        is_tracked: true,
                        is_tracking: bm.is_tracking,
                        is_synced: !bm.is_dirty,
                        is_dirty: bm.is_dirty,
                        remote: None,
                        is_conflicted: bm.is_conflicted,
                    });
                }
            }
            for rb in &node.commit.remote_bookmarks {
                let key = BookmarkName::new(format!("{}@{}", rb.name, rb.remote));
                if seen.insert(key) {
                    entries.push(BookmarkViewEntry {
                        name: rb.name.clone(),
                        commit_id: Some(node.commit.graph_id.clone()),
                        change_id: Some(node.commit.change_id.clone()),
                        description: node.commit.description.clone(),
                        is_tracked: rb.is_tracked,
                        is_tracking: false,
                        is_synced: rb.synced,
                        is_dirty: false,
                        remote: Some(rb.remote.clone()),
                        is_conflicted: false,
                    });
                }
            }
        }

        // Remote bookmarks not attached to any visible node (tracked + untracked).
        for (raw, is_tracked) in self
            .views
            .tracked_bookmarks
            .iter()
            .map(|r| (r, true))
            .chain(self.views.untracked_bookmarks.iter().map(|r| (r, false)))
        {
            if let Some((name, remote)) = raw.rsplit_once('@') {
                let key = BookmarkName::new(raw.as_str());
                if seen.insert(key) {
                    entries.push(BookmarkViewEntry {
                        name: BookmarkName::new(name),
                        commit_id: None,
                        change_id: None,
                        description: None,
                        is_tracked,
                        is_tracking: false,
                        is_synced: false,
                        is_dirty: false,
                        remote: Some(RemoteName::new(remote)),
                        is_conflicted: false,
                    });
                }
            }
        }

        // Sort: pure local → local tracking remote → tracked remote → untracked remote.
        // Within each group, alphabetical by name.
        entries.sort_by(|a, b| {
            let rank = |e: &BookmarkViewEntry| -> u8 {
                if e.remote.is_none() && !e.is_tracking {
                    0 // pure local (no remote tracking)
                } else if e.remote.is_none() {
                    1 // local that tracks a remote
                } else if e.is_tracked {
                    2 // tracked remote
                } else {
                    3 // untracked remote
                }
            };
            rank(a).cmp(&rank(b)).then(a.name.cmp(&b.name))
        });

        self.views.bookmark_entries = entries;
    }

    /// Aggregate tag data from DAG nodes + tag_details into a flat list for the tag view.
    pub fn rebuild_tag_entries(&mut self) {
        use super::TagViewEntry;
        use crate::types::Str;
        use std::collections::HashSet;

        let mut entries: Vec<TagViewEntry> = Vec::new();
        let mut seen: HashSet<Str> = HashSet::new();

        // Tags on visible commits (have full commit info).
        for node in self.nodes.iter() {
            for tag in &node.commit.tags {
                if seen.insert(tag.clone()) {
                    let is_deleted = self
                        .views
                        .tag_details
                        .get(tag.as_str())
                        .is_some_and(|d| d.is_deleted);
                    entries.push(TagViewEntry {
                        name: tag.clone(),
                        commit_id: Some(node.commit.graph_id.clone()),
                        change_id: Some(node.commit.change_id.clone()),
                        description: node.commit.description.clone(),
                        is_deleted,
                    });
                }
            }
        }

        // Tags not attached to any visible commit (from all_tags or remote-only from tag_details).
        for tag in &self.views.all_tags {
            if seen.insert(tag.clone()) {
                let details = self.views.tag_details.get(tag.as_str());
                // Use local target info from tag_details if available.
                let (commit_id, change_id, description) =
                    if let Some(lt) = details.and_then(|d| d.local_target.as_ref()) {
                        (
                            Some(lt.commit_id.clone()),
                            Some(lt.change_id.clone()),
                            lt.description.clone(),
                        )
                    } else {
                        (None, None, None)
                    };
                entries.push(TagViewEntry {
                    name: tag.clone(),
                    commit_id,
                    change_id,
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
                    description: None,
                    is_deleted: true,
                });
            }
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        self.views.tag_entries = entries;
    }
}
