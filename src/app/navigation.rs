use super::App;
use crate::dag::DiffLineKind;
use crate::idx::{EntryIdx, RowIdx};
use crate::types::DisplayRow;

impl App {
    /// Whether a row should be skipped during navigation.
    /// Skips graph links and context diff lines (not actionable).
    fn is_row_skippable(&self, row_idx: RowIdx) -> bool {
        match &self.rows[row_idx.raw()] {
            DisplayRow::GraphLink { .. }
            | DisplayRow::DescriptionLine { .. }
            | DisplayRow::ConflictContext { .. } => true,
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

    /// Previous selectable row above the cursor.
    pub fn peek_up(&self) -> Option<RowIdx> {
        (0..self.cursor.raw())
            .rev()
            .map(RowIdx::new)
            .find(|&j| !self.is_row_skippable(j))
    }

    /// Next selectable row below the cursor.
    pub fn peek_down(&self) -> Option<RowIdx> {
        ((self.cursor.raw() + 1)..self.rows.len())
            .map(RowIdx::new)
            .find(|&j| !self.is_row_skippable(j))
    }

    /// First selectable row.
    pub fn peek_top(&self) -> Option<RowIdx> {
        (0..self.rows.len())
            .map(RowIdx::new)
            .find(|&j| !self.is_row_skippable(j))
    }

    /// Last selectable row.
    pub fn peek_bottom(&self) -> Option<RowIdx> {
        (0..self.rows.len())
            .rev()
            .map(RowIdx::new)
            .find(|&j| !self.is_row_skippable(j))
    }

    pub fn move_up(&mut self) {
        if let Some(j) = self.peek_up() {
            self.cursor = j;
        }
    }

    pub fn move_down(&mut self) {
        if let Some(j) = self.peek_down() {
            self.cursor = j;
        }
    }

    pub fn move_to_top(&mut self) {
        if let Some(j) = self.peek_top() {
            self.cursor = j;
        }
    }

    pub fn move_to_bottom(&mut self) {
        if let Some(j) = self.peek_bottom() {
            self.cursor = j;
        }
    }

    /// Row index of the first parent commit in the DAG (for J on commit rows).
    fn parent_commit_row(&self, entry_idx: EntryIdx) -> Option<RowIdx> {
        let parent_idx = *self.nodes[entry_idx].parents.first()?;
        self.row_of_commit(parent_idx)
    }

    /// Row index of the first child commit in the DAG (for K on commit rows).
    /// When multiple children exist, picks the one closest above the current row.
    fn child_commit_row(&self, entry_idx: EntryIdx) -> Option<RowIdx> {
        let children = &self.nodes[entry_idx].children;
        match children.len() {
            0 => None,
            1 => self.row_of_commit(children[0]),
            _ => {
                // Multiple children: pick the one closest above the current cursor.
                let cur = self.cursor;
                children
                    .iter()
                    .filter_map(|&idx| self.row_of_commit(idx))
                    .filter(|&row| row < cur)
                    .max()
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
        match self.rows.get(self.cursor.raw()) {
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

    /// Context-aware section jump target upward.
    pub fn peek_up_section(&self) -> Option<RowIdx> {
        if let Some(DisplayRow::CommitNode { entry_idx }) = self.rows.get(self.cursor.raw()) {
            if let Some(row) = self.child_commit_row(*entry_idx) {
                return Some(row);
            }
        }

        let commit_level = self.is_commit_level_jump();
        for j in (0..self.cursor.raw()).rev() {
            let target = if commit_level {
                matches!(self.rows[j], DisplayRow::CommitNode { .. })
            } else {
                matches!(
                    self.rows[j],
                    DisplayRow::FileChange { .. } | DisplayRow::CommitNode { .. }
                )
            };
            if target {
                return Some(RowIdx::new(j));
            }
        }
        None
    }

    /// Context-aware section jump target downward.
    pub fn peek_down_section(&self) -> Option<RowIdx> {
        if let Some(DisplayRow::CommitNode { entry_idx }) = self.rows.get(self.cursor.raw()) {
            if let Some(row) = self.parent_commit_row(*entry_idx) {
                return Some(row);
            }
        }

        let commit_level = self.is_commit_level_jump();
        for j in (self.cursor.raw() + 1)..self.rows.len() {
            let target = if commit_level {
                matches!(self.rows[j], DisplayRow::CommitNode { .. })
            } else {
                matches!(
                    self.rows[j],
                    DisplayRow::FileChange { .. } | DisplayRow::CommitNode { .. }
                )
            };
            if target {
                return Some(RowIdx::new(j));
            }
        }
        None
    }

    pub fn move_up_section(&mut self) {
        if let Some(j) = self.peek_up_section() {
            self.cursor = j;
        }
    }

    pub fn move_down_section(&mut self) {
        if let Some(j) = self.peek_down_section() {
            self.cursor = j;
        }
    }

    /// Row of the working copy commit (`@`), if present.
    pub fn peek_working_copy(&self) -> Option<RowIdx> {
        self.rows
            .iter()
            .position(|r| {
                matches!(r, DisplayRow::CommitNode { entry_idx }
                    if self.nodes[*entry_idx].commit.is_working_copy())
            })
            .map(RowIdx::new)
    }

    /// Jump to the working copy commit (`@`).
    pub fn jump_to_working_copy(&mut self) {
        if let Some(pos) = self.peek_working_copy() {
            self.cursor = pos;
        } else {
            self.set_error("working copy not in current revset");
        }
    }

    pub fn jump_to_bookmark(&mut self, name: &crate::types::BookmarkName) {
        // If in the bookmark view, find the entry by name.
        if self.active_view == super::ActiveView::Bookmarks {
            for (idx, entry) in self.views.bookmark_entries.iter().enumerate() {
                if entry.name == *name {
                    if let Some(pos) = self.rows.iter().position(|r| {
                        r.key()
                            == crate::types::RowKey::BookmarkItem(crate::idx::BookmarkIdx::new(idx))
                    }) {
                        self.cursor = RowIdx::new(pos);
                        return;
                    }
                }
            }
        }
        // In DAG view, find the commit with this bookmark.
        for (idx, node) in self.nodes.iter_enumerated() {
            if node.commit.bookmarks.iter().any(|b| b.name == *name) {
                if let Some(row) = self.row_of_commit(idx) {
                    self.cursor = row;
                    return;
                }
            }
        }
        self.set_status("bookmark not in current revset");
    }

    pub fn jump_to_change_id(&mut self, prefix: &str) {
        for (idx, node) in self.nodes.iter_enumerated() {
            if node.commit.change_id.display.starts_with(prefix)
                || node.commit.graph_id.as_str().starts_with(prefix)
            {
                if let Some(row) = self.row_of_commit(idx) {
                    self.cursor = row;
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
    pub fn row_at_screen_line(&self, screen_line: usize) -> RowIdx {
        let offset = self.list_state.offset();
        let mut lines_consumed = 0;
        for idx in offset..self.rows.len() {
            let height = match self.rows[idx] {
                DisplayRow::CommitNode { .. } | DisplayRow::OpLogItem { .. } => 2,
                _ => 1,
            };
            if lines_consumed + height > screen_line {
                return RowIdx::new(idx);
            }
            lines_consumed += height;
        }
        // Past the end — clamp to last row.
        RowIdx::new(self.rows.len().saturating_sub(1))
    }

    /// Select a specific row index (e.g. from mouse click), snapping to the
    /// nearest non-skippable row at or after `row`.
    pub fn select_row(&mut self, row: RowIdx) {
        let target = row.raw().min(self.rows.len().saturating_sub(1));
        for j in target..self.rows.len() {
            let j = RowIdx::new(j);
            if !self.is_row_skippable(j) {
                self.cursor = j;
                return;
            }
        }
        for j in (0..target).rev() {
            let j = RowIdx::new(j);
            if !self.is_row_skippable(j) {
                self.cursor = j;
                return;
            }
        }
    }
}
