use std::collections::HashSet;

use super::{App, AppMode, JumpTarget, Loadable};
use crate::dag::DagEntry;
use crate::graph;
use crate::idx::IndexVec;
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
        let entries = IndexVec::from_vec(entries);
        self.graph = IndexVec::from_vec(graph::render(entries.as_slice()));
        self.visual = None;
        self.visual_persistent = None;
        self.entries = entries;

        // Collect new CommitIds so we can prune stale caches.
        let live_commit_ids: HashSet<CommitId> = self
            .entries
            .iter()
            .map(|e| e.commit.graph_id.clone())
            .collect();

        self.file_states.retain(|k, _| live_commit_ids.contains(k));
        self.diff_states
            .retain(|k, _| live_commit_ids.contains(&k.commit_id));
        self.commit_stats_states
            .retain(|k, _| live_commit_ids.contains(k));

        // Re-request data for commits that are still unfolded but whose
        // new CommitId has no cached file data (happens after mutation).
        for entry in self.entries.iter() {
            let change_id = entry.commit.unique_change_id();
            if self.unfolded_commits.contains(&change_id) {
                let commit_id = &entry.commit.graph_id;
                if self
                    .file_states
                    .get(commit_id)
                    .is_none_or(Loadable::should_request)
                {
                    self.file_states
                        .insert(commit_id.clone(), Loadable::Loading);
                    self.commit_stats_states
                        .insert(commit_id.clone(), Loadable::Loading);
                    self.pending_repo_requests
                        .push(RepoRequest::load_commit_details(commit_id.clone()));
                }
            }
        }

        // Prune fold state for changes no longer in the DAG.
        let live_change_ids: HashSet<ChangeId> = self
            .entries
            .iter()
            .map(|e| e.commit.unique_change_id())
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
                .entries
                .iter_enumerated()
                .find(|(_, entry)| entry.commit.unique_change_id() == change_id)
            {
                let find_row = |pred: &dyn Fn(&DisplayRow) -> bool| self.rows.iter().position(pred);

                let restored = diff_line_idx
                    .and_then(|li| {
                        let fp = file_path.as_deref()?;
                        find_row(&|r| match r {
                            DisplayRow::DiffLine {
                                entry_idx: ei,
                                file_idx,
                                line_idx,
                            } if *ei == entry_idx && *line_idx == li => self
                                .files_for_entry(entry_idx)
                                .and_then(|f| f.get(file_idx.raw()))
                                .is_some_and(|f| f.path == fp),
                            _ => false,
                        })
                    })
                    .or_else(|| {
                        let fp = file_path.as_deref()?;
                        find_row(&|r| match r {
                            DisplayRow::FileChange {
                                entry_idx: ei,
                                file_idx,
                            } if *ei == entry_idx => self
                                .files_for_entry(entry_idx)
                                .and_then(|f| f.get(file_idx.raw()))
                                .is_some_and(|f| f.path == fp),
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
                JumpTarget::Bookmark(name) => self.jump_to_bookmark(&name),
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
            } => {
                self.status_message = None;
                self.revset = revset;
                self.revset_draft = None;
                self.pending_revset = None;
                self.repo_root = repo_root;
                self.untracked_bookmarks = untracked_bookmarks;
                self.tracked_bookmarks = tracked_bookmarks;
                self.revset_state = Loadable::Loaded(());
                self.apply_entries(entries);
            }
            RepoResult::RevsetFailed { revset, error } => {
                self.pending_revset = None;
                self.revset_draft = Some(revset);
                self.revset_state = Loadable::Failed(error.clone());
                self.status_message = Some("failed to load revset".to_string());
                self.mode = AppMode::CommandOutput {
                    command: "revset error".to_string(),
                    output: error.into_bytes(),
                    success: false,
                };
            }
            RepoResult::CommitDetailsLoaded {
                commit_id,
                files,
                stats,
                is_empty,
            } => {
                self.status_message = None;
                // Update is_empty for this commit (may have been skipped
                // during initial load for merge commits).
                for entry in self.entries.iter_mut() {
                    if entry.commit.graph_id == commit_id {
                        entry.commit.is_empty = is_empty;
                        break;
                    }
                }
                // Re-request diffs for files that were previously unfolded.
                let change_id = self.change_id_for_commit_key(&commit_id);
                if let Some(change_id) = &change_id {
                    for file in &files {
                        let fold_key = super::FileFoldKey {
                            change_id: change_id.clone(),
                            path: file.path.clone(),
                        };
                        if self.unfolded_files.contains(&fold_key) {
                            let cache_key = super::FileDiffCacheKey {
                                commit_id: commit_id.clone(),
                                path: file.path.clone(),
                            };
                            if self
                                .diff_states
                                .get(&cache_key)
                                .is_none_or(Loadable::should_request)
                            {
                                self.diff_states.insert(cache_key, Loadable::Loading);
                                self.pending_repo_requests.push(RepoRequest::load_file_diff(
                                    commit_id.clone(),
                                    file.path.clone(),
                                ));
                            }
                        }
                    }
                }
                self.file_states
                    .insert(commit_id.clone(), Loadable::Loaded(files));
                self.commit_stats_states
                    .insert(commit_id, Loadable::Loaded(stats));
                self.rebuild_rows();
            }
            RepoResult::CommitDetailsFailed { commit_id, error } => {
                self.file_states
                    .insert(commit_id.clone(), Loadable::Failed(error.clone()));
                self.commit_stats_states
                    .insert(commit_id.clone(), Loadable::Failed(error));
                self.status_message = Some(format!("failed to load files for {commit_id}"));
                self.rebuild_rows();
            }
            RepoResult::FileDiffLoaded {
                commit_id,
                path,
                lines,
            } => {
                self.status_message = None;
                self.diff_states.insert(
                    super::FileDiffCacheKey { commit_id, path },
                    Loadable::Loaded(lines),
                );
                self.rebuild_rows();
            }
            RepoResult::FileDiffFailed {
                commit_id,
                path,
                error,
            } => {
                self.diff_states.insert(
                    super::FileDiffCacheKey {
                        commit_id,
                        path: path.clone(),
                    },
                    Loadable::Failed(error),
                );
                self.status_message = Some(format!("failed to load diff for {path}"));
                self.rebuild_rows();
            }
            RepoResult::WorkspaceUpdatedStale { message } => {
                self.status_message = Some(message);
            }
            RepoResult::CommitEmpty { commit_id } => {
                for entry in self.entries.iter_mut() {
                    if entry.commit.graph_id == commit_id {
                        entry.commit.is_empty = true;
                        break;
                    }
                }
            }
            RepoResult::DivergenceInfo { updates } => {
                for (commit_id, is_divergent, is_hidden, change_id_suffix) in updates {
                    for entry in self.entries.iter_mut() {
                        if entry.commit.graph_id == commit_id {
                            entry.commit.is_divergent = is_divergent;
                            entry.commit.is_hidden = is_hidden;
                            entry.commit.change_id_suffix = change_id_suffix;
                            break;
                        }
                    }
                }
            }
            RepoResult::PrefixLengths { updates } => {
                for (commit_id, change_display, change_prefix_len, commit_display, commit_prefix_len) in updates {
                    for entry in self.entries.iter_mut() {
                        if entry.commit.graph_id == commit_id {
                            entry.commit.change_id.display = change_display;
                            entry.commit.change_id.prefix_len = change_prefix_len;
                            entry.commit.commit_id.display = commit_display;
                            entry.commit.commit_id.prefix_len = commit_prefix_len;
                            break;
                        }
                    }
                }
            }
        }
    }
}
