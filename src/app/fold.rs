use super::{ActiveView, App, Loadable};
use crate::idx::{
    BookmarkDetailIdx, BookmarkIdx, CommandLogDetailIdx, CommandLogIdx, ConflictHunkIdx,
    ConflictLineIdx, ConflictSideIdx, DescriptionLineIdx, DiffLineIdx, EntryIdx, EvoLogIdx,
    FileIdx, GraphLineIdx, OpLogDetailIdx, OpLogIdx, RowIdx, TagDetailIdx, TagIdx, WorkspaceIdx,
};
use crate::repo_service::RepoRequest;
use crate::types::DisplayRow;

/// Restore cursor position after a row rebuild. Tries each fallback key in
/// order, returning the first matching row index. Falls back to clamping the
/// current cursor within bounds.
fn restore_cursor(rows: &[DisplayRow], cursor: RowIdx, fallbacks: &[Option<DisplayRow>]) -> RowIdx {
    fallbacks
        .iter()
        .flatten()
        .find_map(|key| rows.iter().position(|r| *r == *key))
        .map(RowIdx::new)
        .unwrap_or(RowIdx::new(cursor.raw().min(rows.len().saturating_sub(1))))
}

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
            ActiveView::CommandLog => self.rebuild_command_log_rows(),
        }
    }

    fn rebuild_bookmark_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        self.rows.clear();
        for idx in 0..self.views.bookmark_entries.len() {
            let bi = BookmarkIdx::new(idx);
            self.rows
                .push(DisplayRow::BookmarkItem { bookmark_idx: bi });

            // Emit detail rows unless this bookmark is folded.
            let name = &self.views.bookmark_entries[idx].name;
            if !self.views.folded_bookmarks.contains(name) {
                if let Some(details) = self.views.bookmark_details.get(name) {
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
        }

        let fallback = match prev_cursor {
            Some(
                DisplayRow::BookmarkConflictTarget { bookmark_idx, .. }
                | DisplayRow::BookmarkRemoteTarget { bookmark_idx, .. },
            ) => Some(DisplayRow::BookmarkItem { bookmark_idx }),
            _ => None,
        };
        self.cursor = restore_cursor(&self.rows, self.cursor, &[prev_cursor, fallback]);
    }

    fn rebuild_tag_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        self.rows.clear();
        for idx in 0..self.views.tag_entries.len() {
            let ti = TagIdx::new(idx);
            self.rows.push(DisplayRow::TagItem { tag_idx: ti });

            // Emit remote target child rows unless this tag is folded.
            let name = &self.views.tag_entries[idx].name;
            if !self.views.folded_tags.contains(name) {
                if let Some(details) = self.views.tag_details.get(name) {
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
        }
        self.cursor = restore_cursor(&self.rows, self.cursor, &[prev_cursor]);
    }

    fn rebuild_workspace_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        self.rows.clear();
        for idx in 0..self.views.workspace_entries.len() {
            self.rows.push(DisplayRow::WorkspaceItem {
                workspace_idx: WorkspaceIdx::new(idx),
            });
        }
        self.cursor = restore_cursor(&self.rows, self.cursor, &[prev_cursor]);
    }

    fn rebuild_command_log_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        self.rows.clear();
        // Show entries in reverse chronological order (newest first).
        for idx in (0..self.command_log.entries.len()).rev() {
            let li = CommandLogIdx::new(idx);
            self.rows.push(DisplayRow::CommandLogItem { log_idx: li });
            if self.command_log.unfolded.contains(&idx) {
                let output = &self.command_log.entries[idx].output;
                if !output.is_empty() {
                    let text = String::from_utf8_lossy(output);
                    let line_count = text.lines().count().max(1);
                    for line_i in 0..line_count {
                        self.rows.push(DisplayRow::CommandLogDetail {
                            log_idx: li,
                            line_idx: CommandLogDetailIdx::new(line_i),
                        });
                    }
                }
            }
        }
        self.cursor = restore_cursor(&self.rows, self.cursor, &[prev_cursor]);
    }

    fn rebuild_op_log_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        self.rows.clear();
        for idx in 0..self.op_log.entries.len() {
            // Apply workspace filter (operations with no workspace always pass).
            if !self.op_log.workspace_filter.is_empty() {
                if let Some(ws) = &self.op_log.entries[idx].workspace {
                    if !self.op_log.workspace_filter.contains(ws) {
                        continue;
                    }
                }
            }
            let oi = OpLogIdx::new(idx);
            self.rows.push(DisplayRow::OpLogItem { op_log_idx: oi });

            // Emit detail lines if this op is unfolded and data is loaded.
            let op_id = &self.op_log.entries[idx].id;
            if self.op_log.unfolded.contains(op_id) {
                if let Some(Loadable::Loaded(lines)) = self.op_log.details.get(op_id) {
                    for li in 0..lines.len() {
                        self.rows.push(DisplayRow::OpLogDetailLine {
                            op_log_idx: oi,
                            line_idx: OpLogDetailIdx::new(li),
                        });
                    }
                }
            }

            // Graph link lines between operations.
            for li in 0..self.op_log.entries[idx].graph.extra.len() {
                self.rows.push(DisplayRow::OpLogGraphLink {
                    op_log_idx: oi,
                    line_idx: GraphLineIdx::new(li),
                });
            }
        }
        if self.op_log.has_more {
            self.rows.push(DisplayRow::OpLogLoadMore);
        }
        // Don't follow the LoadMore sentinel — keep the numeric position so
        // the cursor lands on the first newly loaded entry.
        let prev_cursor = prev_cursor.filter(|k| *k != DisplayRow::OpLogLoadMore);
        let fallback = match prev_cursor {
            Some(DisplayRow::OpLogDetailLine { op_log_idx, .. }) => {
                Some(DisplayRow::OpLogItem { op_log_idx })
            }
            _ => None,
        };
        self.cursor = restore_cursor(&self.rows, self.cursor, &[prev_cursor, fallback]);
    }

    fn rebuild_evolog_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        self.rows.clear();
        for idx in 0..self.evolog.entries.len() {
            let ei = EvoLogIdx::new(idx);
            self.rows.push(DisplayRow::EvoLogItem { evolog_idx: ei });

            // Emit file change rows if this entry is unfolded.
            let commit_id = &self.evolog.entries[idx].commit_id;
            if self.evolog.unfolded.contains(commit_id) {
                if let Some(Loadable::Loaded(files)) = self.evolog.files.get(commit_id) {
                    for fi in 0..files.len() {
                        let file_idx = FileIdx::new(fi);
                        self.rows.push(DisplayRow::EvoLogFileChange {
                            evolog_idx: ei,
                            file_idx,
                        });
                        // Emit diff lines if this file is unfolded.
                        let key = (commit_id.clone(), files[fi].path.clone());
                        if self.evolog.unfolded_files.contains(&key) {
                            let git_diff =
                                self.toggles.contains(crate::keymap::CommandFlags::GIT_DIFF);
                            let diffs = if git_diff {
                                &self.evolog.file_diffs
                            } else {
                                &self.evolog.file_diffs_cw
                            };
                            if let Some(Loadable::Loaded(lines)) = diffs.get(&key) {
                                for li in 0..lines.len() {
                                    self.rows.push(DisplayRow::EvoLogFileDiffLine {
                                        evolog_idx: ei,
                                        file_idx,
                                        line_idx: DiffLineIdx::new(li),
                                    });
                                }
                            }
                        }
                    }
                }
            }

            for li in 0..self.evolog.entries[idx].graph.extra.len() {
                self.rows.push(DisplayRow::EvoLogGraphLink {
                    evolog_idx: ei,
                    line_idx: GraphLineIdx::new(li),
                });
            }
        }
        self.cursor = restore_cursor(&self.rows, self.cursor, &[prev_cursor]);
    }

    fn rebuild_dag_rows(&mut self) {
        // Remember what the cursor was pointing at so we can restore it.
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();

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
                            if let Some(hunks) = self.nodes[entry_idx]
                                .conflict_hunks
                                .get(file_idx_raw)
                                .and_then(|l| l.loaded())
                            {
                                // Show conflict hunks instead of diff.
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
                                        }
                                    }
                                }
                            } else if let Some(diff_lines) = self.diff_lines(entry_idx, file_idx) {
                                let n = diff_lines.len();
                                for line_idx_raw in 0..n {
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
        let fallbacks: [Option<DisplayRow>; 3] = match prev_cursor {
            Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            }) => [
                Some(DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    line_idx,
                }),
                Some(DisplayRow::FileChange {
                    entry_idx,
                    file_idx,
                }),
                Some(DisplayRow::CommitNode { entry_idx }),
            ],
            Some(DisplayRow::DescriptionLine { entry_idx, .. }) => {
                [Some(DisplayRow::CommitNode { entry_idx }), None, None]
            }
            Some(DisplayRow::FileChange {
                entry_idx,
                file_idx,
            }) => [
                Some(DisplayRow::FileChange {
                    entry_idx,
                    file_idx,
                }),
                Some(DisplayRow::CommitNode { entry_idx }),
                None,
            ],
            Some(
                DisplayRow::ConflictSide {
                    entry_idx,
                    file_idx,
                    hunk_idx,
                    ..
                }
                | DisplayRow::ConflictContext {
                    entry_idx,
                    file_idx,
                    hunk_idx,
                    ..
                },
            ) => [
                Some(DisplayRow::ConflictHeader {
                    entry_idx,
                    file_idx,
                    hunk_idx,
                }),
                Some(DisplayRow::FileChange {
                    entry_idx,
                    file_idx,
                }),
                Some(DisplayRow::CommitNode { entry_idx }),
            ],
            Some(DisplayRow::ConflictHeader {
                entry_idx,
                file_idx,
                ..
            }) => [
                Some(DisplayRow::FileChange {
                    entry_idx,
                    file_idx,
                }),
                Some(DisplayRow::CommitNode { entry_idx }),
                None,
            ],
            Some(DisplayRow::GraphLink { entry_idx, .. }) => {
                [Some(DisplayRow::CommitNode { entry_idx }), None, None]
            }
            Some(key) => [Some(key), None, None],
            None => [None, None, None],
        };
        self.cursor = restore_cursor(&self.rows, self.cursor, &fallbacks);
    }

    /// After unfolding, scroll just enough to make the last child row visible.
    /// Does nothing if the content already fits in the viewport.
    pub fn scroll_to_show_children(&mut self) {
        let viewport = self.last_list_height as usize;
        if viewport == 0 {
            return;
        }

        // Find the last child row index below cursor.
        let mut last_child = self.cursor.raw();
        for idx in (self.cursor.raw() + 1)..self.rows.len() {
            match self.rows[idx] {
                DisplayRow::CommitNode { .. }
                | DisplayRow::GraphLink { .. }
                | DisplayRow::OpLogItem { .. }
                | DisplayRow::OpLogGraphLink { .. }
                | DisplayRow::OpLogLoadMore => break,
                _ => last_child = idx,
            }
        }
        if last_child == self.cursor.raw() {
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
        match self.rows.get(self.cursor.raw()) {
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
            Some(DisplayRow::EvoLogItem { evolog_idx }) => {
                self.toggle_evolog_fold(*evolog_idx);
            }
            Some(DisplayRow::EvoLogFileChange {
                evolog_idx,
                file_idx,
            }) => {
                self.toggle_evolog_file_fold(*evolog_idx, *file_idx);
            }
            Some(DisplayRow::EvoLogFileDiffLine {
                evolog_idx,
                file_idx,
                ..
            }) => {
                self.toggle_evolog_file_fold(*evolog_idx, *file_idx);
            }
            Some(DisplayRow::CommandLogItem { log_idx }) => {
                let idx = log_idx.raw();
                if self.command_log.unfolded.contains(&idx) {
                    self.command_log.unfolded.remove(&idx);
                } else {
                    self.command_log.unfolded.insert(idx);
                }
                self.rebuild_rows();
            }
            Some(DisplayRow::CommandLogDetail { log_idx, .. }) => {
                let idx = log_idx.raw();
                self.command_log.unfolded.remove(&idx);
                self.rebuild_rows();
            }
            Some(DisplayRow::BookmarkItem { bookmark_idx }) => {
                if let Some(entry) = self.views.bookmark_entries.get(bookmark_idx.raw()) {
                    let name = entry.name.clone();
                    if self.views.folded_bookmarks.contains(&name) {
                        self.views.folded_bookmarks.remove(&name);
                    } else {
                        self.views.folded_bookmarks.insert(name);
                    }
                    self.rebuild_rows();
                }
            }
            Some(
                DisplayRow::BookmarkConflictTarget { bookmark_idx, .. }
                | DisplayRow::BookmarkRemoteTarget { bookmark_idx, .. },
            ) => {
                if let Some(entry) = self.views.bookmark_entries.get(bookmark_idx.raw()) {
                    self.views.folded_bookmarks.insert(entry.name.clone());
                    self.rebuild_rows();
                }
            }
            Some(DisplayRow::TagItem { tag_idx }) => {
                if let Some(entry) = self.views.tag_entries.get(tag_idx.raw()) {
                    let name = entry.name.clone();
                    if self.views.folded_tags.contains(&name) {
                        self.views.folded_tags.remove(&name);
                    } else {
                        self.views.folded_tags.insert(name);
                    }
                    self.rebuild_rows();
                }
            }
            Some(DisplayRow::TagRemoteTarget { tag_idx, .. }) => {
                if let Some(entry) = self.views.tag_entries.get(tag_idx.raw()) {
                    self.views.folded_tags.insert(entry.name.clone());
                    self.rebuild_rows();
                }
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
            if let Some(super::PersistentVisualRange::Lines(vr)) = &self.visual.persistent {
                let cid = self.change_id(entry_idx);
                if let Some(file) = self
                    .files_for_entry(entry_idx)
                    .and_then(|f| f.get(file_idx.raw()))
                {
                    if cid == vr.change_id && file.path == vr.path {
                        self.visual.persistent = None;
                    }
                }
            }
            self.visual.mode = None;
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

    pub(crate) fn toggle_evolog_fold(&mut self, evolog_idx: EvoLogIdx) {
        let Some(entry) = self.evolog.entries.get(evolog_idx.raw()) else {
            return;
        };
        let commit_id = entry.commit_id.clone();

        if self.evolog.unfolded.contains(&commit_id) {
            self.evolog.unfolded.remove(&commit_id);
        } else {
            if self
                .evolog
                .files
                .get(&commit_id)
                .is_none_or(Loadable::should_request)
            {
                if let Some(pred_id) = entry.predecessor_ids.first() {
                    self.evolog
                        .files
                        .insert(commit_id.clone(), Loadable::Loading);
                    self.pending_repo_requests
                        .push(RepoRequest::load_evolog_details(
                            pred_id.clone(),
                            commit_id.clone(),
                        ));
                }
            }
            self.evolog.unfolded.insert(commit_id.clone());
        }
        self.rebuild_rows();
        if self.evolog.unfolded.contains(&commit_id) {
            self.scroll_to_show_children();
        }
    }

    pub(crate) fn toggle_evolog_file_fold(&mut self, evolog_idx: EvoLogIdx, file_idx: FileIdx) {
        let Some(entry) = self.evolog.entries.get(evolog_idx.raw()) else {
            return;
        };
        let commit_id = entry.commit_id.clone();
        let files = match self.evolog.files.get(&commit_id) {
            Some(Loadable::Loaded(f)) => f,
            _ => return,
        };
        let Some(file) = files.get(file_idx.raw()) else {
            return;
        };
        let path = file.path.clone();
        let key = (commit_id.clone(), path.clone());

        if self.evolog.unfolded_files.contains(&key) {
            self.evolog.unfolded_files.remove(&key);
        } else {
            if self
                .evolog
                .file_diffs
                .get(&key)
                .is_none_or(Loadable::should_request)
            {
                if let Some(pred_id) = entry.predecessor_ids.first() {
                    self.evolog
                        .file_diffs
                        .insert(key.clone(), Loadable::Loading);
                    self.pending_repo_requests
                        .push(RepoRequest::load_evolog_file_diff(
                            pred_id.clone(),
                            commit_id,
                            path,
                        ));
                }
            }
            self.evolog.unfolded_files.insert(key.clone());
        }
        self.rebuild_rows();
        if self.evolog.unfolded_files.contains(&key) {
            self.scroll_to_show_children();
        }
    }

    pub(crate) fn toggle_op_fold(&mut self, op_log_idx: OpLogIdx) {
        let Some(entry) = self.op_log.entries.get(op_log_idx.raw()) else {
            return;
        };
        let op_id = entry.id.clone();

        if self.op_log.unfolded.contains(&op_id) {
            self.op_log.unfolded.remove(&op_id);
        } else {
            if self
                .op_log
                .details
                .get(&op_id)
                .is_none_or(Loadable::should_request)
            {
                self.op_log.details.insert(op_id.clone(), Loadable::Loading);
                self.pending_repo_requests
                    .push(RepoRequest::load_op_diff(op_id.clone()));
            }
            self.op_log.unfolded.insert(op_id.clone());
        }
        self.rebuild_rows();
        if self.op_log.unfolded.contains(&op_id) {
            self.scroll_to_show_children();
        }
    }
}
