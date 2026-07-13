use super::{App, PersistentVisualRange, VisualMode};
use crate::dag::DiffLineKind;
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx, RowIdx};
use crate::keymap::AppAction;
use crate::types::{DisplayRow, FileRef, Selection, SelectionKind, VisualRange};

enum SearchDir {
    Up,
    Down,
}

impl App {
    /// Whether a file row is in the active or persistent visual file range.
    /// O(1): in file visual mode the anchor and cursor always sit on
    /// `FileChange` rows of the same entry, and file rows appear in
    /// `file_idx` order, so the range is just the two rows' file indices.
    pub fn is_in_visual_file_range(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> bool {
        if let Some(VisualMode::Files {
            anchor,
            entry_idx: ve,
        }) = &self.visual.mode
            && entry_idx == *ve
            && let Some(a) = self.file_idx_at_row(*anchor, *ve)
            && let Some(c) = self.file_idx_at_row(self.cursor, *ve)
            && file_idx >= a.min(c)
            && file_idx <= a.max(c)
        {
            return true;
        }
        if let Some(PersistentVisualRange::Files {
            entry_idx: ve,
            lo,
            hi,
        }) = &self.visual.persistent
            && entry_idx == *ve
            && file_idx >= *lo
            && file_idx <= *hi
        {
            return true;
        }
        false
    }

    /// The file index of the `FileChange` row at `row`, if it belongs to `entry_idx`.
    fn file_idx_at_row(&self, row: RowIdx, entry_idx: EntryIdx) -> Option<FileIdx> {
        match self.rows.get(row.raw()) {
            Some(DisplayRow::FileChange {
                entry_idx: ei,
                file_idx,
            }) if *ei == entry_idx => Some(*file_idx),
            _ => None,
        }
    }
}

impl App {
    /// Whether any visual mode is active (line or commit).
    pub fn in_visual_mode(&self) -> bool {
        self.visual.mode.is_some()
    }

    /// Enter visual mode based on the current cursor row type.
    /// CommitNode → commit visual mode, DiffLine → line visual mode, FileChange → file visual mode.
    pub fn enter_visual_mode(&mut self) {
        match self.rows.get(self.cursor.raw()) {
            Some(DisplayRow::CommitNode { entry_idx }) => {
                let entry_idx = *entry_idx;
                self.visual.mode = Some(VisualMode::Commits {
                    anchor: entry_idx,
                    path: vec![entry_idx],
                });
                self.visual.persistent = None;
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
                    self.visual.mode = Some(VisualMode::Lines {
                        anchor: self.cursor,
                    });
                    self.visual.persistent = None;
                }
            }
            Some(DisplayRow::FileChange { entry_idx, .. }) => {
                let entry_idx = *entry_idx;
                self.visual.mode = Some(VisualMode::Files {
                    anchor: self.cursor,
                    entry_idx,
                });
                self.visual.persistent = None;
            }
            _ => {}
        }
    }

    /// Exit visual mode with `v`: persist the range but don't create selections.
    pub fn exit_visual_mode(&mut self) {
        match self.visual.mode.take() {
            Some(VisualMode::Lines { anchor }) => {
                self.visual.persistent = self.compute_line_range(anchor);
            }
            Some(VisualMode::Commits { path, .. }) => {
                if !path.is_empty() {
                    self.visual.persistent = Some(PersistentVisualRange::Commits(path));
                }
            }
            Some(VisualMode::Files { anchor, entry_idx }) => {
                self.visual.persistent = self.compute_file_range(anchor, entry_idx);
            }
            None => {}
        }
    }

    /// Cancel visual mode without persisting (Esc or other action).
    pub fn cancel_visual_mode(&mut self) {
        self.visual.mode = None;
    }

