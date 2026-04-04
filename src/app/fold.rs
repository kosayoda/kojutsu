use super::{App, Loadable};
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx, GraphLineIdx};
use crate::repo_service::RepoRequest;
use crate::types::{DisplayRow, RowKey};

impl App {
    /// Rebuild the flattened row list from current fold state.
    pub fn rebuild_rows(&mut self) {
        // Remember what the cursor was pointing at so we can restore it.
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);

        self.rows.clear();
        for (entry_idx, gl) in self.graph.iter_enumerated() {
            self.rows.push(DisplayRow::CommitNode { entry_idx });

            if self.is_commit_unfolded(entry_idx) {
                if let Some(files) = self.files_for_entry(entry_idx) {
                    for file_idx_raw in 0..files.len() {
                        let file_idx = FileIdx::new(file_idx_raw);
                        self.rows.push(DisplayRow::FileChange {
                            entry_idx,
                            file_idx,
                        });

                        // If this file is unfolded, show diff lines.
                        if self.is_file_unfolded(entry_idx, file_idx) {
                            if let Some(diff_lines) = self.diff_lines(entry_idx, file_idx) {
                                for line_idx_raw in 0..diff_lines.len() {
                                    self.rows.push(DisplayRow::DiffLine {
                                        entry_idx,
                                        file_idx,
                                        line_idx: DiffLineIdx::new(line_idx_raw),
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Extra graph lines (link/pad/term) are rendered as separate
            // GraphLink rows between commits.
            for line_idx_raw in 0..gl.extra.len() {
                self.rows.push(DisplayRow::GraphLink {
                    entry_idx,
                    line_idx: GraphLineIdx::new(line_idx_raw),
                });
            }
        }

        // Restore cursor: try exact match, then fall back to parent file,
        // then parent commit. This handles fold scenarios where the cursor
        // was on a diff line that disappeared when the file was folded.
        let fallbacks: [Option<RowKey>; 3] = match prev_cursor {
            Some(RowKey::DiffLine(e, f, l)) => [
                Some(RowKey::DiffLine(e, f, l)),
                Some(RowKey::FileChange(e, f)),
                Some(RowKey::CommitNode(e)),
            ],
            Some(RowKey::FileChange(e, f)) => [
                Some(RowKey::FileChange(e, f)),
                Some(RowKey::CommitNode(e)),
                None,
            ],
            Some(key) => [Some(key), None, None],
            None => [None, None, None],
        };

        self.cursor = fallbacks
            .iter()
            .flatten()
            .find_map(|key| self.rows.iter().position(|r| r.key() == *key))
            .unwrap_or(0);
    }

    /// After unfolding, adjust scroll so the cursor row is near the top of the
    /// viewport, showing as many child rows as possible.
    /// After unfolding, scroll just enough to make the last child row visible.
    /// Does nothing if the content already fits in the viewport.
    pub fn scroll_to_show_children(&mut self) {
        let viewport = self.last_list_height as usize;
        if viewport == 0 {
            return;
        }

        // Find the last child row index below cursor.
        let mut last_child = self.cursor;
        for idx in (self.cursor + 1)..self.rows.len() {
            match self.rows[idx] {
                DisplayRow::CommitNode { .. } | DisplayRow::GraphLink { .. } => break,
                _ => last_child = idx,
            }
        }
        if last_child == self.cursor {
            return; // Nothing unfolded (data not loaded yet)
        }

        // Count display lines from the current offset to the last child row.
        let offset = self.list_state.offset();
        let mut lines = 0;
        for idx in offset..=last_child {
            lines += match self.rows.get(idx) {
                Some(DisplayRow::CommitNode { .. }) => 2,
                Some(_) => 1,
                None => break,
            };
        }

        // Only scroll if the last child extends beyond the viewport.
        if lines <= viewport {
            return;
        }

        // Scroll by the minimum amount to bring the last child into view.
        let overflow = lines - viewport;
        *self.list_state.offset_mut() = offset + overflow;
    }

    /// Toggle fold on the currently selected row.
    ///
    /// - On a commit row: toggle showing file changes.
    /// - On a file row: toggle showing diff hunks.
    pub fn toggle_fold(&mut self) {
        match self.rows.get(self.cursor) {
            Some(DisplayRow::CommitNode { entry_idx }) => {
                self.toggle_commit_fold(*entry_idx);
            }
            Some(DisplayRow::FileChange {
                entry_idx,
                file_idx,
            }) => {
                self.toggle_file_fold(*entry_idx, *file_idx);
            }
            Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                ..
            }) => {
                // Folding on a diff line folds the parent file.
                self.toggle_file_fold(*entry_idx, *file_idx);
            }
            _ => {}
        }
    }

    pub(crate) fn toggle_commit_fold(&mut self, entry_idx: EntryIdx) {
        let change_id = self.change_id(entry_idx);
        let commit_id = self.commit_id(entry_idx).clone();
        if self.is_commit_unfolded(entry_idx) {
            self.unfolded_commits.remove(&change_id);
        } else {
            if self
                .file_states
                .get(&commit_id)
                .is_none_or(Loadable::should_request)
            {
                self.file_states
                    .insert(commit_id.clone(), Loadable::Loading);
                self.commit_stats_states
                    .insert(commit_id.clone(), Loadable::Loading);
                self.pending_repo_requests
                    .push(RepoRequest::load_commit_details(commit_id));
            }
            self.unfolded_commits.insert(change_id);
        }
        self.rebuild_rows();
        if self.is_commit_unfolded(entry_idx) {
            self.scroll_to_show_children();
        }
    }

    pub(crate) fn toggle_file_fold(&mut self, entry_idx: EntryIdx, file_idx: FileIdx) {
        let Some(fold_key) = self.file_fold_key(entry_idx, file_idx) else {
            return;
        };
        let currently_unfolded = self.unfolded_files.contains(&fold_key);

        if currently_unfolded {
            self.unfolded_files.remove(&fold_key);
            // Clear visual state if it's for this file.
            if let Some(super::PersistentVisualRange::Lines(vr)) = &self.visual_persistent {
                let cid = self.change_id(entry_idx);
                if let Some(file) = self
                    .files_for_entry(entry_idx)
                    .and_then(|f| f.get(file_idx.raw()))
                {
                    if cid == vr.change_id && file.path == vr.path {
                        self.visual_persistent = None;
                    }
                }
            }
            self.visual = None;
        } else {
            let cache_key = self.file_cache_key(entry_idx, file_idx);
            let should_request = cache_key
                .as_ref()
                .is_none_or(|k| self.diff_states.get(k).is_none_or(Loadable::should_request));
            if should_request {
                if let Some(cache_key) = cache_key {
                    let commit_id = cache_key.commit_id.clone();
                    let path = cache_key.path.clone();
                    self.diff_states.insert(cache_key, Loadable::Loading);
                    self.pending_repo_requests
                        .push(RepoRequest::load_file_diff(commit_id, path));
                }
            }
            self.unfolded_files.insert(fold_key);
        }
        self.rebuild_rows();
        if !currently_unfolded {
            // We just unfolded — scroll to show child rows.
            self.scroll_to_show_children();
        }
    }
}
