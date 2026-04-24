use super::{ActiveView, App, Loadable};
use crate::idx::{
    BookmarkDetailIdx, BookmarkIdx, DescriptionLineIdx, DiffLineIdx, EntryIdx, FileIdx,
    GraphLineIdx, TagDetailIdx, TagIdx,
};
use crate::repo_service::RepoRequest;
use crate::types::{DisplayRow, RowKey};

impl App {
    /// Rebuild the flattened row list from current fold state.
    pub fn rebuild_rows(&mut self) {
        match self.active_view {
            ActiveView::Dag => self.rebuild_dag_rows(),
            ActiveView::Bookmarks => self.rebuild_bookmark_rows(),
            ActiveView::Tags => self.rebuild_tag_rows(),
        }
    }

    fn rebuild_bookmark_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);
        self.rows.clear();
        for idx in 0..self.bookmark_entries.len() {
            let bi = BookmarkIdx::new(idx);
            self.rows
                .push(DisplayRow::BookmarkItem { bookmark_idx: bi });

            // Always emit detail rows (conflict targets + remote tracking).
            if let Some(details) = self.bookmark_details.get(&self.bookmark_entries[idx].name) {
                for ti in 0..details.conflict_targets.len() {
                    self.rows.push(DisplayRow::BookmarkConflictTarget {
                        bookmark_idx: bi,
                        target_idx: BookmarkDetailIdx::new(ti),
                    });
                }
                for ti in 0..details.remote_targets.len() {
                    self.rows.push(DisplayRow::BookmarkRemoteTarget {
                        bookmark_idx: bi,
                        target_idx: BookmarkDetailIdx::new(ti),
                    });
                }
            }
        }

        // Cursor restore: try exact match, then fall back to parent bookmark.
        let fallback: Option<RowKey> = match prev_cursor {
            Some(RowKey::BookmarkConflictTarget(bi, _) | RowKey::BookmarkRemoteTarget(bi, _)) => {
                Some(RowKey::BookmarkItem(bi))
            }
            _ => None,
        };

        self.cursor = prev_cursor
            .and_then(|key| self.rows.iter().position(|r| r.key() == key))
            .or_else(|| fallback.and_then(|key| self.rows.iter().position(|r| r.key() == key)))
            .unwrap_or(self.cursor.min(self.rows.len().saturating_sub(1)));
    }

    fn rebuild_tag_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);
        self.rows.clear();
        for idx in 0..self.tag_entries.len() {
            let ti = TagIdx::new(idx);
            self.rows.push(DisplayRow::TagItem { tag_idx: ti });

            // Emit remote target child rows (skip if same commit as local).
            if let Some(details) = self.tag_details.get(&self.tag_entries[idx].name) {
                let local_commit = details.local_target.as_ref().map(|lt| &lt.commit_id);
                for ri in 0..details.remote_targets.len() {
                    let rt = &details.remote_targets[ri];
                    if local_commit == Some(&rt.commit_id) {
                        continue;
                    }
                    self.rows.push(DisplayRow::TagRemoteTarget {
                        tag_idx: ti,
                        target_idx: TagDetailIdx::new(ri),
                    });
                }
            }
        }
        self.cursor = prev_cursor
            .and_then(|key| self.rows.iter().position(|r| r.key() == key))
            .unwrap_or(self.cursor.min(self.rows.len().saturating_sub(1)));
    }

    fn rebuild_dag_rows(&mut self) {
        // Remember what the cursor was pointing at so we can restore it.
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);

        self.rows.clear();
        for idx_raw in 0..self.nodes.len() {
            let entry_idx = EntryIdx::new(idx_raw);
            self.nodes[entry_idx].row = self.rows.len();
            self.rows.push(DisplayRow::CommitNode { entry_idx });

            if self.is_commit_unfolded(entry_idx) {
                // Description continuation lines (skip first line — already in CommitNode).
                if let Some(full) = &self.nodes[entry_idx].commit.full_description {
                    for (i, _) in full.lines().skip(1).enumerate() {
                        self.rows.push(DisplayRow::DescriptionLine {
                            entry_idx,
                            line_idx: DescriptionLineIdx::new(i),
                        });
                    }
                }
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
            for line_idx_raw in 0..self.nodes[entry_idx].graph.extra.len() {
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
            Some(RowKey::DescriptionLine(e, _)) => [
                Some(RowKey::CommitNode(e)),
                None,
                None,
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
        if self.is_commit_unfolded(entry_idx) {
            self.unfolded_commits.remove(&change_id);
        } else {
            if self.nodes[entry_idx].files.should_request() {
                let commit_id = self.commit_id(entry_idx).clone();
                self.nodes[entry_idx].files = Loadable::Loading;
                self.nodes[entry_idx].stats = Loadable::Loading;
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
            let fi = file_idx.raw();
            let should_request = self.nodes[entry_idx]
                .diffs
                .get(fi)
                .is_none_or(Loadable::should_request);
            if should_request {
                // Clone file data before mutating nodes.
                let file_info = self
                    .files_for_entry(entry_idx)
                    .and_then(|f| f.get(fi))
                    .map(|f| (f.path.clone(), f.old_path.clone()));
                if let Some((path, old_path)) = file_info {
                    let commit_id = self.commit_id(entry_idx).clone();
                    self.nodes[entry_idx].ensure_diffs(fi + 1);
                    self.nodes[entry_idx].diffs[fi] = Loadable::Loading;
                    self.pending_repo_requests
                        .push(RepoRequest::load_file_diff(commit_id, path, old_path));
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
