use super::App;
use crate::dag::DiffLineKind;
use crate::types::DisplayRow;

impl App {
    /// Whether a row should be skipped during navigation.
    /// Skips graph links and context diff lines (not actionable).
    fn is_row_skippable(&self, row_idx: usize) -> bool {
        match &self.rows[row_idx] {
            DisplayRow::GraphLink { .. } => true,
            DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            } => self
                .diff_lines(*entry_idx, *file_idx)
                .and_then(|lines| lines.get(line_idx.raw()))
                .is_some_and(|dl| dl.kind == DiffLineKind::Context),
            _ => false,
        }
    }

    /// Move selection to the previous selectable row (commit, file, or diff line).
    pub fn move_up(&mut self) {
        for j in (0..self.cursor).rev() {
            if !self.is_row_skippable(j) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the next selectable row (commit, file, or diff line).
    pub fn move_down(&mut self) {
        for j in (self.cursor + 1)..self.rows.len() {
            if !self.is_row_skippable(j) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the first selectable row.
    pub fn move_to_top(&mut self) {
        for j in 0..self.rows.len() {
            if !self.is_row_skippable(j) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the last selectable row.
    pub fn move_to_bottom(&mut self) {
        for j in (0..self.rows.len()).rev() {
            if !self.is_row_skippable(j) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Whether J/K should jump to the next commit (vs next file).
    ///
    /// - On `CommitNode`: always commit-level.
    /// - On collapsed `FileChange`: commit-level (j already moves between files).
    /// - On expanded `FileChange`: file-level (skip diff lines to next file).
    /// - On `DiffLine`: file-level (escape the current diff).
    fn is_commit_level_jump(&self) -> bool {
        match self.rows.get(self.cursor) {
            Some(DisplayRow::CommitNode { .. }) => true,
            Some(DisplayRow::FileChange {
                entry_idx,
                file_idx,
            }) => {
                // Collapsed file → commit-level. Expanded → file-level.
                !self.is_file_unfolded(*entry_idx, *file_idx)
            }
            _ => false, // DiffLine, GraphLink → file-level
        }
    }

    /// Context-aware section jump upward.
    ///
    /// - Commit-level: jump to the previous commit.
    /// - File-level: jump to the previous file or commit.
    pub fn move_up_section(&mut self) {
        let commit_level = self.is_commit_level_jump();

        for j in (0..self.cursor).rev() {
            let target = if commit_level {
                matches!(self.rows[j], DisplayRow::CommitNode { .. })
            } else {
                matches!(
                    self.rows[j],
                    DisplayRow::FileChange { .. } | DisplayRow::CommitNode { .. }
                )
            };
            if target {
                self.cursor = j;
                return;
            }
        }
    }

    /// Context-aware section jump downward.
    ///
    /// - Commit-level: jump to the next commit.
    /// - File-level: jump to the next file or commit.
    pub fn move_down_section(&mut self) {
        let commit_level = self.is_commit_level_jump();

        for j in (self.cursor + 1)..self.rows.len() {
            let target = if commit_level {
                matches!(self.rows[j], DisplayRow::CommitNode { .. })
            } else {
                matches!(
                    self.rows[j],
                    DisplayRow::FileChange { .. } | DisplayRow::CommitNode { .. }
                )
            };
            if target {
                self.cursor = j;
                return;
            }
        }
    }

    /// Jump to the working copy commit (`@`).
    pub fn jump_to_working_copy(&mut self) {
        if let Some(pos) = self.rows.iter().position(|r| {
            matches!(r, DisplayRow::CommitNode { entry_idx }
                if self.entries[*entry_idx].commit.is_working_copy())
        }) {
            self.cursor = pos;
        } else {
            self.set_error("working copy not in current revset");
        }
    }

    pub fn jump_to_bookmark(&mut self, name: &crate::types::BookmarkName) {
        for (idx, entry) in self.entries.iter_enumerated() {
            if entry.commit.bookmarks.iter().any(|b| b.name == *name) {
                if let Some(pos) = self.rows.iter().position(
                    |r| matches!(r, DisplayRow::CommitNode { entry_idx } if *entry_idx == idx),
                ) {
                    self.cursor = pos;
                    return;
                }
            }
        }
    }

    /// Move cursor up by `n` selectable rows (commits or files).
    pub fn page_up(&mut self, n: usize) {
        for _ in 0..n {
            let prev = self.cursor;
            self.move_up();
            if self.cursor == prev {
                break;
            }
        }
    }

    /// Move cursor down by `n` selectable rows (commits or files).
    pub fn page_down(&mut self, n: usize) {
        for _ in 0..n {
            let prev = self.cursor;
            self.move_down();
            if self.cursor == prev {
                break;
            }
        }
    }

    /// Map a screen line (relative to the list area top) to a row index,
    /// accounting for multi-line items (CommitNode = 2 lines, others = 1).
    pub fn row_at_screen_line(&self, screen_line: usize) -> usize {
        let offset = self.list_state.offset();
        let mut lines_consumed = 0;
        for idx in offset..self.rows.len() {
            let height = match self.rows[idx] {
                DisplayRow::CommitNode { .. } => 2,
                _ => 1,
            };
            if lines_consumed + height > screen_line {
                return idx;
            }
            lines_consumed += height;
        }
        // Past the end — clamp to last row.
        self.rows.len().saturating_sub(1)
    }

    /// Select a specific row index (e.g. from mouse click), snapping to the
    /// nearest non-skippable row at or after `row`.
    pub fn select_row(&mut self, row: usize) {
        let target = row.min(self.rows.len().saturating_sub(1));
        for j in target..self.rows.len() {
            if !self.is_row_skippable(j) {
                self.cursor = j;
                return;
            }
        }
        for j in (0..target).rev() {
            if !self.is_row_skippable(j) {
                self.cursor = j;
                return;
            }
        }
    }
}
