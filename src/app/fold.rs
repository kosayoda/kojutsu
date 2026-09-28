use super::{ActiveView, App, FileTree, Loadable, toggle_membership};
use crate::dag::DiffTarget;
use crate::idx::{
    BookmarkDetailIdx, BookmarkIdx, CommandLogDetailIdx, CommandLogIdx, ConflictHunkIdx,
    ConflictLineIdx, ConflictTermIdx, DescriptionLineIdx, DiffLineIdx, EntryIdx, EvoLogIdx,
    FileIdx, GraphLineIdx, OpLogDetailIdx, OpLogIdx, RowIdx, TagDetailIdx, TagIdx, WorkspaceIdx,
};
use crate::repo_service::RepoRequest;
use crate::types::{DisplayRow, FileOwner, SmallVec};

/// Restore cursor position after a row rebuild. Finds all fallback keys in a
/// single pass over the rows, then picks the highest-priority match. Falls back
/// to clamping the current cursor within bounds.
fn restore_cursor(rows: &[DisplayRow], cursor: RowIdx, fallbacks: &[Option<DisplayRow>]) -> RowIdx {
    // Collect the non-None fallbacks we're looking for.
    let targets: SmallVec<(usize, DisplayRow)> = fallbacks
        .iter()
        .enumerate()
        .filter_map(|(pri, opt)| opt.map(|key| (pri, key)))
        .collect();

    if targets.is_empty() {
        return RowIdx::new(cursor.raw().min(rows.len().saturating_sub(1)));
    }

    // Single pass: find the position of each target, keep best priority.
    let mut best: Option<(usize, usize)> = None; // (priority, row_index)
    for (row_idx, row) in rows.iter().enumerate() {
        for &(pri, ref key) in &targets {
            if *row == *key && best.as_ref().is_none_or(|b| pri < b.0) {
                best = Some((pri, row_idx));
                if pri == 0 {
                    return RowIdx::new(row_idx);
                }
                break;
            }
        }
    }
    best.map(|(_, idx)| RowIdx::new(idx))
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
            ActiveView::Interdiff => self.rebuild_interdiff_rows(),
            ActiveView::Annotate => self.rebuild_annotate_rows(),
        }
    }

    fn rebuild_bookmark_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        self.rows.clear();
        let mut prev_rank: Option<u8> = None;
        for idx in 0..self.views.bookmark_entries.len() {
            let rank = self.views.bookmark_entries[idx].kind.rank();
            if self.show_bookmark_separators && prev_rank.is_some_and(|r| r != rank) {
                self.rows.push(DisplayRow::BookmarkSeparator);
            }
            prev_rank = Some(rank);
            let bi = BookmarkIdx::new(idx);
            self.rows
                .push(DisplayRow::BookmarkItem { bookmark_idx: bi });

            // Emit detail rows only for local/tracking bookmarks (not
            // remote-only entries which share the same base name and would
            // incorrectly show the local bookmark's remote tracking info).
            let entry = &self.views.bookmark_entries[idx];
            let is_remote_only = entry.kind.remote().is_some();
            if !is_remote_only
                && !self.views.folded_bookmarks.contains(&entry.name)
                && let Some(details) = self.views.bookmark_details.get(&entry.name)
            {
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
            if !self.views.folded_tags.contains(name)
                && let Some(details) = self.views.tag_details.get(name)
            {
                let local_commit = details
                    .local_target
                    .as_ref()
                    .map(|lt| &lt.summary.commit_id);
                for ri in 0..details.remote_targets.len() {
                    let rt = &details.remote_targets[ri];
                    if local_commit == Some(&rt.summary.commit_id) {
                        continue;
                    }
                    self.rows.push(DisplayRow::TagRemoteTarget {
                        tag_idx: ti,
                        target_idx: TagDetailIdx::new(ri),
                    });
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
            if self.command_log.unfolded.contains(&li) {
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
            if !self.op_log.workspace_filter.is_empty()
                && let Some(ws) = &self.op_log.entries[idx].workspace
                && !self.op_log.workspace_filter.contains(ws)
            {
                continue;
            }
            let oi = OpLogIdx::new(idx);
            self.rows.push(DisplayRow::OpLogItem { op_log_idx: oi });

            // Emit detail lines if this op is unfolded and data is loaded.
            let op_id = &self.op_log.entries[idx].id;
            if self.op_log.unfolded.contains(op_id)
                && let Some(Loadable::Loaded(lines)) = self.op_log.details.get(op_id)
            {
                for li in 0..lines.len() {
                    self.rows.push(DisplayRow::OpLogDetailLine {
                        op_log_idx: oi,
                        line_idx: OpLogDetailIdx::new(li),
                    });
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
        // Don't follow the LoadMore sentinel: keep the numeric position so
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
        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();
        for (idx, entry) in self.evolog.entries.iter().enumerate() {
            let evolog_idx = EvoLogIdx::new(idx);
            rows.push(DisplayRow::EvoLogItem { evolog_idx });
            if self.evolog.unfolded.contains(&entry.commit_id) {
                self.push_file_rows(FileOwner::EvoLog(evolog_idx), &mut rows);
            }
            rows.extend(
                (0..entry.graph.extra.len()).map(|li| DisplayRow::EvoLogGraphLink {
                    evolog_idx,
                    line_idx: GraphLineIdx::new(li),
                }),
            );
        }
        self.rows = rows;
        let fallbacks = cursor_fallbacks(prev_cursor);
        self.cursor = restore_cursor(&self.rows, self.cursor, &fallbacks);
    }

    fn rebuild_dag_rows(&mut self) {
        // Remember what the cursor was pointing at so we can restore it.
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();

        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();
        for idx_raw in 0..self.nodes.len() {
            let entry_idx = EntryIdx::new(idx_raw);
            self.nodes[entry_idx].row = rows.len();
            self.push_dag_entry_rows(entry_idx, &mut rows);
        }
        self.rows = rows;

        // Restore cursor: try exact match, then fall back to parent file,
        // then parent commit. This handles fold scenarios where the cursor
        // was on a diff line that disappeared when the file was folded.
        let fallbacks = cursor_fallbacks(prev_cursor);
        self.cursor = restore_cursor(&self.rows, self.cursor, &fallbacks);
    }

    /// Emit the display rows for one DAG entry (commit node, description,
    /// files, diffs/conflicts, graph links) into `rows`.
    fn push_dag_entry_rows(&self, entry_idx: EntryIdx, rows: &mut Vec<DisplayRow>) {
        rows.push(DisplayRow::CommitNode { entry_idx });

        if self.is_commit_unfolded(entry_idx) {
            // Description continuation lines (skip the first, already in CommitNode).
            if let Some(full) = &self.nodes[entry_idx].commit.full_description {
                for (i, _) in full.lines().skip(1).enumerate() {
                    rows.push(DisplayRow::DescriptionLine {
                        entry_idx,
                        line_idx: DescriptionLineIdx::new(i),
                    });
                }
            }
            self.push_file_rows(FileOwner::Dag(entry_idx), rows);
        }

        // Extra graph lines (link/pad/term) are rendered as separate
        // GraphLink rows between commits.
        for line_idx_raw in 0..self.nodes[entry_idx].graph.extra.len() {
            rows.push(DisplayRow::GraphLink {
                entry_idx,
                line_idx: GraphLineIdx::new(line_idx_raw),
            });
        }
    }

    /// Emit the rows of an owner's files: each file and, under an unfolded
    /// one, its diff lines or, for a conflicted DAG file, its conflict hunks.
    fn push_file_rows(&self, owner: FileOwner, rows: &mut Vec<DisplayRow>) {
        let Some(files) = self.file_tree(owner).and_then(FileTree::files) else {
            return;
        };
        for file_idx in (0..files.len()).map(FileIdx::new) {
            rows.push(DisplayRow::FileChange { owner, file_idx });
            if !self.is_file_unfolded(owner, file_idx) {
                continue;
            }
            if let FileOwner::Dag(entry_idx) = owner
                && let Some(hunks) = self.shown_conflict_hunks(entry_idx, file_idx)
            {
                self.push_conflict_rows(entry_idx, file_idx, hunks, rows);
            } else if let Some(lines) = self.diff_lines(owner, file_idx) {
                rows.extend((0..lines.len()).map(|li| DisplayRow::DiffLine {
                    owner,
                    file_idx,
                    line_idx: DiffLineIdx::new(li),
                }));
            }
        }
    }

    /// Emit the rows of a conflicted file's hunks, shown instead of its diff.
    fn push_conflict_rows(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        hunks: &[crate::conflict::ConflictHunkKind],
        rows: &mut Vec<DisplayRow>,
    ) {
        for (hi, hunk) in hunks.iter().enumerate() {
            let hunk_ref = crate::types::ConflictHunkRef {
                entry_idx,
                file_idx,
                hunk_idx: ConflictHunkIdx::new(hi),
            };
            match hunk {
                crate::conflict::ConflictHunkKind::Resolved { text } => {
                    // Trim to context around adjacent
                    // conflicts; the hidden middle is an
                    // expandable gap row.
                    let push_ctx = |li: usize, rows: &mut Vec<_>| {
                        rows.push(DisplayRow::ConflictContext {
                            entry_idx,
                            file_idx,
                            hunk_idx: ConflictHunkIdx::new(hi),
                            line_idx: ConflictLineIdx::new(li),
                        });
                    };
                    let n = text.lines.len();
                    if let Some(trim) = self.hunk_trimmed_context(hunk_ref) {
                        for li in 0..trim.head {
                            push_ctx(li, rows);
                        }
                        rows.push(DisplayRow::ConflictGap {
                            entry_idx,
                            file_idx,
                            hunk_idx: ConflictHunkIdx::new(hi),
                        });
                        for li in (n - trim.tail)..n {
                            push_ctx(li, rows);
                        }
                    } else {
                        for li in 0..n {
                            push_ctx(li, rows);
                        }
                    }
                }
                crate::conflict::ConflictHunkKind::Conflict { terms } => {
                    rows.push(DisplayRow::ConflictHeader {
                        entry_idx,
                        file_idx,
                        hunk_idx: ConflictHunkIdx::new(hi),
                    });
                    // A hand-edited resolution renders
                    // above the terms.
                    if let Some(crate::conflict::ConflictPick::Edited(text)) =
                        self.hunk_pick(hunk_ref)
                    {
                        for li in 0..text.lines.len().max(1) {
                            rows.push(DisplayRow::ConflictEdited {
                                entry_idx,
                                file_idx,
                                hunk_idx: ConflictHunkIdx::new(hi),
                                line_idx: ConflictLineIdx::new(li),
                            });
                        }
                    }
                    let base_folded = self.hunk_base_folded(hunk_ref);
                    // Display order: sides first, bases
                    // last (dimmed; folded to a stub by
                    // default). Storage order stays
                    // interleaved for assembly.
                    let display_order = terms
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| t.kind.is_side())
                        .chain(terms.iter().enumerate().filter(|(_, t)| !t.kind.is_side()));
                    for (ti, term) in display_order {
                        // A folded base renders as a
                        // one-line stub.
                        let n = if !term.kind.is_side() && base_folded {
                            1
                        } else {
                            // Empty terms (deleted or
                            // emptied file) still get one
                            // row so the side is visible
                            // and pickable.
                            term.text.lines.len().max(1)
                        };
                        for li in 0..n {
                            rows.push(DisplayRow::ConflictTerm {
                                entry_idx,
                                file_idx,
                                hunk_idx: ConflictHunkIdx::new(hi),
                                term_idx: ConflictTermIdx::new(ti),
                                line_idx: ConflictLineIdx::new(li),
                            });
                        }
                    }
                }
            }
        }
    }

    /// Rebuild the rows an owner's files appear in.
    fn rebuild_owner_rows(&mut self, owner: FileOwner) {
        match owner {
            FileOwner::Dag(entry_idx) => self.rebuild_entry_rows(entry_idx),
            FileOwner::EvoLog(_) | FileOwner::Interdiff => self.rebuild_rows(),
        }
    }

    /// Apply a deferred rebuild scope.
    pub fn apply_rebuild(&mut self, scope: super::RebuildScope) {
        match scope {
            super::RebuildScope::None => {}
            super::RebuildScope::Full => self.rebuild_rows(),
            super::RebuildScope::Entries(entries) => {
                for entry_idx in entries {
                    self.rebuild_entry_rows(entry_idx);
                }
            }
        }
    }

    /// Regenerate the rows of a single DAG entry in place, shifting the rows
    /// of following entries instead of rebuilding the whole list.
    /// Falls back to a full rebuild if the entry's row range can't be located.
    pub(super) fn rebuild_entry_rows(&mut self, entry_idx: EntryIdx) {
        if self.active_view != ActiveView::Dag {
            // DAG rows aren't displayed; they are rebuilt on view switch.
            return;
        }
        let Some(node) = self.nodes.get(entry_idx) else {
            return;
        };
        let start = node.row;
        let end = match self.nodes.get(EntryIdx::new(entry_idx.raw() + 1)) {
            Some(next) => next.row,
            None => self.rows.len(),
        };
        let range_valid = start <= end
            && end <= self.rows.len()
            && matches!(
                self.rows.get(start),
                Some(DisplayRow::CommitNode { entry_idx: e }) if *e == entry_idx
            );
        if !range_valid {
            // Row pointers are stale: regenerate everything.
            self.rebuild_dag_rows();
            return;
        }

        let prev_cursor = self.rows.get(self.cursor.raw()).copied();

        let mut new_rows = Vec::with_capacity(end - start);
        self.push_dag_entry_rows(entry_idx, &mut new_rows);
        let new_end = start + new_rows.len();
        let delta = new_end as isize - end as isize;
        self.rows.splice(start..end, new_rows);

        if delta != 0 {
            // Shift the row pointers of all following entries.
            for idx_raw in (entry_idx.raw() + 1)..self.nodes.len() {
                let node = &mut self.nodes[EntryIdx::new(idx_raw)];
                node.row = node.row.saturating_add_signed(delta);
            }
            if self.scroll >= end {
                self.scroll = self.scroll.saturating_add_signed(delta);
            }
            // Search match rows shifted with the splice.
            if self.search.is_some() {
                self.refresh_search_matches();
            }
        }

        // Restore the cursor with the same semantics as a full rebuild.
        let cursor = self.cursor.raw();
        if cursor >= end {
            self.cursor = RowIdx::new(cursor.saturating_add_signed(delta));
        } else if cursor >= start {
            // The cursor was inside the regenerated range: re-find its row
            // (or a fallback) within the entry's new rows.
            let fallbacks = cursor_fallbacks(prev_cursor);
            let relative = restore_cursor(
                &self.rows[start..new_end],
                RowIdx::new(cursor - start),
                &fallbacks,
            );
            self.cursor = RowIdx::new(start + relative.raw());
        }
    }
}

/// Cursor fallback chain for row rebuilds: exact match, then the parent
/// file, then the row the file hangs under.
fn cursor_fallbacks(prev_cursor: Option<DisplayRow>) -> [Option<DisplayRow>; 3] {
    match prev_cursor {
        Some(
            row @ DisplayRow::DiffLine {
                owner, file_idx, ..
            },
        ) => [
            Some(row),
            Some(DisplayRow::FileChange { owner, file_idx }),
            Some(owner_row(owner)),
        ],
        Some(row @ DisplayRow::FileChange { owner, .. }) => {
            [Some(row), Some(owner_row(owner)), None]
        }
        Some(
            DisplayRow::DescriptionLine { entry_idx, .. } | DisplayRow::GraphLink { entry_idx, .. },
        ) => [Some(DisplayRow::CommitNode { entry_idx }), None, None],
        Some(
            DisplayRow::ConflictTerm {
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
            }
            | DisplayRow::ConflictGap {
                entry_idx,
                file_idx,
                hunk_idx,
            }
            | DisplayRow::ConflictEdited {
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
                owner: FileOwner::Dag(entry_idx),
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
                owner: FileOwner::Dag(entry_idx),
                file_idx,
            }),
            Some(DisplayRow::CommitNode { entry_idx }),
            None,
        ],
        Some(key) => [Some(key), None, None],
        None => [None, None, None],
    }
}

/// The row an owner's files hang under.
fn owner_row(owner: FileOwner) -> DisplayRow {
    match owner {
        FileOwner::Dag(entry_idx) => DisplayRow::CommitNode { entry_idx },
        FileOwner::EvoLog(evolog_idx) => DisplayRow::EvoLogItem { evolog_idx },
        FileOwner::Interdiff => DisplayRow::InterdiffHeader,
    }
}

impl App {
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
                | DisplayRow::EvoLogItem { .. }
                | DisplayRow::EvoLogGraphLink { .. }
                | DisplayRow::OpLogItem { .. }
                | DisplayRow::OpLogGraphLink { .. }
                | DisplayRow::OpLogLoadMore
                | DisplayRow::AnnotateLine { .. } => break,
                _ => last_child = idx,
            }
        }
        if last_child == self.cursor.raw() {
            return; // Nothing unfolded (data not loaded yet)
        }

        // Count display lines from the current offset to the last child row.
        let mut lines = 0;
        for idx in self.scroll..=last_child {
            lines += self.row_display_lines(idx);
        }

        // Only scroll if the last child extends beyond the viewport.
        if lines <= viewport {
            return;
        }

        // Scroll down the minimum number of rows to bring the last child into view.
        let overflow = lines - viewport;
        let mut skipped = 0;
        while skipped < overflow && self.scroll < last_child {
            skipped += self.row_display_lines(self.scroll);
            self.scroll += 1;
        }
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
            // Folding on a diff line folds the parent file.
            Some(
                DisplayRow::FileChange { owner, file_idx }
                | DisplayRow::DiffLine {
                    owner, file_idx, ..
                },
            ) => {
                self.toggle_file_fold(*owner, *file_idx);
            }
            Some(DisplayRow::ConflictTerm {
                entry_idx,
                file_idx,
                hunk_idx,
                term_idx,
                ..
            }) => {
                // On a base row, toggle the base block; on a side row,
                // fold the parent file (mirrors diff-line behavior).
                let (entry_idx, file_idx, hunk_idx, term_idx) =
                    (*entry_idx, *file_idx, *hunk_idx, *term_idx);
                if !self.toggle_conflict_base_fold(entry_idx, file_idx, hunk_idx, term_idx) {
                    self.toggle_file_fold(FileOwner::Dag(entry_idx), file_idx);
                }
            }
            Some(
                DisplayRow::ConflictHeader {
                    entry_idx,
                    file_idx,
                    ..
                }
                | DisplayRow::ConflictContext {
                    entry_idx,
                    file_idx,
                    ..
                },
            ) => {
                self.toggle_file_fold(FileOwner::Dag(*entry_idx), *file_idx);
            }
            Some(DisplayRow::ConflictGap {
                entry_idx,
                file_idx,
                hunk_idx,
            }) => {
                self.expand_conflict_context(*entry_idx, *file_idx, *hunk_idx);
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
            Some(DisplayRow::CommandLogItem { log_idx }) => {
                toggle_membership(&mut self.command_log.unfolded, *log_idx);
                self.rebuild_rows();
            }
            Some(DisplayRow::CommandLogDetail { log_idx, .. }) => {
                self.command_log.unfolded.remove(log_idx);
                self.rebuild_rows();
            }
            Some(DisplayRow::BookmarkItem { bookmark_idx }) => {
                if let Some(entry) = self.views.bookmark_entries.get(bookmark_idx.raw()) {
                    let name = entry.name.clone();
                    toggle_membership(&mut self.views.folded_bookmarks, name);
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
                    toggle_membership(&mut self.views.folded_tags, name);
                    self.rebuild_rows();
                }
            }
            Some(DisplayRow::TagRemoteTarget { tag_idx, .. }) => {
                if let Some(entry) = self.views.tag_entries.get(tag_idx.raw()) {
                    self.views.folded_tags.insert(entry.name.clone());
                    self.rebuild_rows();
                }
            }
            Some(DisplayRow::AnnotateLine { line_idx }) => {
                let unfolded = toggle_membership(&mut self.annotate.unfolded_lines, *line_idx);
                self.rebuild_rows();
                if unfolded {
                    self.scroll_to_show_children();
                }
            }
            Some(DisplayRow::AnnotateDetail { line_idx, .. }) => {
                self.annotate.unfolded_lines.remove(line_idx);
                self.rebuild_rows();
            }
            _ => {}
        }
    }

    pub(crate) fn toggle_commit_fold(&mut self, entry_idx: EntryIdx) {
        let change_id = self.change_id(entry_idx);
        let unfolded = toggle_membership(&mut self.unfolded_commits, change_id);
        if unfolded {
            let request = self.nodes[entry_idx].files.request_summary();
            self.pending_repo_requests.extend(request);
        }
        self.rebuild_entry_rows(entry_idx);
        if unfolded {
            self.scroll_to_show_children();
        }
    }

    /// Expand a trimmed resolved section to its full content (tab on the
    /// gap row).
    pub(crate) fn expand_conflict_context(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        hunk_idx: ConflictHunkIdx,
    ) {
        let hunk = crate::types::ConflictHunkRef {
            entry_idx,
            file_idx,
            hunk_idx,
        };
        if matches!(
            self.conflict_hunk(hunk),
            Some(crate::conflict::ConflictHunkKind::Resolved { .. })
        ) {
            self.update_hunk_ui(hunk, |s| s.expanded = true);
            self.rebuild_entry_rows(entry_idx);
        }
    }

    /// Toggle the base block of a conflict hunk between its one-line stub
    /// and full content. Returns whether the term was a base (side terms
    /// are not handled here).
    pub(crate) fn toggle_conflict_base_fold(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        hunk_idx: ConflictHunkIdx,
        term_idx: ConflictTermIdx,
    ) -> bool {
        let hunk = crate::types::ConflictHunkRef {
            entry_idx,
            file_idx,
            hunk_idx,
        };
        let is_base = matches!(
            self.conflict_hunk(hunk),
            Some(crate::conflict::ConflictHunkKind::Conflict { terms })
                if terms.get(term_idx.raw()).is_some_and(|t| !t.kind.is_side())
        );
        if is_base {
            self.update_hunk_ui(hunk, |s| s.base_folded = !s.base_folded);
            self.rebuild_entry_rows(entry_idx);
        }
        is_base
    }

    pub(crate) fn toggle_file_fold(&mut self, owner: FileOwner, file_idx: FileIdx) {
        if self.file(owner, file_idx).is_none() {
            return;
        }
        let unfold = !self.is_file_unfolded(owner, file_idx);
        if unfold {
            self.request_file_contents(owner, file_idx);
        } else if let FileOwner::Dag(entry_idx) = owner {
            // Clear visual state if it's for this file.
            if let Some(super::PersistentVisualRange::Lines(vr)) = &self.visual.persistent
                && let Some(file) = self.file(owner, file_idx)
                && self.change_id(entry_idx) == vr.change_id
                && file.path == vr.path
            {
                self.visual.persistent = None;
            }
            self.visual.mode = None;
        }
        self.set_file_unfolded(owner, file_idx, unfold);
        self.rebuild_owner_rows(owner);
        if unfold {
            self.scroll_to_show_children();
        }
    }

    /// Request what an unfolded file shows: its diff and, for a conflicted
    /// DAG file, its conflict hunks.
    pub(super) fn request_file_contents(&mut self, owner: FileOwner, file_idx: FileIdx) {
        let request = self
            .file_tree_mut(owner)
            .and_then(|tree| tree.request_diff(file_idx));
        self.pending_repo_requests.extend(request);
        if let FileOwner::Dag(entry_idx) = owner {
            let request = self.nodes[entry_idx].request_conflict_hunks(file_idx);
            self.pending_repo_requests.extend(request);
        }
    }

    pub(crate) fn toggle_evolog_fold(&mut self, evolog_idx: EvoLogIdx) {
        let Some(entry) = self.evolog.entries.get(evolog_idx.raw()) else {
            return;
        };
        let commit_id = entry.commit_id.clone();
        let target = DiffTarget::Evolution {
            predecessors: entry.predecessor_ids.clone(),
            commit: commit_id.clone(),
        };
        let unfolded = toggle_membership(&mut self.evolog.unfolded, commit_id.clone());
        if unfolded {
            let request = self
                .evolog
                .files
                .entry(commit_id)
                .or_insert_with(|| FileTree::new(target))
                .request_summary();
            self.pending_repo_requests.extend(request);
        }
        self.rebuild_rows();
        if unfolded {
            self.scroll_to_show_children();
        }
    }

    fn rebuild_interdiff_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();
        rows.push(DisplayRow::InterdiffHeader);
        self.push_file_rows(FileOwner::Interdiff, &mut rows);
        self.rows = rows;
        let fallbacks = cursor_fallbacks(prev_cursor);
        self.cursor = restore_cursor(&self.rows, self.cursor, &fallbacks);
    }

    fn rebuild_annotate_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        self.rows.clear();

        if let Loadable::Loaded(lines) = &self.annotate.lines {
            for (li, line) in lines.iter().enumerate() {
                let line_idx = crate::idx::AnnotateLineIdx::new(li);
                self.rows.push(DisplayRow::AnnotateLine { line_idx });

                if self.annotate.unfolded_lines.contains(&line_idx)
                    && let Some(info) = self.annotate.commit_info.get(&line.commit_id)
                {
                    for di in 0..info.detail_row_count() {
                        self.rows.push(DisplayRow::AnnotateDetail {
                            line_idx,
                            detail_idx: crate::idx::AnnotateDetailIdx::new(di),
                        });
                    }
                }
            }
        }

        // Jump to target line after time-travel reload.
        if let Some(lines) = self.annotate.lines.loaded()
            && let Some(target) = self.annotate.target_line.take()
            && let Some(row_idx) = self.rows.iter().position(|r| {
                matches!(r, DisplayRow::AnnotateLine { line_idx }
                        if lines.get(line_idx.raw())
                            .is_some_and(|l| l.line_number == target))
            })
        {
            self.cursor = crate::idx::RowIdx::new(row_idx);
            return;
        }

        self.cursor = restore_cursor(&self.rows, self.cursor, &[prev_cursor]);
    }

    pub(crate) fn toggle_op_fold(&mut self, op_log_idx: OpLogIdx) {
        let Some(entry) = self.op_log.entries.get(op_log_idx.raw()) else {
            return;
        };
        let op_id = entry.id.clone();
        let unfolded = toggle_membership(&mut self.op_log.unfolded, op_id.clone());
        if unfolded
            && self
                .op_log
                .details
                .entry(op_id.clone())
                .or_insert(Loadable::NotRequested)
                .begin()
        {
            self.pending_repo_requests
                .push(RepoRequest::OpDiff { op_id });
        }
        self.rebuild_rows();
        if unfolded {
            self.scroll_to_show_children();
        }
    }
}
