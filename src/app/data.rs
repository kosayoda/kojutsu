use std::collections::{HashMap, HashSet};

use super::{App, DeferredWork, Loadable};
use crate::dag::{DagEntry, DiffResult, DiffSummary, DiffTarget, EdgeKind};
use crate::idx::{EntryIdx, EvoLogIdx, FileIdx};
use crate::repo_service::{RepoError, RepoRequest, RepoResult, RevsetLoadKind};
use crate::types::ActiveView;
use crate::types::{ChangeId, CommitId, FileOwner, RepoPath, SmallVec};

/// State of an in-progress streamed revset load.
pub(super) struct DagStreamState {
    /// Graph renderer holding renderdag column state across chunks.
    renderer: crate::graph::DagGraphRenderer,
    /// The pre-refresh nodes, whose loaded data is restored as their
    /// commits reappear.
    old_nodes: HashMap<CommitId, super::DagNode>,
    /// The changes that had several commits before the reload, whose state
    /// can only be carried to a rewrite once every commit has arrived.
    divergent_before: HashSet<ChangeId>,
    /// Direct-edge child entries waiting for their parent commit to arrive
    /// in a later chunk.
    pending_parents: HashMap<CommitId, Vec<EntryIdx>>,
}

impl App {
    /// Load a revset (`None` for jj's default). `NoSnapshot` suits pure
    /// revset/UI changes (editing the revset string, switching presets) where
    /// no filesystem mutation occurred.
    pub fn request_revset_load(&mut self, revset: Option<String>, load_kind: RevsetLoadKind) {
        self.revset.load_state = Loadable::Loading;
        self.revset.pending = revset.clone().map(Into::into);
        self.clear_info_status();
        self.pending_repo_requests
            .push(RepoRequest::Revset { revset, load_kind });
    }

    /// Reload the current revset. `Snapshot` re-scans the working copy first:
    /// needed only when the user may have edited files since jj last looked.
    pub fn refresh(&mut self, load_kind: RevsetLoadKind) {
        let revset = match &self.revset.load_state {
            Loadable::Loading => self.revset.pending.as_ref().map(|s| s.to_string()),
            _ => Some(self.revset.current.to_string()),
        };
        self.request_revset_load(revset, load_kind);
    }

    /// Replace the DAG with the first chunk of a (possibly streamed) revset
    /// load. Cached data and fold state carry over via [`DagStreamState`] as
    /// their commits reappear in later chunks; the cursor via its anchor.
    fn apply_entries(&mut self, entries: Vec<DagEntry>, done: bool) {
        self.dag.loads += 1;
        self.anchor_for_reload();
        // The rows point into the nodes being replaced, so they would
        // restore the cursor onto whichever commit now has the old index.
        // The anchor restores it instead.
        if self.active_view == ActiveView::Dag {
            self.rows.clear();
        }

        let old_nodes: HashMap<CommitId, super::DagNode> = std::mem::take(&mut self.dag.nodes)
            .into_vec()
            .into_iter()
            .map(|n| (n.commit.graph_id.clone(), n))
            .collect();

        let mut seen = HashSet::new();
        let divergent_before: HashSet<ChangeId> = old_nodes
            .values()
            .map(|n| n.commit.change_id.change_id())
            .filter(|change| !seen.insert(change.clone()))
            .collect();

        self.dag.commit_index.clear();
        self.visual.mode = None;
        self.visual.persistent = None;
        self.dag.stream = Some(DagStreamState {
            renderer: crate::graph::DagGraphRenderer::new(),
            old_nodes,
            divergent_before,
            pending_parents: HashMap::new(),
        });
        self.append_entries(entries, done);
    }

