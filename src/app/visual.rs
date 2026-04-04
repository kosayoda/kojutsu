use super::{App, PersistentVisualRange, VisualMode};
use crate::dag::DiffLineKind;
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
use crate::types::{
    ChangeId, DisplayRow, FileRef, RepoPath, Selection, SelectionKind, VisualRange,
};

/// A single line within a visual selection range (used during toggle).
struct LineSelection {
    change_id: ChangeId,
    path: RepoPath,
    old_line: Option<u32>,
    new_line: Option<u32>,
}

impl App {
    // -----------------------------------------------------------------------
    // Shared lifecycle
    // -----------------------------------------------------------------------

    /// Whether any visual mode is active (line or commit).
    pub fn in_visual_mode(&self) -> bool {
        self.visual.is_some()
    }

    /// Enter visual mode based on the current cursor row type.
    /// CommitNode → commit visual mode, DiffLine → line visual mode.
    pub fn enter_visual_mode(&mut self) {
        match self.rows.get(self.cursor) {
            Some(DisplayRow::CommitNode { entry_idx }) => {
                let entry_idx = *entry_idx;
                self.visual = Some(VisualMode::Commits {
                    anchor: entry_idx,
                    path: vec![entry_idx],
                });
                self.visual_persistent = None;
            }
            Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            }) => {
                let Some(diff_lines) = self.diff_lines(*entry_idx, *file_idx) else {
                    return;
                };
                let dl = &diff_lines[line_idx.raw()];
                if dl.kind.is_selectable() {
                    self.visual = Some(VisualMode::Lines {
                        anchor: self.cursor,
                    });
                    self.visual_persistent = None;
                }
            }
            _ => {}
        }
    }

    /// Exit visual mode with `v`: persist the range but don't create selections.
    pub fn exit_visual_mode(&mut self) {
        match self.visual.take() {
            Some(VisualMode::Lines { anchor }) => {
                self.visual_persistent = self.compute_line_range(anchor);
            }
            Some(VisualMode::Commits { path, .. }) => {
                if !path.is_empty() {
                    self.visual_persistent = Some(PersistentVisualRange::Commits(path));
                }
            }
            None => {}
        }
    }

    /// Cancel visual mode without persisting (Esc or other action).
    pub fn cancel_visual_mode(&mut self) {
        self.visual = None;
    }

    /// Space in visual mode: convert range to explicit selections and exit.
    pub fn persist_visual_selection(&mut self) {
        match self.visual.take() {
            Some(VisualMode::Lines { anchor }) => {
                self.visual_persistent = self.compute_line_range(anchor);
                self.toggle_line_visual_selection();
            }
            Some(VisualMode::Commits { path, .. }) => {
                self.visual_persistent = Some(PersistentVisualRange::Commits(path));
                self.toggle_commit_visual_selection();
            }
            None => {}
        }
    }

    /// Check if the cursor is in a persistent visual range (for space toggle).
    pub fn cursor_in_persistent_visual_range(&self) -> bool {
        match &self.visual_persistent {
            Some(PersistentVisualRange::Lines(vr)) => {
                if let Some(DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    line_idx,
                }) = self.rows.get(self.cursor)
                {
                    let cid = self.entries[*entry_idx].commit.unique_change_id();
                    if let Some(file) = self
                        .files_for_entry(*entry_idx)
                        .and_then(|f| f.get(file_idx.raw()))
                    {
                        return cid == vr.change_id
                            && file.path == vr.path
                            && *line_idx >= vr.start_line
                            && *line_idx <= vr.end_line;
                    }
                }
                false
            }
            Some(PersistentVisualRange::Commits(range)) => match self.rows.get(self.cursor) {
                Some(DisplayRow::CommitNode { entry_idx }) => range.contains(entry_idx),
                _ => false,
            },
            None => false,
        }
    }

    /// Space on a persistent visual range: toggle into/out of explicit selections.
    pub fn toggle_persistent_visual_selection(&mut self) {
        match &self.visual_persistent {
            Some(PersistentVisualRange::Lines(_)) => {
                self.toggle_line_visual_selection();
            }
            Some(PersistentVisualRange::Commits(_)) => {
                self.toggle_commit_visual_selection();
            }
            None => {}
        }
    }

    // -----------------------------------------------------------------------
    // Movement (dispatches based on variant)
    // -----------------------------------------------------------------------

    pub fn visual_move_down(&mut self) {
        match &self.visual {
            Some(VisualMode::Lines { .. }) => self.line_visual_move_down(),
            Some(VisualMode::Commits { .. }) => self.commit_visual_move_down(),
            None => {}
        }
    }

    pub fn visual_move_up(&mut self) {
        match &self.visual {
            Some(VisualMode::Lines { .. }) => self.line_visual_move_up(),
            Some(VisualMode::Commits { .. }) => self.commit_visual_move_up(),
            None => {}
        }
    }

    // -----------------------------------------------------------------------
    // Range queries (for rendering)
    // -----------------------------------------------------------------------

    /// Check if a diff line is in the visual range (active or persistent).
    pub fn is_in_visual_range(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) -> bool {
        // Check active line visual range.
        if let Some(VisualMode::Lines { anchor }) = &self.visual {
            let lo = (*anchor).min(self.cursor);
            let hi = (*anchor).max(self.cursor);
            if let Some(row_idx) = self.rows.iter().position(|r| {
                matches!(r, DisplayRow::DiffLine { entry_idx: e, file_idx: f, line_idx: l }
                    if *e == entry_idx && *f == file_idx && *l == line_idx)
            }) {
                return row_idx >= lo && row_idx <= hi;
            }
        }

        // Check persistent line range.
        if let Some(PersistentVisualRange::Lines(vr)) = &self.visual_persistent {
            let cid = self.entries[entry_idx].commit.unique_change_id();
            if let Some(files) = self.files_for_entry(entry_idx) {
                if let Some(file) = files.get(file_idx.raw()) {
                    if cid == vr.change_id
                        && file.path == vr.path
                        && line_idx >= vr.start_line
                        && line_idx <= vr.end_line
                    {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Check if a commit is in the visual range (active or persistent).
    pub fn is_in_visual_commit_range(&self, entry_idx: EntryIdx) -> bool {
        if let Some(VisualMode::Commits { path, .. }) = &self.visual {
            if path.contains(&entry_idx) {
                return true;
            }
        }
        if let Some(PersistentVisualRange::Commits(range)) = &self.visual_persistent {
            if range.contains(&entry_idx) {
                return true;
            }
        }
        false
    }

    // -----------------------------------------------------------------------
    // Line visual mode internals
    // -----------------------------------------------------------------------

    fn visual_line_file(&self) -> Option<(EntryIdx, FileIdx)> {
        let Some(VisualMode::Lines { anchor }) = &self.visual else {
            return None;
        };
        match self.rows.get(*anchor) {
            Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                ..
            }) => Some((*entry_idx, *file_idx)),
            _ => None,
        }
    }

    fn line_visual_move_down(&mut self) {
        let Some((anchor_entry, anchor_file)) = self.visual_line_file() else {
            return;
        };
        for j in (self.cursor + 1)..self.rows.len() {
            match &self.rows[j] {
                DisplayRow::GraphLink { .. } => continue,
                DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    line_idx,
                } if *entry_idx == anchor_entry && *file_idx == anchor_file => {
                    // Skip context lines.
                    if self
                        .diff_lines(*entry_idx, *file_idx)
                        .and_then(|lines| lines.get(line_idx.raw()))
                        .is_some_and(|dl| dl.kind == DiffLineKind::Context)
                    {
                        continue;
                    }
                    self.cursor = j;
                    return;
                }
                _ => return,
            }
        }
    }

    fn line_visual_move_up(&mut self) {
        let Some((anchor_entry, anchor_file)) = self.visual_line_file() else {
            return;
        };
        for j in (0..self.cursor).rev() {
            match &self.rows[j] {
                DisplayRow::GraphLink { .. } => continue,
                DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    line_idx,
                } if *entry_idx == anchor_entry && *file_idx == anchor_file => {
                    // Skip context lines.
                    if self
                        .diff_lines(*entry_idx, *file_idx)
                        .and_then(|lines| lines.get(line_idx.raw()))
                        .is_some_and(|dl| dl.kind == DiffLineKind::Context)
                    {
                        continue;
                    }
                    self.cursor = j;
                    return;
                }
                _ => return,
            }
        }
    }

    /// Compute the persistent line range from the given anchor and current cursor.
    fn compute_line_range(&self, anchor: usize) -> Option<PersistentVisualRange> {
        let lo = anchor.min(self.cursor);
        let hi = anchor.max(self.cursor);

        let mut start_line = None;
        let mut end_line = None;
        let mut change_id = None;
        let mut path = None;

        for idx in lo..=hi {
            if let Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            }) = self.rows.get(idx)
            {
                if start_line.is_none() {
                    start_line = Some(*line_idx);
                    change_id = Some(self.entries[*entry_idx].commit.unique_change_id());
                    path = self
                        .files_for_entry(*entry_idx)
                        .and_then(|files| files.get(file_idx.raw()))
                        .map(|file| file.path.clone());
                }
                end_line = Some(*line_idx);
            }
        }

        match (start_line, end_line, change_id, path) {
            (Some(start), Some(end), Some(change_id), Some(p)) => {
                Some(PersistentVisualRange::Lines(VisualRange {
                    change_id,
                    path: p,
                    start_line: start,
                    end_line: end,
                }))
            }
            _ => None,
        }
    }

    /// Toggle all selectable lines in a persistent visual line range.
    fn toggle_line_visual_selection(&mut self) {
        let Some(PersistentVisualRange::Lines(vr)) = &self.visual_persistent else {
            return;
        };
        let vr = vr.clone();

        let line_data: Vec<LineSelection> = self
            .diff_states
            .iter()
            .filter_map(|(cache_key, diff_lines)| {
                let diff_lines = diff_lines.loaded()?;
                let cid = self.change_id_for_commit_key(&cache_key.commit_id)?;
                if cid != vr.change_id || cache_key.path != vr.path {
                    return None;
                }
                let lines: Vec<_> = diff_lines
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| {
                        let idx = DiffLineIdx::new(*i);
                        idx >= vr.start_line && idx <= vr.end_line
                    })
                    .filter(|(_, dl)| dl.kind.is_selectable())
                    .map(|(_, dl)| LineSelection {
                        change_id: vr.change_id.clone(),
                        path: vr.path.clone(),
                        old_line: dl.old_line,
                        new_line: dl.new_line,
                    })
                    .collect();
                Some(lines)
            })
            .flatten()
            .collect();

        if line_data.is_empty() {
            return;
        }

        let all_selected = line_data.iter().all(|ls| {
            self.selection.contains(&Selection::Line {
                file_ref: FileRef {
                    change_id: ls.change_id.clone(),
                    path: ls.path.clone(),
                },
                old_line: ls.old_line,
                new_line: ls.new_line,
            })
        });

        if all_selected {
            for ls in &line_data {
                self.selection.remove(&Selection::Line {
                    file_ref: FileRef {
                        change_id: ls.change_id.clone(),
                        path: ls.path.clone(),
                    },
                    old_line: ls.old_line,
                    new_line: ls.new_line,
                });
            }
        } else {
            if let Some(ls) = line_data.first() {
                self.clear_other_commits(&ls.change_id);
            }
            self.selection.ensure_kind(SelectionKind::Line);
            for ls in line_data {
                self.selection.remove(&Selection::File(FileRef {
                    change_id: ls.change_id.clone(),
                    path: ls.path.clone(),
                }));
                self.selection.insert(
                    SelectionKind::Line,
                    Selection::Line {
                        file_ref: FileRef {
                            change_id: ls.change_id,
                            path: ls.path,
                        },
                        old_line: ls.old_line,
                        new_line: ls.new_line,
                    },
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Commit visual mode internals
    // -----------------------------------------------------------------------

    fn commit_visual_move_down(&mut self) {
        let Some(VisualMode::Commits { anchor, path }) = &self.visual else {
            return;
        };
        let anchor = *anchor;

        // If anchor is at the bottom (last), cursor is at top — shrink from top.
        if path.len() > 1 && path.last() == Some(&anchor) {
            let Some(VisualMode::Commits { path, .. }) = &mut self.visual else {
                return;
            };
            path.remove(0);
            let target = path[0];
            self.jump_cursor_to_commit(target);
            return;
        }

        // Otherwise extend at the bottom: next entry in display order.
        let tail = match &self.visual {
            Some(VisualMode::Commits { path, .. }) => path.last().copied(),
            _ => None,
        };
        let Some(tail) = tail else { return };
        if let Some(parent) = self.next_entry_down(tail) {
            let Some(VisualMode::Commits { path, .. }) = &mut self.visual else {
                return;
            };
            path.push(parent);
            self.jump_cursor_to_commit(parent);
        }
    }

    fn commit_visual_move_up(&mut self) {
        let Some(VisualMode::Commits { anchor, path }) = &self.visual else {
            return;
        };
        let anchor = *anchor;

        // If anchor is at the top (first), cursor is at bottom — shrink from bottom.
        if path.len() > 1 && path.first() == Some(&anchor) {
            let Some(VisualMode::Commits { path, .. }) = &mut self.visual else {
                return;
            };
            path.pop();
            let target = *path.last().expect("path has >1 element after length check");
            self.jump_cursor_to_commit(target);
            return;
        }

        // Otherwise extend at the top: next entry in display order (upward).
        let head = match &self.visual {
            Some(VisualMode::Commits { path, .. }) => path.first().copied(),
            _ => None,
        };
        let Some(head) = head else { return };
        if let Some(child) = self.next_entry_up(head) {
            let Some(VisualMode::Commits { path, .. }) = &mut self.visual else {
                return;
            };
            path.insert(0, child);
            self.jump_cursor_to_commit(child);
        }
    }

    /// Toggle all commits in the persistent visual commit range as explicit selections.
    fn toggle_commit_visual_selection(&mut self) {
        let Some(PersistentVisualRange::Commits(range)) = &self.visual_persistent else {
            return;
        };

        self.selection.ensure_kind(SelectionKind::Commit);
        let all_selected = range.iter().all(|idx| {
            let cid = self.entries[*idx].commit.unique_change_id();
            self.selection.contains(&Selection::Commit(cid))
        });

        // Clone the range to avoid borrowing self.visual_persistent while mutating selection.
        let range: Vec<EntryIdx> = range.clone();
        if all_selected {
            for idx in &range {
                let cid = self.entries[*idx].commit.unique_change_id();
                self.selection.remove(&Selection::Commit(cid));
            }
        } else {
            for idx in &range {
                let cid = self.entries[*idx].commit.unique_change_id();
                self.selection
                    .insert(SelectionKind::Commit, Selection::Commit(cid));
            }
        }
    }

    /// The next entry in display order (downward = toward parents).
    fn next_entry_down(&self, entry_idx: EntryIdx) -> Option<EntryIdx> {
        let next = entry_idx.raw() + 1;
        if next < self.entries.len() {
            Some(EntryIdx::new(next))
        } else {
            None
        }
    }

    /// The next entry in display order (upward = toward children).
    fn next_entry_up(&self, entry_idx: EntryIdx) -> Option<EntryIdx> {
        let raw = entry_idx.raw();
        if raw > 0 {
            Some(EntryIdx::new(raw - 1))
        } else {
            None
        }
    }

    fn jump_cursor_to_commit(&mut self, entry_idx: EntryIdx) {
        if let Some(pos) = self
            .rows
            .iter()
            .position(|r| matches!(r, DisplayRow::CommitNode { entry_idx: e } if *e == entry_idx))
        {
            self.cursor = pos;
        }
    }
}
