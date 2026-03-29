use super::App;
use crate::dag::DiffLineKind;
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
use crate::types::{
    ChangeId, DisplayRow, FileRef, Selection, SelectionKind, VisualRange,
};

impl App {
    /// Toggle visual mode.
    ///
    /// - If not in visual mode: start visual selection on current line (must be
    ///   an Added/Removed diff line). Clears any persistent visual range.
    /// - If in visual mode: exit and persist the current range as a `VisualRange`.
    pub fn toggle_visual_mode(&mut self) {
        if self.visual_anchor.is_some() {
            // Exit visual mode → persist the range.
            self.persist_visual_range();
            self.visual_anchor = None;
        } else if let Some(DisplayRow::DiffLine {
            entry_idx,
            file_idx,
            line_idx,
        }) = self.rows.get(self.cursor)
        {
            let Some(diff_lines) = self.diff_lines(*entry_idx, *file_idx) else {
                return;
            };
            let dl = &diff_lines[line_idx.raw()];
            if dl.kind == DiffLineKind::Added || dl.kind == DiffLineKind::Removed {
                self.visual_range = None; // clear any old persistent range
                self.visual_anchor = Some(self.cursor);
            }
        }
    }

    /// Exit visual mode, discarding the range (no persistence).
    pub fn cancel_visual_mode(&mut self) {
        self.visual_anchor = None;
    }

    /// Whether visual mode is actively selecting (anchor set).
    pub fn in_visual_mode(&self) -> bool {
        self.visual_anchor.is_some()
    }