    /// Append a chunk of streamed entries to the DAG, resolving cross-chunk
    /// parent/child edges and restoring surviving caches. Finalizes the
    /// stream state when `done`.
    pub(super) fn append_entries(&mut self, entries: Vec<DagEntry>, done: bool) {
        // A chunk without an active stream is stale (e.g. the stream was
        // finalized by an error), so ignore it.
        let Some(mut stream) = self.dag.stream.take() else {
            return;
        };

        let base = self.dag.nodes.len();
        for (i, entry) in entries.iter().enumerate() {
            self.dag
                .commit_index
                .insert(entry.commit.graph_id.clone(), EntryIdx::new(base + i));
        }
        let graph_lines = stream.renderer.render(&entries, &self.config.glyphs);
        self.dag.nodes.reserve(entries.len());
        for (entry, graph) in entries.into_iter().zip(graph_lines) {
            let idx = EntryIdx::new(self.dag.nodes.len());
            // Resolve direct parents against commits loaded so far; targets
            // arriving in later chunks are linked below as they appear.
            let mut parents = SmallVec::new();
            for edge in entry
                .edges
                .iter()
                .filter(|e| matches!(e.kind, EdgeKind::Direct))
            {
                if let Some(&parent_idx) = self.dag.commit_index.get(&edge.target) {
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
            if let Some(old) = stream.old_nodes.remove(&node.commit.graph_id) {
                node.restore(old);
            }
            self.dag.nodes.push(node);
        }

        // Second pass, once all of the chunk's nodes exist: fill children
        // (parents may point forward within the chunk) and link children
        // from earlier chunks that were waiting on these commits.
        for idx_raw in base..self.dag.nodes.len() {
            let idx = EntryIdx::new(idx_raw);
            for parent_idx in self.dag.nodes[idx].parents.clone() {
                self.dag.nodes[parent_idx].children.push(idx);
            }
            if let Some(waiting) = stream
                .pending_parents
                .remove(&self.dag.nodes[idx].commit.graph_id)
            {
                for child_idx in waiting {
                    self.dag.nodes[child_idx].parents.push(idx);
                    self.dag.nodes[idx].children.push(child_idx);
                }
            }
        }

        let dropped_selection =
            self.carry_commit_state(&stream.old_nodes, &stream.divergent_before, done);

        // Unfolded commits need their files again when the cached ones didn't
        // survive: a rewrite, or state just carried to the commit.
        for idx in (0..self.dag.nodes.len()).map(EntryIdx::new) {
            if self.is_commit_unfolded(idx) {
                let request = self.dag.nodes[idx].files.request_summary();
                self.pending_repo_requests.extend(request);
            }
        }

        self.anchor_list_cursor();
        self.rebuild_bookmark_entries();
        self.rebuild_tag_entries();
        self.rebuild_rows();
        self.settle_list_anchor(done);

        if done {
            self.prune_conflict_ui();
        }
        if dropped_selection {
            self.set_status(
                "a selected commit was rewritten into several copies; selection cleared",
            );
        }

        if !done {
            self.dag.stream = Some(stream);
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
                    // Before the lists the view on screen points into change.
                    self.anchor_list_cursor();
                    self.clear_info_status();
                    self.run_jobs = data.run_jobs;
                    self.revset.current = data.revset.into();
                    self.revset.draft = None;
                    self.revset.pending = None;
                    // Invalidate op log; re-request if currently viewing.
                    self.op_log.load_state = Loadable::NotRequested;
                    self.op_log.limit = super::OP_LOG_BATCH_SIZE;
                    self.op_log.details.clear();
                    self.op_log.unfolded.clear();
                    if self.active_view == ActiveView::Operations && self.op_log.load_state.begin()
                    {
                        self.pending_repo_requests.push(RepoRequest::Operations {
                            limit: self.op_log.limit,
                        });
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
                    // The status bar is one line and does not wrap, so joining
                    // every warning into it hides all but the first behind the
                    // right edge. Show the first and count the rest: each one
                    // was just logged above, so the count is a pointer, not a
                    // summary.
                    if let Some((first, rest)) = data.warnings.split_first() {
                        if rest.is_empty() {
                            self.set_error(first.clone());
                        } else {
                            self.set_error(format!(
                                "{first} (+{} more in the command log)",
                                rest.len()
                            ));
                        }
                    }
                    // apply_entries does its own rebuild_rows.
                    self.apply_entries(data.entries, data.done);
                }
                Err(error) => {
                    self.revset.pending = None;
                    self.revset.draft = Some(revset.into());
                    self.revset.load_state = Loadable::Failed(error.clone());
                    self.drop_cursor_target_for_failed_load();
                    self.set_error("failed to load revset");
                    self.show_error_overlay("revset error", error);
                }
            },
            // append_entries does its own rebuild_rows.
            RepoResult::RevsetChunk { entries, done } => {
                self.append_entries(entries, done);
            }
            RepoResult::DiffSummary { target, result } => {
                self.apply_diff_summary(&target, result, &mut deferred);
            }
            RepoResult::FileDiff {
                target,
                path,
                result,
            } => {
                self.apply_file_diff(&target, &path, result, &mut deferred);
            }
            RepoResult::WorkspaceUpdatedStale { message } => {
                self.set_status(message);
            }
            RepoResult::CommitEmpty { commit_id } => {
                if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                    self.dag.nodes[idx].commit.is_empty = true;
                }
            }
            RepoResult::DivergenceInfo { updates } => {
                for (commit_id, update) in updates {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        self.dag.nodes[idx].commit.divergence = Some(update);
                    }
                }
            }
            RepoResult::PrefixLengths { updates } => {
                for (commit_id, update) in updates {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                        let commit = &mut self.dag.nodes[idx].commit;
                        commit.change_id.set_prefix_len(update.change_prefix_len);
                        commit.commit_id.set_prefix_len(update.commit_prefix_len);
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
                        change_id.set_prefix_len(u.change_prefix_len);
                    }
                }
                self.rebuild_tag_entries();
            }
            RepoResult::Operations { limit, result } => match result {
                // Result for a superseded limit (load-more raced) - ignore.
                _ if limit != self.op_log.limit => {}
                Ok((entries, has_more)) => {
                    self.op_log.entries = super::draw_log(entries, &self.config.glyphs);
                    self.op_log.load_state = Loadable::Loaded(());
                    self.op_log.has_more = has_more;
                    if self.active_view == ActiveView::Operations {
                        deferred.rebuild.set_full();
                    }
                }
                Err(error) => {
                    self.op_log.load_state = Loadable::Failed(error.clone());
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
                    if self.active_view == ActiveView::Operations {
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
                        && let Some(file_idx) = self.dag.nodes[idx].files.file_idx(&path)
                    {
                        self.dag.nodes[idx]
                            .set_conflict_hunks(file_idx, super::Loadable::Loaded(hunks));
                        deferred.rebuild.add_entry(idx);
                        deferred.scroll = true;
                    }
                }
                Err(error) => {
                    if let Some(idx) = self.entry_by_commit_id(&commit_id)
                        && let Some(file_idx) = self.dag.nodes[idx].files.file_idx(&path)
                    {
                        self.dag.nodes[idx]
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
                    self.evolog.entries = super::draw_log(entries, &self.config.glyphs);
                    self.evolog.load_state = Loadable::Loaded(());
                    if self.active_view == ActiveView::Evolog {
                        deferred.rebuild.set_full();
                    }
                }
                Err(error) => {
                    self.evolog.clear();
                    self.evolog.load_state = Loadable::Failed(error.clone());
                    let msg = format!("failed to load evolog: {error}");
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
                    self.annotate.cache.insert(
                        crate::app::types::AnnotateTarget { commit_id, path },
                        annotate_result.clone(),
                    );
                    self.annotate.lines = Loadable::Loaded(annotate_result.lines);
                    self.annotate.commit_info = annotate_result.commit_info;
                    if self.active_view == ActiveView::Annotate {
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
            RepoResult::FileContent {
                commit_id,
                path,
                result,
            } => {
                // Only the latest request is still wanted.
                let Some(request) = self
                    .pending_file_view
                    .take_if(|r| r.commit_id == commit_id && r.path == path)
                else {
                    return deferred;
                };
                match result {
                    Ok(content) => {
                        deferred.file_view = Some(super::FileView {
                            path,
                            content,
                            line: request.line,
                        });
                    }
                    Err(error) => {
                        self.show_error_overlay(format!("read {path} at {commit_id}"), error)
                    }
                }
            }
            RepoResult::BackgroundError { error } => {
                let msg = format!("background task failed: {}", error.message);
                self.log_background_error(msg);
            }
        }
        deferred
    }

    /// Process a repo result immediately (convenience wrapper).
    /// The owner whose file tree is waiting on `target`, if any still is.
    /// Each kind of target belongs to one view, and a tree is keyed by the
    /// target it was created for, so a result for a stale one finds nothing.
    fn owner_of(&self, target: &DiffTarget) -> Option<FileOwner> {
        let owner = match target {
            DiffTarget::Commit(commit_id) => FileOwner::Dag(self.entry_by_commit_id(commit_id)?),
            DiffTarget::Evolution { commit, .. } => FileOwner::EvoLog(EvoLogIdx::new(
                self.evolog
                    .entries
                    .iter()
                    .position(|e| e.commit_id == *commit)?,
            )),
            DiffTarget::Interdiff { .. } => FileOwner::Interdiff,
        };
        (self.file_tree(owner)?.target() == target).then_some(owner)
    }

    /// Queue the rebuild of an owner's rows, if they are on screen.
    fn owner_changed(&self, owner: FileOwner, deferred: &mut DeferredWork) {
        match owner {
            FileOwner::Dag(idx) => deferred.rebuild.add_entry(idx),
            FileOwner::EvoLog(_) if self.active_view == ActiveView::Evolog => {
                deferred.rebuild.set_full();
            }
            FileOwner::Interdiff if self.active_view == ActiveView::Interdiff => {
                deferred.rebuild.set_full();
            }
            FileOwner::EvoLog(_) | FileOwner::Interdiff => {}
        }
    }

    fn apply_diff_summary(
        &mut self,
        target: &DiffTarget,
        result: Result<DiffSummary, RepoError>,
        deferred: &mut DeferredWork,
    ) {
        let Some(owner) = self.owner_of(target) else {
            return;
        };
        match &result {
            Ok(_) => {
                self.clear_info_status();
                deferred.scroll = true;
            }
            Err(error) => {
                self.show_error_overlay(format!("load files for {target}"), error.clone())
            }
        }
        let loaded = result.is_ok();
        let Some(tree) = self.file_tree_mut(owner) else {
            return;
        };
        tree.set_summary(result);
        let file_count = tree.files().map_or(0, <[_]>::len);
        if let FileOwner::Dag(idx) = owner
            && loaded
        {
            self.dag.nodes[idx].commit.is_empty = file_count == 0;
            // The file list is the first point this commit's changes
            // are known, so it's also where a line selection made
            // before a rewrite gets re-checked against them.
            self.drop_blocked_line_selection(idx);
        }
        // Files unfolded before a refresh or rewrite stay unfolded: request
        // what they show again.
        for file_idx in (0..file_count).map(FileIdx::new) {
            if self.is_file_unfolded(owner, file_idx) {
                self.request_file_contents(owner, file_idx);
            }
        }
        self.owner_changed(owner, deferred);
    }

    fn apply_file_diff(
        &mut self,
        target: &DiffTarget,
        path: &RepoPath,
        result: Result<DiffResult, RepoError>,
        deferred: &mut DeferredWork,
    ) {
        let Some(owner) = self.owner_of(target) else {
            return;
        };
        match &result {
            Ok(_) => {
                self.clear_info_status();
                deferred.scroll = true;
            }
            Err(error) => self.show_error_overlay(format!("load diff for {path}"), error.clone()),
        }
        if let Some(tree) = self.file_tree_mut(owner) {
            tree.set_diff(path, result);
        }
        self.owner_changed(owner, deferred);
    }

    pub fn handle_repo_result(&mut self, result: RepoResult) {
        let deferred = self.handle_repo_result_deferred(result);
        self.apply_deferred(deferred);
    }

    /// Finish a batch of repo results: rebuild the rows they touched, then
    /// move the cursor to its pending target now that the rows are there.
    /// Returns a file that arrived to be opened.
    pub fn apply_deferred(&mut self, deferred: DeferredWork) -> Option<super::FileView> {
        self.apply_rebuild(deferred.rebuild);
        self.settle_pending_cursor();
        if deferred.scroll {
            self.scroll_to_show_children();
        }
        deferred.file_view
    }

    /// Aggregate bookmark data from DAG nodes into a flat list for the bookmark view.
    pub fn rebuild_bookmark_entries(&mut self) {
        use super::{BookmarkKind, BookmarkViewEntry};
        use crate::types::BookmarkName;
        use std::collections::HashSet as HS;

        let mut entries: Vec<BookmarkViewEntry> = Vec::new();
        let mut seen: HS<(BookmarkName, Option<crate::types::RemoteName>)> = HS::new();

        // Local + remote bookmarks from DAG nodes in a single pass.
        for node in self.dag.nodes.iter() {
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
                        revision: Some(node.commit.unique_prefix()),
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
                        revision: Some(node.commit.unique_prefix()),
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
                    revision: None,
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
        use crate::dag::TagPresence;
        use std::collections::HashSet;

        let mut entries: Vec<TagViewEntry> = Vec::new();
        let mut seen: HashSet<crate::types::TagName> = HashSet::new();

        // Tags on visible commits (have full commit info).
        for node in self.dag.nodes.iter() {
            for tag in &node.commit.tags {
                if seen.insert(tag.clone()) {
                    let presence = self
                        .views
                        .tag_details
                        .get(tag)
                        .map_or(TagPresence::Local, |d| d.presence);
                    entries.push(TagViewEntry {
                        name: tag.clone(),
                        commit_id: Some(node.commit.graph_id.clone()),
                        change_id: Some(node.commit.change_id.clone()),
                        short_commit_id: Some(node.commit.commit_id.clone()),
                        revision: Some(node.commit.unique_prefix()),
                        description: node.commit.description.clone(),
                        presence,
                    });
                }
            }
        }

        // Tags not attached to any visible commit (from all_tags or remote-only from tag_details).
        for tag in &self.views.all_tags {
            if seen.insert(tag.clone()) {
                let details = self.views.tag_details.get(tag);
                // Use local target info from tag_details if available.
                let (commit_id, change_id, short_commit_id, revision, description) =
                    if let Some(lt) = details.and_then(|d| d.local_target.as_ref()) {
                        (
                            Some(lt.summary.commit_id.clone()),
                            Some(lt.summary.change_id.clone()),
                            Some(lt.summary.short_commit_id.clone()),
                            Some(lt.summary.revision()),
                            lt.summary.description.clone(),
                        )
                    } else {
                        (None, None, None, None, None)
                    };
                entries.push(TagViewEntry {
                    name: tag.clone(),
                    commit_id,
                    change_id,
                    short_commit_id,
                    revision,
                    description,
                    presence: TagPresence::Local,
                });
            }
        }

        // Tags only on remotes: deleted here, or never taken up.
        for (name, details) in &self.views.tag_details {
            if details.presence != TagPresence::Local && seen.insert(name.clone()) {
                entries.push(TagViewEntry {
                    name: name.clone(),
                    commit_id: None,
                    change_id: None,
                    short_commit_id: None,
                    revision: None,
                    description: None,
                    presence: details.presence,
                });
            }
        }

        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        self.views.tag_entries = entries;
    }
}

#[cfg(test)]
mod repo_result_tests {
    use super::super::{App, FileTree, Loadable, draw_log};
    use crate::dag::{DiffSummary, DiffTarget, FileChange, LineStats};
    use crate::history::EvoLogEntry;
    use crate::idx::{EvoLogIdx, FileIdx};
    use crate::repo_service::{RepoError, RepoErrorKind, RepoRequest, RepoResult};
    use crate::types::{CommitId, FileOwner, RepoPath};

    fn failure() -> RepoError {
        RepoError::new(RepoErrorKind::Operation, "boom")
    }

    /// An app whose evolog holds one step, unfolded so its file list is
    /// loading. Returns the step's diff target.
    fn app_with_evolog_step() -> (App, DiffTarget) {
        let mut app = App::for_test();
        let commit = CommitId::new("abc123");
        let predecessor = CommitId::new("def456");
        app.evolog.entries = draw_log(
            vec![EvoLogEntry::for_test(
                commit.clone(),
                vec![predecessor.clone()],
            )],
            &app.config.glyphs,
        );
        app.toggle_evolog_fold(EvoLogIdx::new(0));
        (
            app,
            DiffTarget::Evolution {
                predecessors: vec![predecessor],
                commit,
            },
        )
    }

    fn step_files(app: &App) -> &FileTree {
        app.file_tree(FileOwner::EvoLog(EvoLogIdx::new(0)))
            .expect("the step was unfolded")
    }

    /// A failed load has to land in `Failed`, not stay in `Loading`:
    /// `should_request` retries `Failed` and never retries `Loading`, so a
    /// transient error would otherwise wedge the entry forever.
    #[test]
    fn a_failed_evolog_detail_load_is_retryable() {
        let (mut app, target) = app_with_evolog_step();
        assert!(matches!(step_files(&app).summary(), Loadable::Loading));

        app.handle_repo_result(RepoResult::DiffSummary {
            target,
            result: Err(failure()),
        });

        assert!(matches!(step_files(&app).summary(), Loadable::Failed(_)));
    }

    #[test]
    fn a_failed_evolog_file_diff_is_retryable() {
        let (mut app, target) = app_with_evolog_step();
        let path = RepoPath::new("a.rs");
        app.handle_repo_result(RepoResult::DiffSummary {
            target: target.clone(),
            result: Ok(DiffSummary {
                files: vec![FileChange::for_test(path.as_str())],
                stats: LineStats::default(),
            }),
        });
        app.toggle_file_fold(FileOwner::EvoLog(EvoLogIdx::new(0)), FileIdx::new(0));
        assert!(matches!(
            step_files(&app).diff(FileIdx::new(0)),
            Some(Loadable::Loading)
        ));

        app.handle_repo_result(RepoResult::FileDiff {
            target,
            path,
            result: Err(failure()),
        });

        assert!(matches!(
            step_files(&app).diff(FileIdx::new(0)),
            Some(Loadable::Failed(_))
        ));
    }

    /// A step's diff is taken against all of its predecessors, as jj's is:
    /// a squash has several, and the step that created the change has none.
    #[test]
    fn an_evolog_step_diffs_against_all_its_predecessors() {
        for predecessors in [
            Vec::new(),
            vec![CommitId::new("def456"), CommitId::new("0ab789")],
        ] {
            let mut app = App::for_test();
            let commit = CommitId::new("abc123");
            app.evolog.entries = draw_log(
                vec![EvoLogEntry::for_test(commit.clone(), predecessors.clone())],
                &app.config.glyphs,
            );
            app.toggle_evolog_fold(EvoLogIdx::new(0));

            let expected = DiffTarget::Evolution {
                predecessors,
                commit,
            };
            assert!(matches!(
                app.take_repo_requests().as_slice(),
                [RepoRequest::DiffSummary { target }] if *target == expected
            ));
        }
    }

    /// A result for a target nobody is waiting on any more (the evolog moved
    /// to another commit meanwhile) is dropped rather than stored.
    #[test]
    fn a_stale_evolog_result_is_ignored() {
        let (mut app, _) = app_with_evolog_step();
        app.handle_repo_result(RepoResult::DiffSummary {
            target: DiffTarget::Evolution {
                predecessors: vec![CommitId::new("0ld")],
                commit: CommitId::new("abc123"),
            },
            result: Err(failure()),
        });

        assert!(matches!(step_files(&app).summary(), Loadable::Loading));
    }

    /// A file view opens with the content of the latest request only: an
    /// earlier request still in flight is superseded rather than opened
    /// over the one the user asked for since.
    #[test]
    fn a_file_view_opens_only_for_the_latest_request() {
        let mut app = App::for_test();
        let commit_id = CommitId::new("abc123");
        let (old, new) = (RepoPath::new("old.rs"), RepoPath::new("new.rs"));
        app.request_file_view(commit_id.clone(), old.clone(), 1);
        app.request_file_view(commit_id.clone(), new.clone(), 7);

        let content = |path: &RepoPath| RepoResult::FileContent {
            commit_id: commit_id.clone(),
            path: path.clone(),
            result: Ok(b"x".to_vec()),
        };
        assert!(
            app.handle_repo_result_deferred(content(&old))
                .file_view
                .is_none()
        );
        let view = app
            .handle_repo_result_deferred(content(&new))
            .file_view
            .expect("the latest request opens");
        assert_eq!((view.path, view.line), (new, 7));
    }

    /// The background pass keys its updates by whole commit ID. A workspace
    /// entry holding anything shorter matches nothing and keeps its
    /// placeholder width forever.
    #[test]
    fn a_workspace_entry_is_shortened_by_its_whole_commit_id() {
        use crate::dag::WorkspaceInfo;
        use crate::dag::{PrefixLengthUpdate, ShortId};
        use crate::types::WorkspaceName;

        let commit_id = CommitId::new("7bbaa2cb1f0e4d3a9c8b7a6e5d4c3b2a19087654");
        let mut app = App::for_test();
        app.views.workspace_entries.push(WorkspaceInfo {
            name: WorkspaceName::new("default"),
            commit_id: Some(commit_id.clone()),
            change_id: Some(ShortId::new("uunnomkxrqvlypszwlwkvvqnstvzoxrs")),
            description: None,
            is_current: true,
            path: None,
        });

        app.handle_repo_result(RepoResult::BookmarkDetailPrefixLengths {
            updates: vec![(
                commit_id,
                PrefixLengthUpdate {
                    change_prefix_len: 2,
                    commit_prefix_len: 2,
                },
            )],
        });

        let entry = &app.views.workspace_entries[0];
        assert_eq!(entry.change_id.as_ref().unwrap().prefix(), "uu");
    }

    fn loaded_with_warnings(warnings: &[&str]) -> App {
        use crate::repo_service::RevsetData;

        let mut app = App::for_test();
        app.handle_repo_result(RepoResult::Revset {
            revset: "@".into(),
            result: Ok(Box::new(RevsetData {
                revset: "@".into(),
                repo_root: String::new(),
                entries: Vec::new(),
                remote_bookmarks: Vec::new(),
                remotes: Vec::new(),
                all_tags: Vec::new(),
                tag_details: Default::default(),
                bookmark_details: Default::default(),
                workspace_entries: Vec::new(),
                warnings: warnings.iter().map(|w| (*w).to_string()).collect(),
                run_jobs: None,
                done: true,
            })),
        });
        app
    }

    fn status_of(app: &App) -> String {
        app.status_message
            .as_ref()
            .expect("a warning reaches the status bar")
            .0
            .clone()
    }

    #[test]
    fn a_lone_revset_warning_reaches_the_status_bar_whole() {
        let app = loaded_with_warnings(&["immutable() failed"]);
        assert_eq!(status_of(&app), "immutable() failed");
    }

    /// The status bar is one line and never wraps, so warnings joined into it
    /// are lost past the right edge. The count is what says to go looking.
    #[test]
    fn extra_revset_warnings_are_counted_rather_than_run_together() {
        let app = loaded_with_warnings(&["first", "second", "third"]);
        assert_eq!(status_of(&app), "first (+2 more in the command log)");
    }

    /// Every warning is logged individually, so the count the status bar
    /// quotes has somewhere to lead.
    #[test]
    fn every_revset_warning_is_logged_even_when_the_status_bar_counts_them() {
        let app = loaded_with_warnings(&["first", "second", "third"]);
        let logged = app
            .command_log
            .entries
            .iter()
            .filter(|e| matches!(e.kind, crate::app::CommandLogKind::Warning))
            .count();
        assert_eq!(logged, 3);
    }
}

#[cfg(test)]
mod view_entry_revision_tests {
    use super::super::App;
    use crate::dag::{CommitSummary, DivergenceInfo, ShortId, TagDetails, TagLocalTarget};
    use crate::types::{CommitId, TagName};

    const CHANGE_ID: &str = "uunnomkxrqvlypszwlwkvvqnstvzoxrs";
    const COMMIT_ID: &str = "7bbaa2cb1f0e4d3a9c8b7a6e5d4c3b2a19087654";

    fn app_with_tag_on(divergence: Option<DivergenceInfo>) -> App {
        let mut change_id = ShortId::new(CHANGE_ID);
        change_id.set_prefix_len(2);

        let mut app = App::for_test();
        let tag = TagName::new("v1.0");
        app.views.all_tags.push(tag.clone());
        app.views.tag_details.insert(
            tag,
            TagDetails {
                presence: crate::dag::TagPresence::Local,
                local_target: Some(TagLocalTarget {
                    summary: CommitSummary {
                        commit_id: CommitId::new(COMMIT_ID),
                        change_id,
                        short_commit_id: ShortId::new(COMMIT_ID),
                        description: None,
                        divergence,
                    },
                }),
                remote_targets: Vec::new(),
            },
        );
        app.rebuild_tag_entries();
        app
    }

    #[test]
    fn a_tag_row_carries_the_revision_for_its_commit() {
        let app = app_with_tag_on(None);
        let entry = &app.views.tag_entries[0];
        assert_eq!(entry.revision.as_ref().unwrap().as_str(), "uu");
    }

    #[test]
    fn a_tag_on_a_hidden_commit_keeps_its_offset() {
        // Without the offset this row would act on whatever superseded the
        // tagged commit, silently and with no error.
        let app = app_with_tag_on(Some(DivergenceInfo {
            is_divergent: false,
            is_hidden: true,
            suffix: Some(1),
        }));
        let entry = &app.views.tag_entries[0];
        assert_eq!(entry.revision.as_ref().unwrap().as_str(), "uu/1");
    }
}

#[cfg(test)]
mod tag_tracking_tests {
    use super::super::App;
    use crate::dag::{CommitSummary, ShortId, TagDetails, TagPresence, TagRef, TagRemoteTarget};
    use crate::idx::{TagDetailIdx, TagIdx};
    use crate::types::{ActiveView, CommitId, DisplayRow, RemoteName, TagName};

    fn remote(name: &str, commit: &str, is_tracked: bool) -> TagRemoteTarget {
        TagRemoteTarget {
            remote: RemoteName::new(name),
            summary: CommitSummary {
                commit_id: CommitId::new(commit),
                change_id: ShortId::new(""),
                short_commit_id: ShortId::new(commit),
                description: None,
                divergence: None,
            },
            is_tracked,
        }
    }

    /// `v1` locally, tracked on origin and not on upstream (both away from
    /// the local tag, so both have rows); `v2` only on origin, untracked.
    fn tag_view() -> App {
        let mut app = App::for_test();
        app.views.all_tags.push(TagName::new("v1"));
        app.views.tag_details.insert(
            TagName::new("v1"),
            TagDetails {
                presence: TagPresence::Local,
                local_target: None,
                remote_targets: vec![
                    remote("origin", "o1", true),
                    remote("upstream", "u1", false),
                ],
            },
        );
        app.views.tag_details.insert(
            TagName::new("v2"),
            TagDetails {
                presence: TagPresence::RemoteOnly,
                local_target: None,
                remote_targets: vec![remote("origin", "o2", false)],
            },
        );
        app.rebuild_tag_entries();
        app.switch_view(ActiveView::Tags);
        app
    }

    fn refs(tags: &[TagRef]) -> Vec<String> {
        tags.iter()
            .map(|t| format!("{}@{}", t.name, t.remote))
            .collect()
    }

    fn cursor_on(app: &mut App, row: DisplayRow) {
        app.cursor = app.position_of(row).expect("row is shown");
    }

    /// A tag fetched but never tracked is listed, so there's something to
    /// track it from.
    #[test]
    fn a_tag_only_on_a_remote_is_listed_as_such() {
        let app = tag_view();
        let v2 = app
            .views
            .tag_entries
            .iter()
            .find(|e| e.name.as_str() == "v2");
        assert_eq!(v2.map(|e| e.presence), Some(TagPresence::RemoteOnly));
    }

    /// On the tag's own row: its remotes not yet tracked, or those tracked.
    #[test]
    fn a_tag_row_offers_its_own_remotes() {
        let mut app = tag_view();
        cursor_on(
            &mut app,
            DisplayRow::TagItem {
                tag_idx: TagIdx::new(0),
            },
        );
        assert_eq!(refs(&app.remote_tags_to_track(true)), ["v1@upstream"]);
        assert_eq!(refs(&app.remote_tags_to_track(false)), ["v1@origin"]);
    }

    /// On a remote's row: that remote alone.
    #[test]
    fn a_remote_row_offers_only_that_remote() {
        let mut app = tag_view();
        cursor_on(
            &mut app,
            DisplayRow::TagRemoteTarget {
                tag_idx: TagIdx::new(0),
                target_idx: TagDetailIdx::new(0),
            },
        );
        assert_eq!(refs(&app.remote_tags_to_track(false)), ["v1@origin"]);
        assert!(app.remote_tags_to_track(true).is_empty());
    }

    /// Outside the tag view, every remote tag is a candidate.
    #[test]
    fn elsewhere_every_remote_tag_is_offered() {
        let mut app = tag_view();
        app.switch_view(ActiveView::Dag);
        assert_eq!(
            refs(&app.remote_tags_to_track(true)),
            ["v1@upstream", "v2@origin"]
        );
    }
}