    /// Space in visual mode: convert range to explicit selections and exit.
    pub fn persist_visual_selection(&mut self) {
        match self.visual.mode.take() {
            Some(VisualMode::Lines { anchor }) => {
                self.visual.persistent = self.compute_line_range(anchor);
                self.toggle_line_visual_selection();
            }
            Some(VisualMode::Commits { path, .. }) => {
                self.visual.persistent = Some(PersistentVisualRange::Commits(path));
                self.toggle_commit_visual_selection();
            }
            Some(VisualMode::Files { anchor, entry_idx }) => {
                self.visual.persistent = self.compute_file_range(anchor, entry_idx);
                self.toggle_file_visual_selection();
            }
            None => {}
        }
    }

    /// Check if the cursor is in a persistent visual range (for space toggle).
    pub fn cursor_in_persistent_visual_range(&self) -> bool {
        match &self.visual.persistent {
            Some(PersistentVisualRange::Lines(vr)) => {
                if let Some(DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    line_idx,
                }) = self.rows.get(self.cursor.raw())
                {
                    let cid = self.nodes[*entry_idx].commit.unique_change_id();
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
            Some(PersistentVisualRange::Commits(range)) => match self.rows.get(self.cursor.raw()) {
                Some(DisplayRow::CommitNode { entry_idx }) => range.contains(entry_idx),
                _ => false,
            },
            Some(PersistentVisualRange::Files { entry_idx, lo, hi }) => {
                match self.rows.get(self.cursor.raw()) {
                    Some(DisplayRow::FileChange {
                        entry_idx: ei,
                        file_idx,
                    }) => ei == entry_idx && file_idx >= lo && file_idx <= hi,
                    _ => false,
                }
            }
            None => false,
        }
    }

    /// Space on a persistent visual range: toggle into/out of explicit selections.
    pub fn toggle_persistent_visual_selection(&mut self) {
        match &self.visual.persistent {
            Some(PersistentVisualRange::Lines(_)) => {
                self.toggle_line_visual_selection();
            }
            Some(PersistentVisualRange::Commits(_)) => {
                self.toggle_commit_visual_selection();
            }
            Some(PersistentVisualRange::Files { .. }) => {
                self.toggle_file_visual_selection();
            }
            None => {}
        }
    }

    pub fn visual_move_down(&mut self) {
        match &self.visual.mode {
            Some(VisualMode::Lines { .. }) => self.line_visual_move_down(),
            Some(VisualMode::Commits { .. }) => self.commit_visual_move_down(),
            Some(VisualMode::Files { .. }) => self.file_visual_move_down(),
            None => {}
        }
    }

    pub fn visual_move_up(&mut self) {
        match &self.visual.mode {
            Some(VisualMode::Lines { .. }) => self.line_visual_move_up(),
            Some(VisualMode::Commits { .. }) => self.commit_visual_move_up(),
            Some(VisualMode::Files { .. }) => self.file_visual_move_up(),
            None => {}
        }
    }

    pub fn visual_move(&mut self, action: AppAction, page_size: usize) {
        if matches!(action, AppAction::MoveDown | AppAction::MoveUp) {
            match action {
                AppAction::MoveDown => self.visual_move_down(),
                AppAction::MoveUp => self.visual_move_up(),
                _ => unreachable!(),
            }
            return;
        }

        let old_cursor = self.cursor;
        self.execute_movement(action, page_size);
        if self.cursor == old_cursor {
            return;
        }

        match &self.visual.mode {
            Some(VisualMode::Lines { anchor }) => {
                let anchor = *anchor;
                if !self.clamp_line_visual_cursor(anchor) {
                    self.cursor = old_cursor;
                }
            }
            Some(VisualMode::Commits { .. }) => {
                if !self.rebuild_commit_visual_path() {
                    self.cursor = old_cursor;
                }
            }
            Some(VisualMode::Files { entry_idx, .. }) => {
                let entry_idx = *entry_idx;
                if !self.clamp_file_visual_cursor(entry_idx) {
                    self.cursor = old_cursor;
                }
            }
            None => {}
        }
    }

    fn execute_movement(&mut self, action: AppAction, page_size: usize) {
        match action {
            AppAction::MoveDown => self.move_down(),
            AppAction::MoveUp => self.move_up(),
            AppAction::MoveDownSection => self.move_down_section(),
            AppAction::MoveUpSection => self.move_up_section(),
            AppAction::PageDown => self.page_down(page_size),
            AppAction::PageUp => self.page_up(page_size),
            AppAction::MoveToTop => self.move_to_top(),
            AppAction::MoveToBottom => self.move_to_bottom(),
            AppAction::MoveToScreenTop => self.move_to_screen_top(),
            AppAction::MoveToScreenMiddle => self.move_to_screen_middle(),
            AppAction::MoveToScreenBottom => self.move_to_screen_bottom(),
            AppAction::JumpToWorkingCopy => {
                self.jump_to_working_copy();
            }
            _ => {}
        }
    }

    fn rebuild_commit_visual_path(&mut self) -> bool {
        let Some(entry) = self.rows.get(self.cursor.raw()).and_then(|r| r.entry_idx()) else {
            return false;
        };
        let Some(VisualMode::Commits { anchor, path }) = &mut self.visual.mode else {
            return false;
        };
        let lo = anchor.raw().min(entry.raw());
        let hi = anchor.raw().max(entry.raw());
        *path = (lo..=hi).map(EntryIdx::new).collect();
        self.jump_cursor_to_commit(entry);
        true
    }

    fn clamp_line_visual_cursor(&mut self, anchor: RowIdx) -> bool {
        let Some((anchor_entry, anchor_file)) = (match self.rows.get(anchor.raw()) {
            Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                ..
            }) => Some((*entry_idx, *file_idx)),
            _ => None,
        }) else {
            return false;
        };

