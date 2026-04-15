use std::collections::{HashMap, HashSet};

use super::{App, AppMode, JumpTarget, Loadable};
use crate::dag::DagEntry;
use crate::idx::{EntryIdx, IndexVec};
use crate::repo_service::{RepoRequest, RepoResult};
use crate::types::{ChangeId, CommitId, DisplayRow};

impl App {
    pub fn request_revset_load(&mut self, revset: Option<String>) {
        self.revset_state = Loadable::Loading;
        self.pending_revset = revset.clone();
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

        // Collect old caches keyed by CommitId before replacing nodes.
        let old_caches: HashMap<
            CommitId,
            (
                Loadable<Vec<crate::dag::FileChange>>,
                Loadable<crate::dag::LineStats>,
                Vec<Loadable<Vec<crate::dag::DiffLine>>>,
            ),
        > = std::mem::take(&mut self.nodes)
            .into_vec()
            .into_iter()
            .map(|n| {
                (
                    n.commit.graph_id.clone(),
                    (n.files, n.stats, n.diffs),
                )
            })
            .collect();

        let mut nodes = super::build_nodes(entries, &new_commit_index, self.glyphs);

        // Restore cached data for commits that survived the refresh.
        for node in nodes.iter_mut() {
            if let Some((files, stats, diffs)) = old_caches.get(&node.commit.graph_id) {
                if !files.should_request() {
                    node.files = files.clone();
                }
                if !stats.should_request() {
                    node.stats = stats.clone();
                }
                if !diffs.is_empty() {
                    node.diffs = diffs.clone();
                }
            }
        }

        self.visual = None;
        self.visual_persistent = None;
        self.commit_index = new_commit_index;
        self.nodes = nodes;

        // Re-request data for commits that are still unfolded but whose
        // new CommitId has no cached file data (happens after mutation).
        for node in self.nodes.iter() {
            let change_id = node.commit.unique_change_id();
            if self.unfolded_commits.contains(&change_id) {
                if node.files.should_request() {
                    let commit_id = node.commit.graph_id.clone();
                    self.pending_repo_requests
                        .push(RepoRequest::load_commit_details(commit_id));
                }
            }
        }
        // Mark files/stats as Loading for re-requested commits.
        for idx_raw in 0..self.nodes.len() {
            let idx = EntryIdx::new(idx_raw);
            let change_id = self.nodes[idx].commit.unique_change_id();
            if self.unfolded_commits.contains(&change_id) && self.nodes[idx].files.should_request() {
                self.nodes[idx].files = Loadable::Loading;
                self.nodes[idx].stats = Loadable::Loading;
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
            }
        }

        self.refresh_search_matches();
    }

    pub fn handle_repo_result(&mut self, result: RepoResult) {
        match result {
            RepoResult::RevsetLoaded {
                revset,
                repo_root,
                entries,
                untracked_bookmarks,
                tracked_bookmarks,
                remotes,
            } => {
                self.status_message = None;
                self.revset = revset;
                self.revset_draft = None;
                self.pending_revset = None;
                self.repo_root = repo_root;
                self.untracked_bookmarks = untracked_bookmarks;
                self.tracked_bookmarks = tracked_bookmarks;
                self.remotes = remotes;
                self.revset_state = Loadable::Loaded(());
                self.apply_entries(entries);
            }
            RepoResult::RevsetFailed { revset, error } => {
                self.pending_revset = None;
                self.revset_draft = Some(revset);
                self.revset_state = Loadable::Failed(error.clone());
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
                    return;
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
                        // Ensure diffs vec is large enough.
                        let file_idx = fi;
                        if file_idx >= self.nodes[idx].diffs.len() {
                            self.nodes[idx]
                                .diffs
                                .resize_with(file_idx + 1, || Loadable::NotRequested);
                        }
                        if self.nodes[idx].diffs[file_idx].should_request() {
                            self.nodes[idx].diffs[file_idx] = Loadable::Loading;
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
                if self.nodes[idx].diffs.len() < nfiles {
                    self.nodes[idx]
                        .diffs
                        .resize_with(nfiles, || Loadable::NotRequested);
                }
                self.rebuild_rows();
                self.scroll_to_show_children();
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
                self.rebuild_rows();
            }
            RepoResult::FileDiffLoaded {
                commit_id,
                path,
                lines,
            } => {
                self.status_message = None;
                if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                    if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                        let fi = file_idx.raw();
                        if fi >= self.nodes[idx].diffs.len() {
                            self.nodes[idx]
                                .diffs
                                .resize_with(fi + 1, || Loadable::NotRequested);
                        }
                        self.nodes[idx].diffs[fi] = Loadable::Loaded(lines);
                    }
                }
                self.rebuild_rows();
                self.scroll_to_show_children();
            }
            RepoResult::FileDiffFailed {
                commit_id,
                path,
                error,
            } => {
                if let Some(idx) = self.entry_by_commit_id(&commit_id) {
                    if let Some(file_idx) = self.file_idx_by_path(idx, &path) {
                        let fi = file_idx.raw();
                        if fi >= self.nodes[idx].diffs.len() {
                            self.nodes[idx]
                                .diffs
                                .resize_with(fi + 1, || Loadable::NotRequested);
                        }
                        self.nodes[idx].diffs[fi] = Loadable::Failed(error.clone());
                    }
                }
                self.mode = AppMode::CommandOutput {
                    command: format!("load diff for {path}"),
                    output: error.into_bytes(),
                    success: false,
                };
                self.rebuild_rows();
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
            }
            RepoResult::BackgroundError { error } => {
                self.set_error(format!("background task failed: {error}"));
            }
        }
    }
}
