use super::{ActiveView, App, Loadable};
use crate::idx::{
    BookmarkDetailIdx, BookmarkIdx, ConflictHunkIdx, ConflictLineIdx, ConflictSideIdx,
    DescriptionLineIdx, DiffLineIdx, EntryIdx, EvoLogIdx, FileIdx, GraphLineIdx, OpLogDetailIdx,
    OpLogIdx, TagDetailIdx, TagIdx, WorkspaceIdx,
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
            ActiveView::Operations => self.rebuild_op_log_rows(),
            ActiveView::Evolog => self.rebuild_evolog_rows(),
            ActiveView::Workspaces => self.rebuild_workspace_rows(),
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

    fn rebuild_workspace_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);
        self.rows.clear();
        for idx in 0..self.workspace_entries.len() {
            self.rows.push(DisplayRow::WorkspaceItem {
                workspace_idx: WorkspaceIdx::new(idx),
            });
        }
        self.cursor = prev_cursor
            .and_then(|key| self.rows.iter().position(|r| r.key() == key))
            .unwrap_or(self.cursor.min(self.rows.len().saturating_sub(1)));
    }

    fn rebuild_op_log_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);
        self.rows.clear();
        for idx in 0..self.op_log_entries.len() {
            // Apply workspace filter (operations with no workspace always pass).
            if !self.op_log_workspace_filter.is_empty() {
                if let Some(ws) = &self.op_log_entries[idx].workspace {
                    if !self.op_log_workspace_filter.contains(ws) {
                        continue;
                    }
                }
            }
            let oi = OpLogIdx::new(idx);
            self.rows.push(DisplayRow::OpLogItem { op_log_idx: oi });

            // Emit detail lines if this op is unfolded and data is loaded.
            let op_id = &self.op_log_entries[idx].id;
            if self.unfolded_ops.contains(op_id) {
                if let Some(Loadable::Loaded(lines)) = self.op_details.get(op_id) {
                    for li in 0..lines.len() {
                        self.rows.push(DisplayRow::OpLogDetailLine {
                            op_log_idx: oi,
                            line_idx: OpLogDetailIdx::new(li),
                        });
                    }
                }
            }

            // Graph link lines between operations.
            for li in 0..self.op_log_entries[idx].graph.extra.len() {
                self.rows.push(DisplayRow::OpLogGraphLink {
                    op_log_idx: oi,
                    line_idx: GraphLineIdx::new(li),
                });
            }
        }
        if self.op_log_has_more {
            self.rows.push(DisplayRow::OpLogLoadMore);
        }
        // Don't follow the LoadMore sentinel — keep the numeric position so
        // the cursor lands on the first newly loaded entry.
        let prev_cursor = prev_cursor.filter(|k| *k != RowKey::OpLogLoadMore);
        // Detail lines fall back to their parent OpLogItem.
        let fallback = match prev_cursor {
            Some(RowKey::OpLogDetailLine(oi, _)) => Some(RowKey::OpLogItem(oi)),
            _ => None,
        };
        self.cursor = prev_cursor
            .and_then(|key| self.rows.iter().position(|r| r.key() == key))
            .or_else(|| fallback.and_then(|key| self.rows.iter().position(|r| r.key() == key)))
            .unwrap_or(self.cursor.min(self.rows.len().saturating_sub(1)));
    }

    fn rebuild_evolog_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);
        self.rows.clear();
        for idx in 0..self.evolog_entries.len() {
            let ei = EvoLogIdx::new(idx);
            self.rows.push(DisplayRow::EvoLogItem { evolog_idx: ei });
            for li in 0..self.evolog_entries[idx].graph.extra.len() {
                self.rows.push(DisplayRow::EvoLogGraphLink {
                    evolog_idx: ei,
                    line_idx: GraphLineIdx::new(li),
                });
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

                        // If this file is unfolded, show diff lines or conflict hunks.
                        if self.is_file_unfolded(entry_idx, file_idx) {
                            let has_conflict_hunks = self.nodes[entry_idx]
                                .conflict_hunks
                                .get(file_idx_raw)
                                .and_then(|l| l.loaded())
                                .is_some();

                            if has_conflict_hunks {
                                // Show conflict hunks instead of diff.
                                let hunks = self.nodes[entry_idx].conflict_hunks[file_idx_raw]
                                    .loaded()
                                    .unwrap();
                                let mut conflict_num = 0;
                                for (hi, hunk) in hunks.iter().enumerate() {
                                    match &hunk.kind {
                                        crate::dag::ConflictHunkKind::Resolved { lines } => {
                                            for li in 0..lines.len() {
                                                self.rows.push(DisplayRow::ConflictContext {
                                                    entry_idx,
                                                    file_idx,
                                                    hunk_idx: ConflictHunkIdx::new(hi),
                                                    line_idx: ConflictLineIdx::new(li),
                                                });
                                            }
                                        }
                                        crate::dag::ConflictHunkKind::Conflict {
                                            sides, ..
                                        } => {
                                            self.rows.push(DisplayRow::ConflictHeader {
                                                entry_idx,
                                                file_idx,
                                                hunk_idx: ConflictHunkIdx::new(hi),
                                            });
                                            for (si, side) in sides.iter().enumerate() {
                                                for li in 0..side.len() {
                                                    self.rows.push(DisplayRow::ConflictSide {
                                                        entry_idx,
                                                        file_idx,
                                                        hunk_idx: ConflictHunkIdx::new(hi),
                                                        side_idx: ConflictSideIdx::new(si),
                                                        line_idx: ConflictLineIdx::new(li),
                                                    });
                                                }
                                            }
                                            conflict_num += 1;
                                        }
                                    }
                                }
                                let _ = conflict_num;
                            } else if let Some(diff_lines) = self.diff_lines(entry_idx, file_idx) {
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
            Some(RowKey::DescriptionLine(e, _)) => [Some(RowKey::CommitNode(e)), None, None],
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
                DisplayRow::CommitNode { .. }
                | DisplayRow::GraphLink { .. }
                | DisplayRow::OpLogItem { .. }
                | DisplayRow::OpLogGraphLink { .. }
                | DisplayRow::OpLogLoadMore => break,
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
                Some(DisplayRow::CommitNode { .. } | DisplayRow::OpLogItem { .. }) => 2,
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
            Some(DisplayRow::OpLogItem { op_log_idx }) => {
                self.toggle_op_fold(*op_log_idx);
            }
            Some(DisplayRow::OpLogDetailLine { op_log_idx, .. }) => {
                self.toggle_op_fold(*op_log_idx);
            }
            Some(DisplayRow::OpLogLoadMore) => {
                self.request_op_log_load_more();
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
            let file_info = self
                .files_for_entry(entry_idx)
                .and_then(|f| f.get(fi))
                .map(|f| (f.path.clone(), f.old_path.clone(), f.has_conflict));

            if let Some((path, old_path, has_conflict)) = file_info {
                let commit_id = self.commit_id(entry_idx).clone();

                // Request diff lines if needed.
                let should_request_diff = self.nodes[entry_idx]
                    .diffs
                    .get(fi)
                    .is_none_or(Loadable::should_request);
                if should_request_diff {
                    self.nodes[entry_idx].ensure_diffs(fi + 1);
                    self.nodes[entry_idx].diffs[fi] = Loadable::Loading;
                    self.pending_repo_requests.push(RepoRequest::load_file_diff(
                        commit_id.clone(),
                        path.clone(),
                        old_path,
                    ));
                }

                // Also request conflict hunks if the file is conflicted.
                if has_conflict {
                    let should_request_hunks = self.nodes[entry_idx]
                        .conflict_hunks
                        .get(fi)
                        .is_none_or(Loadable::should_request);
                    if should_request_hunks {
                        self.nodes[entry_idx].ensure_conflict_hunks(fi + 1);
                        self.nodes[entry_idx].conflict_hunks[fi] = Loadable::Loading;
                        self.pending_repo_requests
                            .push(RepoRequest::load_conflict_hunks(commit_id, path));
                    }
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

    pub(crate) fn toggle_op_fold(&mut self, op_log_idx: OpLogIdx) {
        let Some(entry) = self.op_log_entries.get(op_log_idx.raw()) else {
            return;
        };
        let op_id = entry.id.clone();

        if self.unfolded_ops.contains(&op_id) {
            self.unfolded_ops.remove(&op_id);
        } else {
            if self
                .op_details
                .get(&op_id)
                .is_none_or(Loadable::should_request)
            {
                self.op_details.insert(op_id.clone(), Loadable::Loading);
                self.pending_repo_requests
                    .push(RepoRequest::load_op_diff(op_id.clone()));
            }
            self.unfolded_ops.insert(op_id.clone());
        }
        self.rebuild_rows();
        if self.unfolded_ops.contains(&op_id) {
            self.scroll_to_show_children();
        }
    }
}