    /// Get the active visual range as (lo, hi) row indices (inclusive).
    /// Only valid while `in_visual_mode()` is true.
    fn active_visual_row_range(&self) -> Option<(usize, usize)> {
        self.visual_anchor
            .map(|anchor| (anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// Get the (entry_idx, file_idx) of the visual mode anchor.
    fn visual_file(&self) -> Option<(EntryIdx, FileIdx)> {
        let anchor = self.visual_anchor?;
        match self.rows.get(anchor) {
            Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                ..
            }) => Some((*entry_idx, *file_idx)),
            _ => None,
        }
    }

    /// Persist the current active visual range as a `VisualRange`.
    fn persist_visual_range(&mut self) {
        let Some((lo, hi)) = self.active_visual_row_range() else {
            return;
        };

        // Find the DiffLineIdx bounds from the row range.
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
                    change_id = Some(self.entries[*entry_idx].commit.change_id.change_id());
                    path = self
                        .files_for_entry(*entry_idx)
                        .and_then(|files| files.get(file_idx.raw()))
                        .map(|file| file.path.clone());
                }
                end_line = Some(*line_idx);
            }
        }

        if let (Some(start), Some(end), Some(change_id), Some(p)) =
            (start_line, end_line, change_id, path)
        {
            self.visual_range = Some(VisualRange {
                change_id,
                path: p,
                start_line: start,
                end_line: end,
            });
        }
    }

    /// Check if a diff line is within the visual range (active or persistent).
    pub fn is_in_visual_range(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) -> bool {
        // Check active visual range first (while selecting).
        if let Some((lo, hi)) = self.active_visual_row_range() {
            // Check by row index (the cursor range).
            if let Some(row_idx) = self.rows.iter().position(|r| {
                matches!(r, DisplayRow::DiffLine { entry_idx: e, file_idx: f, line_idx: l }
                    if *e == entry_idx && *f == file_idx && *l == line_idx)
            }) {
                return row_idx >= lo && row_idx <= hi;
            }
        }

        // Check persistent visual range.
        if let Some(vr) = &self.visual_range {
            let cid = self.entries[entry_idx].commit.change_id.change_id();
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

    /// Check if the cursor is on a line within the persistent visual range.
    pub fn cursor_in_persistent_visual_range(&self) -> bool {
        let Some(vr) = &self.visual_range else {
            return false;
        };
        if let Some(DisplayRow::DiffLine {
            entry_idx,
            file_idx,
            line_idx,
        }) = self.rows.get(self.cursor)
        {
            let cid = self.entries[*entry_idx].commit.change_id.change_id();
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

    /// Move cursor down, constrained to the same file's diff lines (visual mode).
    pub fn visual_move_down(&mut self) {
        let Some((anchor_entry, anchor_file)) = self.visual_file() else {
            return;
        };
        for j in (self.cursor + 1)..self.rows.len() {
            if matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                continue;
            }
            match &self.rows[j] {
                DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    ..
                } if *entry_idx == anchor_entry && *file_idx == anchor_file => {
                    self.cursor = j;
                    return;
                }
                _ => return, // hit file/commit boundary, stop
            }
        }
    }

    /// Move cursor up, constrained to the same file's diff lines (visual mode).
    pub fn visual_move_up(&mut self) {
        let Some((anchor_entry, anchor_file)) = self.visual_file() else {
            return;
        };
        for j in (0..self.cursor).rev() {
            if matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                continue;
            }
            match &self.rows[j] {
                DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    ..
                } if *entry_idx == anchor_entry && *file_idx == anchor_file => {
                    self.cursor = j;
                    return;
                }
                _ => return, // hit file/commit boundary, stop
            }
        }
    }

    /// Toggle all selectable lines in a visual range (active or persistent).
    /// If active visual mode, persists the range first.
    /// Clears the persistent range after toggling.
    pub fn toggle_visual_selection(&mut self) {
        // If actively selecting, persist first.
        if self.visual_anchor.is_some() {
            self.persist_visual_range();
            self.visual_anchor = None;
        }

        let Some(vr) = self.visual_range.clone() else {
            return;
        };

        // Collect line data from the persistent range before mutating.
        let line_data: Vec<(ChangeId, String, Option<u32>, Option<u32>)> = self
            .diff_states
            .iter()
            .filter_map(|((commit_id, path), diff_lines)| {
                let diff_lines = diff_lines.loaded()?;
                let cid = self.change_id_for_commit_key(commit_id)?;
                if cid != vr.change_id || path != &vr.path {
                    return None;
                }
                let lines: Vec<_> = diff_lines
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| {
                        let idx = DiffLineIdx::new(*i);
                        idx >= vr.start_line && idx <= vr.end_line
                    })
                    .filter(|(_, dl)| {
                        dl.kind == DiffLineKind::Added || dl.kind == DiffLineKind::Removed
                    })
                    .map(|(_, dl)| {
                        (
                            vr.change_id.clone(),
                            vr.path.clone(),
                            dl.old_line,
                            dl.new_line,
                        )
                    })
                    .collect();
                Some(lines)
            })
            .flatten()
            .collect();

        if line_data.is_empty() {
            return;
        }

        // If all are already selected, deselect. Otherwise select.
        let all_selected = line_data.iter().all(|(cid, path, ol, nl)| {
            let file_ref = FileRef {
                change_id: cid.clone(),
                path: path.clone(),
            };

            self.selection.contains(&Selection::Line {
                file_ref,
                old_line: *ol,
                new_line: *nl,
            })
        });

        if all_selected {
            for (cid, path, ol, nl) in &line_data {
                self.selection.remove(&Selection::Line {
                    file_ref: FileRef {
                        change_id: cid.clone(),
                        path: path.clone(),
                    },
                    old_line: *ol,
                    new_line: *nl,
                });
            }
        } else {
            if let Some((cid, ..)) = line_data.first() {
                self.clear_other_commits(cid);
            }
            self.selection.ensure_kind(SelectionKind::Line);
            for (cid, path, ol, nl) in line_data {
                self.selection.remove(&Selection::File(FileRef {
                    change_id: cid.clone(),
                    path: path.clone(),
                }));
                self.selection.insert(
                    SelectionKind::Line,
                    Selection::Line {
                        file_ref: FileRef {
                            change_id: cid,
                            path,
                        },
                        old_line: ol,
                        new_line: nl,
                    },
                );
            }
        }
    }
}