        if self.is_valid_line_visual_row(self.cursor, anchor_entry, anchor_file) {
            return true;
        }

        let search_dir = if self.cursor > anchor {
            SearchDir::Down
        } else {
            SearchDir::Up
        };
        if let Some(clamped) =
            self.find_line_visual_bound(anchor, anchor_entry, anchor_file, search_dir)
        {
            self.cursor = clamped;
            true
        } else {
            false
        }
    }

    fn is_valid_line_visual_row(&self, row: RowIdx, entry: EntryIdx, file: FileIdx) -> bool {
        match self.rows.get(row.raw()) {
            Some(DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            }) if *entry_idx == entry && *file_idx == file => self
                .diff_lines(*entry_idx, *file_idx)
                .and_then(|lines| lines.get(line_idx.raw()))
                .is_some_and(|dl| dl.kind != DiffLineKind::Context),
            _ => false,
        }
    }

    fn find_line_visual_bound(
        &self,
        anchor: RowIdx,
        entry: EntryIdx,
        file: FileIdx,
        dir: SearchDir,
    ) -> Option<RowIdx> {
        let range: Box<dyn Iterator<Item = usize>> = match dir {
            SearchDir::Down => Box::new((anchor.raw() + 1)..self.rows.len()),
            SearchDir::Up => Box::new((0..anchor.raw()).rev()),
        };
        let mut last_valid = None;
        for j in range {
            match &self.rows[j] {
                DisplayRow::GraphLink { .. } => continue,
                DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    line_idx,
                } if *entry_idx == entry && *file_idx == file => {
                    let is_selectable = self
                        .diff_lines(*entry_idx, *file_idx)
                        .and_then(|lines| lines.get(line_idx.raw()))
                        .is_some_and(|dl| dl.kind != DiffLineKind::Context);
                    if is_selectable {
                        last_valid = Some(RowIdx::new(j));
                    }
                }
                _ => break,
            }
        }
        last_valid
    }

    fn clamp_file_visual_cursor(&mut self, entry_idx: EntryIdx) -> bool {
        if matches!(
            self.rows.get(self.cursor.raw()),
            Some(DisplayRow::FileChange { entry_idx: ei, .. }) if *ei == entry_idx
        ) {
            return true;
        }

        let anchor_row = match &self.visual.mode {
            Some(VisualMode::Files { anchor, .. }) => *anchor,
            _ => return false,
        };

        let search_dir = if self.cursor > anchor_row {
            SearchDir::Down
        } else {
            SearchDir::Up
        };
        if let Some(clamped) = self.find_file_visual_bound(anchor_row, entry_idx, search_dir) {
            self.cursor = clamped;
            true
        } else {
            false
        }
    }

    fn find_file_visual_bound(
        &self,
        anchor: RowIdx,
        entry_idx: EntryIdx,
        dir: SearchDir,
    ) -> Option<RowIdx> {
        let range: Box<dyn Iterator<Item = usize>> = match dir {
            SearchDir::Down => Box::new((anchor.raw() + 1)..self.rows.len()),
            SearchDir::Up => Box::new((0..anchor.raw()).rev()),
        };
        let mut last_valid = None;
        for j in range {
            match &self.rows[j] {
                DisplayRow::GraphLink { .. } => continue,
                DisplayRow::FileChange { entry_idx: ei, .. } if *ei == entry_idx => {
                    last_valid = Some(RowIdx::new(j));
                }
                _ => break,
            }
        }
        last_valid
    }

    /// Check if a diff line is in the visual range (active or persistent).
    pub fn is_in_visual_range(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
        row_idx: RowIdx,
    ) -> bool {
        // Check active line visual range.
        if let Some(VisualMode::Lines { anchor }) = &self.visual.mode {
            let lo = (*anchor).min(self.cursor);
            let hi = (*anchor).max(self.cursor);
            if row_idx >= lo && row_idx <= hi {
                return true;
            }
        }

        // Check persistent line range.
        if let Some(PersistentVisualRange::Lines(vr)) = &self.visual.persistent {
            let cid = self.nodes[entry_idx].commit.unique_change_id();
            if let Some(files) = self.files_for_entry(entry_idx)
                && let Some(file) = files.get(file_idx.raw())
                && cid == vr.change_id
                && file.path == vr.path
                && line_idx >= vr.start_line
                && line_idx <= vr.end_line
            {
                return true;
            }
        }

        false
    }

    /// Check if a commit is in the visual range (active or persistent).
    pub fn is_in_visual_commit_range(&self, entry_idx: EntryIdx) -> bool {
        if let Some(VisualMode::Commits { path, .. }) = &self.visual.mode
            && path.contains(&entry_idx)
        {
            return true;
        }
        if let Some(PersistentVisualRange::Commits(range)) = &self.visual.persistent
            && range.contains(&entry_idx)
        {
            return true;
        }
        false
    }

    fn visual_line_file(&self) -> Option<(EntryIdx, FileIdx)> {
        let Some(VisualMode::Lines { anchor }) = &self.visual.mode else {
            return None;
        };
        match self.rows.get(anchor.raw()) {
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
        for j in (self.cursor.raw() + 1)..self.rows.len() {
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
                    self.cursor = RowIdx::new(j);
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
        for j in (0..self.cursor.raw()).rev() {
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
                    self.cursor = RowIdx::new(j);
                    return;
                }
                _ => return,
            }
        }
    }

    /// Compute the persistent line range from the given anchor and current cursor.
    fn compute_line_range(&self, anchor: RowIdx) -> Option<PersistentVisualRange> {
        let lo = anchor.min(self.cursor).raw();
        let hi = anchor.max(self.cursor).raw();

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
                    change_id = Some(self.nodes[*entry_idx].commit.unique_change_id());
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
        let Some(PersistentVisualRange::Lines(vr)) = &self.visual.persistent else {
            return;
        };
        let vr = vr.clone();

        let line_data: Vec<Selection> = self
            .nodes
            .iter()
            .filter_map(|node| {
                let cid = node.commit.unique_change_id();
                if cid != vr.change_id {
                    return None;
                }
                let files = node.files.loaded()?;
                let fi = FileIdx::new(files.iter().position(|f| f.path == vr.path)?);
                let diff_lines = node.diff(fi, super::DiffFormat::Git)?;
                let lines: Vec<_> = diff_lines
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| {
                        let idx = DiffLineIdx::new(*i);
                        idx >= vr.start_line && idx <= vr.end_line
                    })
                    .filter(|(_, dl)| dl.kind.is_selectable())
                    .map(|(_, dl)| Selection::Line {
                        file_ref: FileRef {
                            change_id: vr.change_id.clone(),
                            path: vr.path.clone(),
                        },
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

        let all_selected = line_data.iter().all(|s| self.selection.contains(s));

        if all_selected {
            for s in &line_data {
                self.selection.remove(s);
            }
        } else {
            if let Some(Selection::Line { file_ref, .. }) = line_data.first() {
                self.clear_other_commits(&file_ref.change_id);
            }
            self.selection.ensure_kind(SelectionKind::Line);
            for s in line_data {
                if let Selection::Line { ref file_ref, .. } = s {
                    self.selection.remove(&Selection::File(file_ref.clone()));
                }
                self.selection.insert(s);
            }
        }
    }

    fn commit_visual_move_down(&mut self) {
        let Some(VisualMode::Commits { anchor, path }) = &self.visual.mode else {
            return;
        };
        let anchor = *anchor;

        // If anchor is at the bottom (last), cursor is at top — shrink from top.
        if path.len() > 1 && path.last() == Some(&anchor) {
            let Some(VisualMode::Commits { path, .. }) = &mut self.visual.mode else {
                return;
            };
            path.remove(0);
            let target = path[0];
            self.jump_cursor_to_commit(target);
            return;
        }

        // Otherwise extend at the bottom: next entry in display order.
        let tail = match &self.visual.mode {
            Some(VisualMode::Commits { path, .. }) => path.last().copied(),
            _ => None,
        };
        let Some(tail) = tail else { return };
        if let Some(parent) = self.next_entry_down(tail) {
            let Some(VisualMode::Commits { path, .. }) = &mut self.visual.mode else {
                return;
            };
            path.push(parent);
            self.jump_cursor_to_commit(parent);
        }
    }

    fn commit_visual_move_up(&mut self) {
        let Some(VisualMode::Commits { anchor, path }) = &self.visual.mode else {
            return;
        };
        let anchor = *anchor;

        // If anchor is at the top (first), cursor is at bottom — shrink from bottom.
        if path.len() > 1 && path.first() == Some(&anchor) {
            let Some(VisualMode::Commits { path, .. }) = &mut self.visual.mode else {
                return;
            };
            path.pop();
            let target = *path.last().expect("path has >1 element after length check");
            self.jump_cursor_to_commit(target);
            return;
        }

        // Otherwise extend at the top: next entry in display order (upward).
        let head = match &self.visual.mode {
            Some(VisualMode::Commits { path, .. }) => path.first().copied(),
            _ => None,
        };
        let Some(head) = head else { return };
        if let Some(child) = self.next_entry_up(head) {
            let Some(VisualMode::Commits { path, .. }) = &mut self.visual.mode else {
                return;
            };
            path.insert(0, child);
            self.jump_cursor_to_commit(child);
        }
    }

    /// Toggle all commits in the persistent visual commit range as explicit selections.
    fn toggle_commit_visual_selection(&mut self) {
        let Some(PersistentVisualRange::Commits(range)) = &self.visual.persistent else {
            return;
        };

        self.selection.ensure_kind(SelectionKind::Commit);
        let all_selected = range.iter().all(|idx| {
            let cid = self.nodes[*idx].commit.unique_change_id();
            self.selection.contains(&Selection::Commit(cid))
        });

        // Clone the range to avoid borrowing self.visual.persistent while mutating selection.
        let range: Vec<EntryIdx> = range.clone();
        if all_selected {
            for idx in &range {
                let cid = self.nodes[*idx].commit.unique_change_id();
                self.selection.remove(&Selection::Commit(cid));
            }
        } else {
            for idx in &range {
                let cid = self.nodes[*idx].commit.unique_change_id();
                self.selection.insert(Selection::Commit(cid));
            }
        }
    }

    /// The next entry in display order (downward = toward parents).
    fn next_entry_down(&self, entry_idx: EntryIdx) -> Option<EntryIdx> {
        let next = entry_idx.raw() + 1;
        if next < self.nodes.len() {
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

    fn file_visual_move_down(&mut self) {
        let Some(VisualMode::Files { entry_idx, .. }) = &self.visual.mode else {
            return;
        };
        let entry_idx = *entry_idx;
        for j in (self.cursor.raw() + 1)..self.rows.len() {
            match &self.rows[j] {
                DisplayRow::GraphLink { .. } => continue,
                DisplayRow::FileChange { entry_idx: ei, .. } if *ei == entry_idx => {
                    self.cursor = RowIdx::new(j);
                    return;
                }
                _ => return,
            }
        }
    }

    fn file_visual_move_up(&mut self) {
        let Some(VisualMode::Files { entry_idx, .. }) = &self.visual.mode else {
            return;
        };
        let entry_idx = *entry_idx;
        for j in (0..self.cursor.raw()).rev() {
            match &self.rows[j] {
                DisplayRow::GraphLink { .. } => continue,
                DisplayRow::FileChange { entry_idx: ei, .. } if *ei == entry_idx => {
                    self.cursor = RowIdx::new(j);
                    return;
                }
                _ => return,
            }
        }
    }

    fn compute_file_range(
        &self,
        anchor: RowIdx,
        entry_idx: EntryIdx,
    ) -> Option<PersistentVisualRange> {
        let lo = anchor.min(self.cursor).raw();
        let hi = anchor.max(self.cursor).raw();
        let mut min_fi: Option<FileIdx> = None;
        let mut max_fi: Option<FileIdx> = None;
        for i in lo..=hi {
            if let Some(DisplayRow::FileChange {
                entry_idx: ei,
                file_idx,
            }) = self.rows.get(i)
                && *ei == entry_idx
            {
                min_fi = Some(min_fi.map_or(*file_idx, |m: FileIdx| m.min(*file_idx)));
                max_fi = Some(max_fi.map_or(*file_idx, |m: FileIdx| m.max(*file_idx)));
            }
        }
        match (min_fi, max_fi) {
            (Some(lo), Some(hi)) => Some(PersistentVisualRange::Files { entry_idx, lo, hi }),
            _ => None,
        }
    }

    fn toggle_file_visual_selection(&mut self) {
        let Some(PersistentVisualRange::Files { entry_idx, lo, hi }) = &self.visual.persistent
        else {
            return;
        };
        let (entry_idx, lo, hi) = (*entry_idx, *lo, *hi);

        let Some(files) = self.files_for_entry(entry_idx) else {
            return;
        };
        let change_id = self.nodes[entry_idx].commit.unique_change_id();
        let file_refs: Vec<FileRef> = files
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                let fi = FileIdx::new(*i);
                fi >= lo && fi <= hi
            })
            .map(|(_, f)| FileRef {
                change_id: change_id.clone(),
                path: f.path.clone(),
            })
            .collect();

        if file_refs.is_empty() {
            return;
        }

        let all_selected = file_refs
            .iter()
            .all(|fr| self.selection.contains(&Selection::File(fr.clone())));

        if all_selected {
            for fr in &file_refs {
                self.selection.remove(&Selection::File(fr.clone()));
            }
        } else {
            self.clear_other_commits(&change_id);
            self.selection.ensure_kind(SelectionKind::File);
            for fr in file_refs {
                self.selection.insert(Selection::File(fr));
            }
        }
    }

    fn jump_cursor_to_commit(&mut self, entry_idx: EntryIdx) {
        if let Some(pos) = self
            .rows
            .iter()
            .position(|r| matches!(r, DisplayRow::CommitNode { entry_idx: e } if *e == entry_idx))
        {
            self.cursor = RowIdx::new(pos);
        }
    }
}
