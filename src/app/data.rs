use std::collections::{HashMap, HashSet};

use super::{App, DeferredWork, JumpTarget, Loadable};
use crate::dag::DagEntry;
use crate::idx::{EntryIdx, FileIdx, IndexVec, RowIdx};
use crate::repo_service::{RepoRequest, RepoResult};
use crate::types::{ChangeId, CommitId, DisplayRow};

impl App {
    pub fn request_revset_load(&mut self, revset: Option<String>) {
        self.revset.load_state = Loadable::Loading;
        self.revset.pending = revset.clone().map(Into::into);
        self.clear_info_status();
        self.pending_repo_requests
            .push(RepoRequest::load_revset(revset));
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

    fn apply_entries(&mut self, entries: Vec<DagEntry>) {
        // Capture cursor context using stable ChangeId for restore after rebuild.
        let cursor_context = self.selected_entry_idx().map(|entry_idx| {
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
            (change_id, file_path, diff_line_idx)
        });

        // Build new nodes from entries, preserving cached data from old nodes.
        let entries = IndexVec::from_vec(entries);
        let new_commit_index = super::build_commit_index(&entries);

        type NodeCache = (
            Loadable<Vec<crate::dag::FileChange>>,
            Loadable<crate::dag::LineStats>,
            Vec<Loadable<crate::dag::DiffResult>>,
        );
        // Collect old caches keyed by CommitId before replacing nodes.
        let mut old_caches: HashMap<CommitId, NodeCache> = std::mem::take(&mut self.nodes)
            .into_vec()
            .into_iter()
            .map(|mut n| {
                let diffs = n.take_diffs();
                (n.commit.graph_id.clone(), (n.files, n.stats, diffs))
            })
            .collect();

        let mut nodes = super::build_nodes(entries, &new_commit_index, self.glyphs);

        // Restore cached data for commits that survived the refresh.
        for node in nodes.iter_mut() {
            if let Some((files, stats, diffs)) = old_caches.remove(&node.commit.graph_id) {
                if !files.should_request() {
                    node.files = files;
                }
                if !stats.should_request() {
                    node.stats = stats;
                }
                if !diffs.is_empty() {
                    node.restore_diffs(diffs);
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
                    self.cursor = RowIdx::new(row_idx);
                }
            }
        }

        // Apply post-refresh jump target if set.
        if let Some(target) = self.jump_after_refresh.take() {
            match target {
                JumpTarget::WorkingCopy => self.jump_to_working_copy(),
                JumpTarget::Bookmark(ref name) => self.jump_to_bookmark(name),
                JumpTarget::Prefix(ref prefix) => self.jump_to_change_id(prefix),
            }
        }

        self.refresh_search_matches();
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
                    self.apply_entries(data.entries);
                }
                Err(error) => {
                    self.revset.pending = None;
                    self.revset.draft = Some(revset.into());
                    self.revset.load_state = Loadable::Failed(error.clone());
                    self.set_error("failed to load revset");
                    self.show_error_overlay("revset error", error);
                }
            },
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
                        }
                    }

                    // Store loaded files and stats.
                    self.nodes[idx].files = Loadable::Loaded(details.files);
                    self.nodes[idx].stats = Loadable::Loaded(details.stats);
                    deferred.rebuild = true;
                    deferred.scroll = true;
                }
                Err(error) => {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        self.nodes[idx].files = Loadable::Failed(error.clone());
                        self.nodes[idx].stats = Loadable::Failed(error.clone());
                    }
                    self.show_error_overlay(format!("load files for {commit_id}"), error);
                    deferred.rebuild = true;
                }
            },
            RepoResult::FileDiff {
                commit_id,
                path,
                result,
            } => match result {
                Ok(diff_result) => {
                    self.clear_info_status();
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                            self.nodes[idx].set_diff(file_idx, diff_result);
                        }
                    }
                    deferred.rebuild = true;
                    deferred.scroll = true;
                }
                Err(error) => {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                            self.nodes[idx]
                                .set_diff_state(file_idx, Loadable::Failed(error.clone()));
                        }
                    }
                    self.show_error_overlay(format!("load diff for {path}"), error);
                    deferred.rebuild = true;
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
                    if let Some(lt) = &mut details.local_target {
                        if let Some(u) = update_map.get(&lt.summary.commit_id) {
                            u.apply(&mut lt.summary);
                        }
                    }
                    for rt in &mut details.remote_targets {
                        if let Some(u) = update_map.get(&rt.summary.commit_id) {
                            u.apply(&mut rt.summary);
                        }
                    }
                }
                for ws in &mut self.views.workspace_entries {
                    if let (Some(cid), Some(change_id)) = (&ws.commit_id, &mut ws.change_id) {
                        if let Some(u) = update_map.get(cid) {
                            change_id.display.clone_from(&u.change_display);
                            change_id.prefix_len = u.change_prefix_len;
                        }
                    }
                }
                self.rebuild_tag_entries();
            }
            RepoResult::Operations { result } => match result {
                Ok((entries, has_more)) => {
                    self.op_log.entries = entries;
                    self.op_log.loaded = true;
                    self.op_log.has_more = has_more;
                    if self.active_view == super::ActiveView::Operations {
                        deferred.rebuild = true;
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
                        deferred.rebuild = true;
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
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                            self.nodes[idx]
                                .set_conflict_hunks(file_idx, super::Loadable::Loaded(hunks));
                        }
                    }
                    deferred.rebuild = true;
                    deferred.scroll = true;
                }
                Err(error) => {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                            self.nodes[idx].set_conflict_hunks(
                                file_idx,
                                super::Loadable::Failed(error.clone()),
                            );
                        }
                    }
                    let msg = format!("failed to load conflict hunks for {path}: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::EvoLog { result } => match result {
                Ok(entries) => {
                    self.evolog.entries = entries;
                    self.evolog.loaded = true;
                    if self.active_view == super::ActiveView::Evolog {
                        deferred.rebuild = true;
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
                        deferred.rebuild = true;
                        deferred.scroll = true;
                    }
                }
                Err(error) => {
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
                        deferred.rebuild = true;
                        deferred.scroll = true;
                    }
                }
                Err(error) => {
                    let msg = format!("failed to load evolog file diff: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::InterdiffDetails { result } => match result {
                Ok(files) => {
                    self.interdiff.files = Loadable::Loaded(files);
                    if self.active_view == super::ActiveView::Interdiff {
                        deferred.rebuild = true;
                    }
                }
                Err(error) => {
                    self.interdiff.files = Loadable::Failed(error.clone());
                    let msg = format!("failed to load interdiff: {error}");
                    self.log_background_error(msg);
                }
            },
            RepoResult::InterdiffFileDiff { path, result } => match result {
                Ok(diff_result) => {
                    self.interdiff
                        .file_diffs
                        .insert(path, Loadable::Loaded(diff_result));
                    if self.active_view == super::ActiveView::Interdiff {
                        deferred.rebuild = true;
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
            RepoResult::Annotate { result } => match result {
                Ok(annotate_result) => {
                    self.annotate.lines = Loadable::Loaded(annotate_result.lines);
                    self.annotate.commit_info = annotate_result.commit_info;
                    if self.active_view == super::ActiveView::Annotate {
                        deferred.rebuild = true;
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
        if deferred.rebuild {
            self.rebuild_rows();
        }
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
                let (commit_id, change_id, description) =
                    if let Some(lt) = details.and_then(|d| d.local_target.as_ref()) {
                        (
                            Some(lt.summary.commit_id.clone()),
                            Some(lt.summary.change_id.clone()),
                            lt.summary.description.clone(),
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

        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        self.views.tag_entries = entries;
    }
}
