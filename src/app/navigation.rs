use super::App;
use crate::dag::DiffLineKind;
use crate::idx::{EntryIdx, RowIdx};
use crate::types::DisplayRow;

/// Rows of context kept visible above and below the cursor when scrolling.
const SCROLL_PADDING: usize = 2;

impl App {
    /// Whether a row should be skipped during navigation.
    /// Skips graph links and context diff lines (not actionable).
    fn is_row_skippable(&self, row_idx: RowIdx) -> bool {
        let Some(row) = self.rows.get(row_idx.raw()) else {
            return true;
        };
        match row {
            DisplayRow::GraphLink { .. }
            | DisplayRow::DescriptionLine { .. }
            | DisplayRow::ConflictContext { .. }
            | DisplayRow::BookmarkSeparator => true,
            DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            } => self
                .diff_lines(*entry_idx, *file_idx)
                .and_then(|lines| lines.get(line_idx.raw()))
                .is_some_and(|dl| dl.kind == DiffLineKind::Context),
            DisplayRow::InterdiffDiffLine { file_idx, line_idx } => self
                .interdiff_diff_lines(*file_idx)
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

    /// Number of display lines a row occupies.
    pub fn row_display_lines(&self, row_idx: usize) -> usize {
        let Some(row) = self.rows.get(row_idx) else {
            return 1;
        };
        match row {
            DisplayRow::CommitNode { .. }
            | DisplayRow::OpLogItem { .. }
            | DisplayRow::EvoLogItem { .. } => 2,
            DisplayRow::AnnotateLine { line_idx }
                if self.annotate.show_commit_separators
                    && self.annotate_line_is_boundary(line_idx.raw()) =>
            {
                2
            }
            _ => 1,
        }
    }

    /// Whether an annotate line starts a new commit group (rendered with a
    /// separator line above it when separators are enabled).
    fn annotate_line_is_boundary(&self, line_idx: usize) -> bool {
        if line_idx == 0 {
            return false;
        }
        self.annotate
            .lines
            .loaded()
            .and_then(|lines| Some((lines.get(line_idx)?, lines.get(line_idx - 1)?)))
            .is_some_and(|(cur, prev)| cur.commit_id != prev.commit_id)
    }

    /// Adjust the scroll offset so the cursor row — plus up to
    /// [`SCROLL_PADDING`] rows of context above and below — is fully visible
    /// in a viewport of `viewport` display lines. Called before each render.
    pub fn update_scroll(&mut self, viewport: usize) {
        if self.rows.is_empty() || viewport == 0 {
            self.scroll = 0;
            return;
        }
        let cursor = self.cursor.raw().min(self.rows.len() - 1);
        let top_target = cursor.saturating_sub(SCROLL_PADDING);
        let bottom_target = (cursor + SCROLL_PADDING).min(self.rows.len() - 1);
        self.scroll = self.scroll.min(top_target);
        // Smallest offset that keeps rows through `bottom_target` fully visible.
        let mut lines = 0;
        let mut min_scroll = bottom_target;
        for idx in (0..=bottom_target).rev() {
            lines += self.row_display_lines(idx);
            if lines > viewport {
                break;
            }
            min_scroll = idx;
        }
        // If the viewport is too small for the padding, keep the cursor itself
        // visible rather than scrolling it off the top.
        self.scroll = self.scroll.max(min_scroll.min(cursor));
    }

    /// Compute the visible row range accounting for multi-line rows.
    /// Returns (start_row, end_row) where rows in start..end fit in the viewport.
    pub(super) fn visible_row_range(&self) -> (usize, usize) {
        let offset = self.scroll;
        let height = self.last_list_height as usize;
        let mut lines = 0;
        let mut end = offset;
        while end < self.rows.len() && lines < height {
            lines += self.row_display_lines(end);
            end += 1;
        }
        (offset, end)
    }

    /// First non-skippable row in the visible viewport.
    pub fn peek_screen_top(&self) -> Option<RowIdx> {
        let (start, end) = self.visible_row_range();
        (start..end)
            .map(RowIdx::new)
            .find(|&j| !self.is_row_skippable(j))
    }

    /// Non-skippable row nearest the middle of the visible viewport.
    pub fn peek_screen_middle(&self) -> Option<RowIdx> {
        let (start, end) = self.visible_row_range();
        // Find the row at the middle display line.
        let height = self.last_list_height as usize;
        let mid_line = height / 2;
        let mut lines = 0;
        let mut mid_row = start;
        for i in start..end {
            lines += self.row_display_lines(i);
            if lines > mid_line {
                mid_row = i;
                break;
            }
        }
        // Search outward from mid_row for a non-skippable row.
        let mut lo = mid_row;
        let mut hi = mid_row + 1;
        loop {
            if lo >= start {
                let j = RowIdx::new(lo);
                if !self.is_row_skippable(j) {
                    return Some(j);
                }
            }
            if hi < end {
                let j = RowIdx::new(hi);
                if !self.is_row_skippable(j) {
                    return Some(j);
                }
            }
            if lo <= start && hi >= end {
                return None;
            }
            lo = lo.saturating_sub(1);
            hi += 1;
        }
    }

    /// Last non-skippable row in the visible viewport.
    pub fn peek_screen_bottom(&self) -> Option<RowIdx> {
        let (start, end) = self.visible_row_range();
        (start..end)
            .rev()
            .map(RowIdx::new)
            .find(|&j| !self.is_row_skippable(j))
    }

    pub fn move_to_screen_top(&mut self) {
        if let Some(j) = self.peek_screen_top() {
            self.cursor = j;
        }
    }

    pub fn move_to_screen_middle(&mut self) {
        if let Some(j) = self.peek_screen_middle() {
            self.cursor = j;
        }
    }

    pub fn move_to_screen_bottom(&mut self) {
        if let Some(j) = self.peek_screen_bottom() {
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
        if let Some(DisplayRow::CommitNode { entry_idx }) = self.rows.get(self.cursor.raw())
            && let Some(row) = self.child_commit_row(*entry_idx)
        {
            return Some(row);
        }

        for j in (0..self.cursor.raw()).rev() {
            if self.is_section_boundary(j) {
                return Some(RowIdx::new(j));
            }
        }
        None
    }

    /// Context-aware section jump target downward.
    pub fn peek_down_section(&self) -> Option<RowIdx> {
        if let Some(DisplayRow::CommitNode { entry_idx }) = self.rows.get(self.cursor.raw())
            && let Some(row) = self.parent_commit_row(*entry_idx)
        {
            return Some(row);
        }

        for j in (self.cursor.raw() + 1)..self.rows.len() {
            if self.is_section_boundary(j) {
                return Some(RowIdx::new(j));
            }
        }
        None
    }

    /// Whether a row is a section boundary for J/K navigation.
    fn is_section_boundary(&self, j: usize) -> bool {
        let Some(row) = self.rows.get(j) else {
            return false;
        };
        match self.active_view {
            crate::app::ActiveView::Dag => {
                let commit_level = self.is_commit_level_jump();
                if commit_level {
                    matches!(row, DisplayRow::CommitNode { .. })
                } else {
                    matches!(
                        row,
                        DisplayRow::FileChange { .. } | DisplayRow::CommitNode { .. }
                    )
                }
            }
            crate::app::ActiveView::Bookmarks => matches!(row, DisplayRow::BookmarkItem { .. }),
            crate::app::ActiveView::Tags => matches!(row, DisplayRow::TagItem { .. }),
            crate::app::ActiveView::Operations => matches!(row, DisplayRow::OpLogItem { .. }),
            crate::app::ActiveView::Workspaces => matches!(row, DisplayRow::WorkspaceItem { .. }),
            crate::app::ActiveView::Evolog => {
                matches!(
                    row,
                    DisplayRow::EvoLogItem { .. } | DisplayRow::EvoLogFileChange { .. }
                )
            }
            crate::app::ActiveView::CommandLog => {
                matches!(row, DisplayRow::CommandLogItem { .. })
            }
            crate::app::ActiveView::Interdiff => {
                matches!(row, DisplayRow::InterdiffFileChange { .. })
            }
            crate::app::ActiveView::Annotate => {
                // Jump between commit boundaries in annotate view.
                if j == 0 {
                    return true;
                }
                let prev_commit = self.annotate.lines.loaded().and_then(|lines| {
                    // Find the line_idx for row j and j-1.
                    let cur_li = match row {
                        DisplayRow::AnnotateLine { line_idx } => Some(line_idx.raw()),
                        _ => None,
                    }?;
                    let prev_row = self.rows.get(j - 1)?;
                    let prev_li = match prev_row {
                        DisplayRow::AnnotateLine { line_idx } => Some(line_idx.raw()),
                        DisplayRow::AnnotateDetail { line_idx, .. } => Some(line_idx.raw()),
                        _ => None,
                    }?;
                    let cur_cid = &lines.get(cur_li)?.commit_id;
                    let prev_cid = &lines.get(prev_li)?.commit_id;
                    Some(cur_cid != prev_cid)
                });
                matches!(row, DisplayRow::AnnotateLine { .. }) && prev_commit.unwrap_or(false)
            }
        }
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

    /// Jump to the working copy commit (`@`). Returns whether it was found.
    pub fn jump_to_working_copy(&mut self) -> bool {
        if let Some(pos) = self.peek_working_copy() {
            self.cursor = pos;
            true
        } else {
            false
        }
    }

    /// Jump to the commit (or bookmark-view entry) with this bookmark.
    /// Returns whether it was found.
    pub fn jump_to_bookmark(&mut self, name: &crate::types::BookmarkName) -> bool {
        // If in the bookmark view, find the entry by name.
        if self.active_view == super::ActiveView::Bookmarks {
            for (idx, entry) in self.views.bookmark_entries.iter().enumerate() {
                if entry.name == *name
                    && let Some(pos) = self.rows.iter().position(|r| {
                        *r == crate::types::DisplayRow::BookmarkItem {
                            bookmark_idx: crate::idx::BookmarkIdx::new(idx),
                        }
                    })
                {
                    self.cursor = RowIdx::new(pos);
                    return true;
                }
            }
        }
        // In DAG view, find the commit with this bookmark.
        for (idx, node) in self.nodes.iter_enumerated() {
            if node.commit.bookmarks.iter().any(|b| b.name == *name)
                && let Some(row) = self.row_of_commit(idx)
            {
                self.cursor = row;
                return true;
            }
        }
        false
    }

    /// Jump to a commit by change/commit ID prefix. Returns whether it was found.
    pub fn jump_to_change_id(&mut self, prefix: &str) -> bool {
        for (idx, node) in self.nodes.iter_enumerated() {
            if (node.commit.change_id.display.starts_with(prefix)
                || node.commit.graph_id.as_str().starts_with(prefix))
                && let Some(row) = self.row_of_commit(idx)
            {
                self.cursor = row;
                return true;
            }
        }
        false
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
    /// accounting for multi-line items.
    pub fn row_at_screen_line(&self, screen_line: usize) -> RowIdx {
        let mut lines_consumed = 0;
        for idx in self.scroll..self.rows.len() {
            let height = self.row_display_lines(idx);
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

    /// Set cursor to a specific row, clamping to valid bounds.
    pub fn set_cursor(&mut self, row: RowIdx) {
        self.cursor = RowIdx::new(row.raw().min(self.rows.len().saturating_sub(1)));
    }
}
