use super::placeholder::SlotState;
use super::{App, FileTree, Loadable, toggle_membership};
use crate::dag::DiffTarget;
use crate::idx::{
    BookmarkDetailIdx, BookmarkIdx, CommandLogDetailIdx, ConflictHunkIdx, ConflictLineIdx,
    ConflictTermIdx, DescriptionLineIdx, DiffLineIdx, EntryIdx, EvoLogIdx, FileIdx, GraphLineIdx,
    OpLogDetailIdx, OpLogIdx, RowIdx, TagDetailIdx, TagIdx, WorkspaceIdx,
};
use crate::repo_service::RepoRequest;
use crate::types::ActiveView;
use crate::types::{ContentSlot, DisplayRow, FileOwner, SmallVec};

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
        // Matches are row positions, so new rows need them found again.
        self.refresh_search_matches();
    }

    fn rebuild_bookmark_rows(&mut self) {
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        self.rows.clear();
        let mut prev_rank: Option<u8> = None;
        for idx in 0..self.views.bookmark_entries.len() {
            let rank = self.views.bookmark_entries[idx].kind.rank();
            if self.views.show_bookmark_separators && prev_rank.is_some_and(|r| r != rank) {
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
                for (ri, rt) in details.remote_targets.iter().enumerate() {
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
        for li in self.command_log.indices().rev() {
            self.rows.push(DisplayRow::CommandLogItem { log_idx: li });
            if self.command_log.unfolded.contains(&li)
                && let Some(entry) = self.command_log.get(li)
            {
                let output = &entry.output;
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
        // "Load more" lands on the first entry it brought: the first shown
        // after the last one shown before.
        let last_shown = match prev_cursor {
            Some(DisplayRow::OpLogLoadMore) => self.rows.iter().rev().find_map(|row| match row {
                DisplayRow::OpLogItem { op_log_idx } => Some(*op_log_idx),
                _ => None,
            }),
            _ => None,
        };
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

            // Emit detail lines if this op is unfolded: the lines once
            // loaded, a placeholder until then.
            let op_id = &self.op_log.entries[idx].id;
            if self.op_log.unfolded.contains(op_id) {
                match self.op_log.details.get(op_id) {
                    Some(Loadable::Loaded(lines)) => {
                        for li in 0..lines.len() {
                            self.rows.push(DisplayRow::OpLogDetailLine {
                                op_log_idx: oi,
                                line_idx: OpLogDetailIdx::new(li),
                            });
                        }
                    }
                    _ if self.slot_state(ContentSlot::Operation(oi)).needs_row() => self
                        .rows
                        .push(DisplayRow::Placeholder(ContentSlot::Operation(oi))),
                    _ => {}
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
        if self.op_log.entries.is_empty() && self.slot_state(ContentSlot::View).needs_row() {
            self.rows.push(DisplayRow::Placeholder(ContentSlot::View));
        }
        let fallbacks = match prev_cursor {
            Some(DisplayRow::OpLogLoadMore) => {
                let first_new = last_shown.and_then(|last| {
                    self.rows.iter().copied().find(|row| {
                        matches!(row, DisplayRow::OpLogItem { op_log_idx } if *op_log_idx > last)
                    })
                });
                [first_new, prev_cursor]
            }
            Some(
                DisplayRow::OpLogDetailLine { op_log_idx, .. }
                | DisplayRow::Placeholder(ContentSlot::Operation(op_log_idx)),
            ) => [prev_cursor, Some(DisplayRow::OpLogItem { op_log_idx })],
            _ => [prev_cursor, None],
        };
        self.cursor = restore_cursor(&self.rows, self.cursor, &fallbacks);
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
        if self.evolog.entries.is_empty() && self.slot_state(ContentSlot::View).needs_row() {
            rows.push(DisplayRow::Placeholder(ContentSlot::View));
        }
        self.rows = rows;
        let fallbacks = cursor_fallbacks(prev_cursor);
        self.cursor = restore_cursor(&self.rows, self.cursor, &fallbacks);
    }

    fn rebuild_dag_rows(&mut self) {
        // Remember what the cursor and the visual anchor were on, to find
        // them again.
        let prev_cursor = self.rows.get(self.cursor.raw()).copied();
        let visual_anchor = self.visual_anchor_row();

        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();
        for idx_raw in 0..self.dag.nodes.len() {
            let entry_idx = EntryIdx::new(idx_raw);
            self.dag.nodes[entry_idx].row = rows.len();
            self.push_dag_entry_rows(entry_idx, &mut rows);
        }
        if self.dag.nodes.is_empty() && self.slot_state(ContentSlot::View).needs_row() {
            rows.push(DisplayRow::Placeholder(ContentSlot::View));
        }
        self.rows = rows;

        // Restore cursor: try exact match, then fall back to parent file,
        // then parent commit. This handles fold scenarios where the cursor
        // was on a diff line that disappeared when the file was folded.
        let fallbacks = cursor_fallbacks(prev_cursor);
        self.cursor = restore_cursor(&self.rows, self.cursor, &fallbacks);
        self.reanchor_visual(visual_anchor);
    }

    /// Emit the display rows for one DAG entry (commit node, description,
    /// files, diffs/conflicts, graph links) into `rows`.
    fn push_dag_entry_rows(&self, entry_idx: EntryIdx, rows: &mut Vec<DisplayRow>) {
        rows.push(DisplayRow::CommitNode { entry_idx });

        if self.is_commit_unfolded(entry_idx) {
            // Description continuation lines (skip the first, already in CommitNode).
            if let Some(full) = &self.dag.nodes[entry_idx].commit.full_description {
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
        for line_idx_raw in 0..self.dag.nodes[entry_idx].graph.extra.len() {
            rows.push(DisplayRow::GraphLink {
                entry_idx,
                line_idx: GraphLineIdx::new(line_idx_raw),
            });
        }
    }

    /// Emit the rows of an owner's files: each file and, under an unfolded
    /// one, its diff lines or, for a conflicted DAG file, its conflict hunks.
    fn push_file_rows(&self, owner: FileOwner, rows: &mut Vec<DisplayRow>) {
        let Some(tree) = self.file_tree(owner) else {
            return;
        };
        let Some(files) = tree.files() else {
            if self.slot_state(ContentSlot::Files(owner)).needs_row() {
                rows.push(DisplayRow::Placeholder(ContentSlot::Files(owner)));
            }
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
            } else if self
                .slot_state(ContentSlot::Diff(owner, file_idx))
                .needs_row()
            {
                rows.push(DisplayRow::Placeholder(ContentSlot::Diff(owner, file_idx)));
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
                    let n = text.line_count();
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
                        for li in 0..text.line_count().max(1) {
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
                            term.text.line_count().max(1)
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
        let Some(node) = self.dag.nodes.get(entry_idx) else {
            return;
        };
        let start = node.row;
        let end = match self.dag.nodes.get(EntryIdx::new(entry_idx.raw() + 1)) {
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
        let visual_anchor = self.visual_anchor_row();

        let mut new_rows = Vec::with_capacity(end - start);
        self.push_dag_entry_rows(entry_idx, &mut new_rows);
        let new_end = start + new_rows.len();
        let delta = new_end as isize - end as isize;
        self.rows.splice(start..end, new_rows);

        if delta != 0 {
            // Shift the row pointers of all following entries.
            for idx_raw in (entry_idx.raw() + 1)..self.dag.nodes.len() {
                let node = &mut self.dag.nodes[EntryIdx::new(idx_raw)];
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
        self.reanchor_visual(visual_anchor);
    }
}

/// Cursor fallback chain for row rebuilds: the row itself, then what it
/// folds into, nearest first.
fn cursor_fallbacks(prev_cursor: Option<DisplayRow>) -> [Option<DisplayRow>; 5] {
    let Some(row) = prev_cursor else {
        return [None; 5];
    };
    let [a, b, c, d] = folds_into(row);
    [Some(row), a, b, c, d]
}

/// Where a row goes when it is folded away, nearest first: a diff line
/// into its file, a file into the row it hangs under, a base's lines into
/// the stub it folds to, and a conflict's rows into its header.
fn folds_into(row: DisplayRow) -> [Option<DisplayRow>; 4] {
    match row {
        DisplayRow::DiffLine {
            owner, file_idx, ..
        }
        | DisplayRow::Placeholder(ContentSlot::Diff(owner, file_idx)) => [
            Some(DisplayRow::FileChange { owner, file_idx }),
            Some(owner_row(owner)),
            None,
            None,
        ],
        DisplayRow::FileChange { owner, .. }
        | DisplayRow::Placeholder(ContentSlot::Files(owner)) => {
            [Some(owner_row(owner)), None, None, None]
        }
        DisplayRow::DescriptionLine { entry_idx, .. } | DisplayRow::GraphLink { entry_idx, .. } => {
            [Some(DisplayRow::CommitNode { entry_idx }), None, None, None]
        }
        DisplayRow::ConflictTerm {
            entry_idx,
            file_idx,
            hunk_idx,
            term_idx,
            line_idx,
        } if line_idx.raw() > 0 => {
            let [header, file, commit] = hunk_folds_into(entry_idx, file_idx, hunk_idx);
            let stub = DisplayRow::ConflictTerm {
                entry_idx,
                file_idx,
                hunk_idx,
                term_idx,
                line_idx: ConflictLineIdx::new(0),
            };
            [Some(stub), header, file, commit]
        }
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
        } => {
            let [header, file, commit] = hunk_folds_into(entry_idx, file_idx, hunk_idx);
            [header, file, commit, None]
        }
        DisplayRow::ConflictHeader {
            entry_idx,
            file_idx,
            ..
        } => [
            Some(DisplayRow::FileChange {
                owner: FileOwner::Dag(entry_idx),
                file_idx,
            }),
            Some(DisplayRow::CommitNode { entry_idx }),
            None,
            None,
        ],
        _ => [None; 4],
    }
}

/// Where the rows of a conflict hunk go when folded away: its header, its
/// file, its commit.
fn hunk_folds_into(
    entry_idx: EntryIdx,
    file_idx: FileIdx,
    hunk_idx: ConflictHunkIdx,
) -> [Option<DisplayRow>; 3] {
    [
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
    ]
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
        let children = self
            .rows
            .get(self.cursor.raw() + 1..)
            .unwrap_or_default()
            .iter()
            .take_while(|row| {
                !matches!(
                    row,
                    DisplayRow::CommitNode { .. }
                        | DisplayRow::GraphLink { .. }
                        | DisplayRow::EvoLogItem { .. }
                        | DisplayRow::EvoLogGraphLink { .. }
                        | DisplayRow::OpLogItem { .. }
                        | DisplayRow::OpLogGraphLink { .. }
                        | DisplayRow::OpLogLoadMore
                        | DisplayRow::AnnotateLine { .. }
                )
            })
            .count();
        let last_child = self.cursor.raw() + children;
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
        // On content that failed to load, the fold key asks for it again.
        if let Some(DisplayRow::Placeholder(slot)) = self.rows.get(self.cursor.raw()).copied()
            && matches!(self.slot_state(slot), SlotState::Failed(_))
        {
            self.retry_slot(slot);
            return;
        }
        match self.rows.get(self.cursor.raw()) {
            Some(DisplayRow::CommitNode { entry_idx }) => {
                self.toggle_commit_fold(*entry_idx);
            }
            // Folding on a diff line folds the parent file.
            Some(
                DisplayRow::FileChange { owner, file_idx }
                | DisplayRow::DiffLine {
                    owner, file_idx, ..
                }
                | DisplayRow::Placeholder(ContentSlot::Diff(owner, file_idx)),
            ) => {
                self.toggle_file_fold(*owner, *file_idx);
            }
            // Folding while the files load folds what they belong to.
            Some(DisplayRow::Placeholder(ContentSlot::Files(owner))) => match *owner {
                FileOwner::Dag(entry_idx) => self.toggle_commit_fold(entry_idx),
                FileOwner::EvoLog(evolog_idx) => self.toggle_evolog_fold(evolog_idx),
                FileOwner::Interdiff => {}
            },
            Some(DisplayRow::Placeholder(ContentSlot::Operation(op_log_idx))) => {
                self.toggle_op_fold(*op_log_idx);
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
        let commit_id = self.commit_id(entry_idx).clone();
        let unfolded = toggle_membership(&mut self.dag.unfolded_commits, commit_id);
        if unfolded {
            let request = self.dag.nodes[entry_idx].files.request_summary();
            self.pending_repo_requests.extend(request);
        }
        self.rebuild_entry_rows(entry_idx);
        if unfolded {
            self.scroll_to_show_children();
        }
    }

    /// Expand a trimmed resolved section to its full content (tab on the
    /// gap row), leaving the cursor on the first line the gap hid, which
    /// takes the gap's place.
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
        let Some(trim) = self.hunk_trimmed_context(hunk) else {
            return;
        };
        self.update_hunk_ui(hunk, |s| s.expanded = true);
        self.rebuild_entry_rows(entry_idx);
        self.return_to_row(Some(DisplayRow::ConflictContext {
            entry_idx,
            file_idx,
            hunk_idx,
            line_idx: ConflictLineIdx::new(trim.head),
        }));
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
                && *self.commit_id(entry_idx) == vr.commit_id
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
            let request = self.dag.nodes[entry_idx].request_conflict_hunks(file_idx);
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
        } else if self.slot_state(ContentSlot::View).needs_row() {
            self.rows.push(DisplayRow::Placeholder(ContentSlot::View));
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

#[cfg(test)]
mod placeholder_tests {
    use super::super::test_support::{entry, load};
    use super::*;
    use crate::dag::{DiffSummary, FileChange, LineStats};
    use crate::repo_service::{RepoResult, RevsetLoadKind};

    const COMMIT: &str = "7bbaa2cb1f0e4d3a9c8b7a6e5d4c3b2a19087654";
    const ENTRY: EntryIdx = EntryIdx::new(0);

    fn app_with_commit() -> App {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', COMMIT)], true);
        app
    }

    fn summary_arrives(app: &mut App) {
        app.handle_repo_result(RepoResult::DiffSummary {
            target: DiffTarget::Commit(crate::types::CommitId::new(COMMIT)),
            result: Ok(DiffSummary {
                files: vec![FileChange::for_test("a.rs")],
                stats: LineStats::default(),
            }),
        });
    }

    fn loading_rows(app: &App) -> Vec<ContentSlot> {
        app.rows
            .iter()
            .filter_map(|row| match row {
                DisplayRow::Placeholder(loading) => Some(*loading),
                _ => None,
            })
            .collect()
    }

    /// An unfolded commit shows where its files will be, until they come.
    #[test]
    fn unfolded_files_hold_a_place_until_loaded() {
        let mut app = app_with_commit();
        app.toggle_commit_fold(ENTRY);
        assert_eq!(
            loading_rows(&app),
            [ContentSlot::Files(FileOwner::Dag(ENTRY))]
        );
        summary_arrives(&mut app);
        assert!(loading_rows(&app).is_empty());
        assert!(app.rows.contains(&DisplayRow::FileChange {
            owner: FileOwner::Dag(ENTRY),
            file_idx: FileIdx::new(0),
        }));
    }

    #[test]
    fn an_unfolded_file_holds_a_place_for_its_diff() {
        let mut app = app_with_commit();
        app.toggle_commit_fold(ENTRY);
        summary_arrives(&mut app);
        app.toggle_file_fold(FileOwner::Dag(ENTRY), FileIdx::new(0));
        assert_eq!(
            loading_rows(&app),
            [ContentSlot::Diff(FileOwner::Dag(ENTRY), FileIdx::new(0))]
        );
    }

    /// A cursor left on a placeholder lands on what it belonged to.
    #[test]
    fn a_cursor_on_a_placeholder_falls_back_to_its_owner() {
        let mut app = app_with_commit();
        app.toggle_commit_fold(ENTRY);
        let placeholder = app
            .rows
            .iter()
            .position(|row| matches!(row, DisplayRow::Placeholder(_)))
            .expect("a placeholder");
        app.cursor = RowIdx::new(placeholder);
        // The placeholder answers for its commit meanwhile.
        assert_eq!(app.rows[placeholder].entry_idx(), Some(ENTRY));
        summary_arrives(&mut app);
        assert!(app.rows[app.cursor.raw()] == DisplayRow::CommitNode { entry_idx: ENTRY });
    }

    /// Before the first commits arrive, the log says it is loading them.
    #[test]
    fn an_empty_log_holds_a_place_while_loading() {
        let mut app = App::for_test();
        app.request_revset_load(None, RevsetLoadKind::Snapshot);
        assert!(app.rows == [DisplayRow::Placeholder(ContentSlot::View)]);
        load(&mut app, vec![entry('a', COMMIT)], true);
        assert!(loading_rows(&app).is_empty());
    }

    /// Folding on the placeholder folds the commit it is loading for.
    #[test]
    fn folding_on_a_placeholder_folds_its_owner() {
        let mut app = app_with_commit();
        app.toggle_commit_fold(ENTRY);
        app.cursor = RowIdx::new(
            app.rows
                .iter()
                .position(|row| matches!(row, DisplayRow::Placeholder(_)))
                .expect("a placeholder"),
        );
        app.toggle_fold();
        assert!(!app.is_commit_unfolded(ENTRY));
        assert!(loading_rows(&app).is_empty());
    }

    fn failure() -> crate::repo_service::RepoError {
        crate::repo_service::RepoError::new(crate::repo_service::RepoErrorKind::Operation, "boom")
    }

    /// A file list that failed keeps its row, which says so; the fold key
    /// on it asks again, and the same row reads as loading.
    #[test]
    fn failed_files_hold_their_place_and_retry_on_fold() {
        let mut app = app_with_commit();
        app.toggle_commit_fold(ENTRY);
        app.take_repo_requests();
        app.handle_repo_result(RepoResult::DiffSummary {
            target: DiffTarget::Commit(crate::types::CommitId::new(COMMIT)),
            result: Err(failure()),
        });
        let slot = ContentSlot::Files(FileOwner::Dag(ENTRY));
        assert_eq!(loading_rows(&app), [slot]);
        assert!(matches!(app.slot_state(slot), SlotState::Failed(e) if e.message == "boom"));

        app.cursor = RowIdx::new(
            app.rows
                .iter()
                .position(|row| *row == DisplayRow::Placeholder(slot))
                .expect("the placeholder"),
        );
        app.toggle_fold();
        assert!(matches!(app.slot_state(slot), SlotState::Loading));
        assert!(matches!(
            app.take_repo_requests().as_slice(),
            [RepoRequest::DiffSummary { .. }]
        ));
        // Still unfolded, and the cursor still on the placeholder.
        assert!(app.is_commit_unfolded(ENTRY));
        assert!(app.rows[app.cursor.raw()] == DisplayRow::Placeholder(slot));
    }

    /// The op log's place is held while it first loads, and, if it fails,
    /// by the failure rather than an empty view; the fold key there asks
    /// again.
    #[test]
    fn the_op_log_holds_its_place_while_loading_and_after_failing() {
        let mut app = App::for_test();
        app.switch_view(ActiveView::Operations);
        assert!(app.rows == [DisplayRow::Placeholder(ContentSlot::View)]);
        assert!(matches!(
            app.slot_state(ContentSlot::View),
            SlotState::Loading
        ));
        app.take_repo_requests();
        app.handle_repo_result(RepoResult::Operations {
            limit: app.op_log.limit,
            result: Err(failure()),
        });
        assert!(app.rows == [DisplayRow::Placeholder(ContentSlot::View)]);
        assert!(matches!(
            app.slot_state(ContentSlot::View),
            SlotState::Failed(_)
        ));
        app.cursor = RowIdx::new(0);
        app.toggle_fold();
        assert!(matches!(
            app.take_repo_requests().as_slice(),
            [RepoRequest::Operations { .. }]
        ));
    }
}

#[cfg(test)]
mod cursor_fallback_tests {
    use super::super::FileFoldKey;
    use super::super::test_support::{entry, load};
    use super::*;
    use crate::conflict::{ConflictHunkKind, ConflictTerm, ConflictTermKind, ConflictText};
    use crate::dag::{DiffLine, DiffLineKind, DiffResult, DiffSummary, FileChange, LineStats};
    use crate::types::{CommitId, RepoPath};

    const ENTRY: EntryIdx = EntryIdx::new(0);
    const CONFLICTED: FileIdx = FileIdx::new(1);
    const HUNK: ConflictHunkIdx = ConflictHunkIdx::new(1);
    /// Where the base lands among the conflict's terms, which are stored
    /// side, base, side.
    const BASE: ConflictTermIdx = ConflictTermIdx::new(1);
    const OURS: ConflictTermIdx = ConflictTermIdx::new(0);

    fn term(kind: ConflictTermKind, text: &str) -> ConflictTerm {
        ConflictTerm {
            kind,
            absent: false,
            text: ConflictText::from_bytes(text.as_bytes()),
            token_lines: Vec::new(),
        }
    }

    /// A header, then one removed and one added line.
    fn diff() -> DiffResult {
        let line = |kind, old_line, new_line| DiffLine {
            kind,
            content: "x".into(),
            tokens: vec![],
            old_line,
            new_line,
            conflict_region: false,
        };
        let lines = vec![
            line(DiffLineKind::Header, None, None),
            line(DiffLineKind::Removed, Some(1), None),
            line(DiffLineKind::Added, None, Some(1)),
        ];
        DiffResult {
            git: lines.clone(),
            color_words: lines,
        }
    }

    /// Commit `a` unfolded to two unfolded files: `diffed`, with a two-line
    /// diff, and `conflicted`, a conflict of one-line sides over a two-line
    /// base between ten resolved lines either side, all but the three next
    /// to the conflict behind a gap.
    fn app() -> App {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1")], true);
        let mut conflicted = FileChange::for_test("conflicted");
        conflicted.has_conflict = true;
        let node = &mut app.dag.nodes[ENTRY];
        node.files.set_summary(Ok(DiffSummary {
            files: vec![FileChange::for_test("diffed"), conflicted],
            stats: LineStats::default(),
        }));
        node.files.set_diff(&RepoPath::new("diffed"), Ok(diff()));
        node.set_conflict_hunks(
            CONFLICTED,
            Loadable::Loaded(vec![
                ConflictHunkKind::Resolved {
                    text: ConflictText::from_bytes("x\n".repeat(10).as_bytes()),
                },
                ConflictHunkKind::Conflict {
                    terms: vec![
                        term(ConflictTermKind::Side(0), "ours\n"),
                        term(ConflictTermKind::Base(0), "base 1\nbase 2\n"),
                        term(ConflictTermKind::Side(1), "theirs\n"),
                    ],
                },
                ConflictHunkKind::Resolved {
                    text: ConflictText::from_bytes("x\n".repeat(10).as_bytes()),
                },
            ]),
        );
        app.dag.unfolded_commits.insert(CommitId::new("a1"));
        for path in ["diffed", "conflicted"] {
            app.dag.unfolded_files.insert(FileFoldKey {
                commit_id: CommitId::new("a1"),
                path: RepoPath::new(path),
            });
        }
        app.rebuild_rows();
        app
    }

    fn header_row() -> DisplayRow {
        DisplayRow::ConflictHeader {
            entry_idx: ENTRY,
            file_idx: CONFLICTED,
            hunk_idx: HUNK,
        }
    }

    fn term_row(term_idx: ConflictTermIdx, line: usize) -> DisplayRow {
        DisplayRow::ConflictTerm {
            entry_idx: ENTRY,
            file_idx: CONFLICTED,
            hunk_idx: HUNK,
            term_idx,
            line_idx: ConflictLineIdx::new(line),
        }
    }

    fn put_cursor_on(app: &mut App, row: DisplayRow) {
        app.cursor = app.position_of(row).expect("row is displayed");
    }

    /// Picking a side rebuilds the rows without changing them; the cursor
    /// stays on the row it picked from rather than climbing to its hunk or
    /// file.
    #[test]
    fn picking_a_side_leaves_the_cursor_where_it_was() {
        for row in [header_row(), term_row(OURS, 0)] {
            let mut app = app();
            put_cursor_on(&mut app, row);

            app.pick_conflict_term(
                crate::types::ConflictHunkRef {
                    entry_idx: ENTRY,
                    file_idx: CONFLICTED,
                    hunk_idx: HUNK,
                },
                ConflictTermKind::Side(1),
            );

            assert!(app.cursor_row() == Some(row));
        }
    }

    /// Folding a base from any of its lines leaves the cursor on the stub
    /// it folds to, where the same key unfolds it again.
    #[test]
    fn folding_a_base_lands_on_its_stub_and_unfolds_from_there() {
        let mut app = app();
        put_cursor_on(&mut app, term_row(BASE, 1));

        app.toggle_fold();
        assert!(app.cursor_row() == Some(term_row(BASE, 0)));
        assert!(app.position_of(term_row(BASE, 1)).is_none());

        app.toggle_fold();
        assert!(app.position_of(term_row(BASE, 1)).is_some());
        assert!(app.is_file_unfolded(FileOwner::Dag(ENTRY), CONFLICTED));
    }

    /// Expanding a gap puts the cursor on the first line it hid, in the
    /// gap's place, rather than back up on the file. After the conflict
    /// that is the fourth line, the three before it having been shown.
    #[test]
    fn expanding_a_gap_lands_on_the_first_line_it_hid() {
        let mut app = app();
        let gap = DisplayRow::ConflictGap {
            entry_idx: ENTRY,
            file_idx: CONFLICTED,
            hunk_idx: ConflictHunkIdx::new(2),
        };
        put_cursor_on(&mut app, gap);
        let at = app.cursor;

        app.toggle_fold();

        assert!(app.position_of(gap).is_none());
        assert_eq!(app.cursor, at);
        assert!(matches!(
            app.cursor_row(),
            Some(DisplayRow::ConflictContext { line_idx, .. }) if line_idx.raw() == 3
        ));
    }
}

#[cfg(test)]
mod op_log_row_tests {
    use super::*;
    use crate::history::OpLogEntry;
    use crate::repo_service::RepoResult;

    /// The op log on screen, given `ops` and whether there are more.
    fn deliver(app: &mut App, ops: Vec<OpLogEntry>, has_more: bool) {
        app.handle_repo_result(RepoResult::Operations {
            limit: app.op_log.limit,
            result: Ok((ops, has_more)),
        });
    }

    fn op_at_cursor(app: &App) -> Option<&str> {
        match app.cursor_row()? {
            DisplayRow::OpLogItem { op_log_idx } => {
                Some(app.op_log.entries[op_log_idx.raw()].id.as_str())
            }
            _ => None,
        }
    }

    /// The graph of a cut-off log ends in link rows for the edges it cuts,
    /// which the longer log draws differently (here `o1`'s two parents,
    /// side by side, then joined), so "load more" can't keep the cursor's
    /// old position: it lands on the first entry the load brought.
    #[test]
    fn loading_more_lands_on_the_first_entry_it_brought() {
        let ops = || {
            vec![
                OpLogEntry::for_test("o0", &["o1"], None),
                OpLogEntry::for_test("o1", &["o2", "o3"], None),
                OpLogEntry::for_test("o2", &["o4"], None),
                OpLogEntry::for_test("o3", &["o4"], None),
                OpLogEntry::for_test("o4", &[], None),
            ]
        };
        let mut app = App::for_test();
        app.switch_view(ActiveView::Operations);
        deliver(&mut app, ops().into_iter().take(2).collect(), true);
        app.cursor = app.position_of(DisplayRow::OpLogLoadMore).unwrap();

        app.toggle_fold();
        deliver(&mut app, ops(), false);

        assert_eq!(op_at_cursor(&app), Some("o2"));
    }
}
