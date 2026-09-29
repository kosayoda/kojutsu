use super::{App, PersistentVisualRange, VisualMode};
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx, RowIdx};
use crate::keymap::AppAction;
use crate::types::FileOwner;
use crate::types::{DisplayRow, FileRef, Selection, SelectionKind, VisualRange};

#[derive(Clone, Copy)]
enum Direction {
    Up,
    Down,
}

/// The rows line and file visual modes are confined to: one file's diff
/// lines, or one commit's files.
#[derive(Clone, Copy)]
enum VisualRun {
    Lines { entry: EntryIdx, file: FileIdx },
    Files { entry: EntryIdx },
}

/// The commits between `a` and `b` in display order, low end first.
fn ordered(a: EntryIdx, b: EntryIdx) -> (EntryIdx, EntryIdx) {
    (a.min(b), a.max(b))
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
                owner: FileOwner::Dag(ei),
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
        let mode = match self.rows.get(self.cursor.raw()) {
            Some(DisplayRow::CommitNode { entry_idx }) => VisualMode::Commits {
                anchor: *entry_idx,
                head: *entry_idx,
            },
            Some(DisplayRow::DiffLine {
                owner: FileOwner::Dag(entry_idx),
                file_idx,
                line_idx,
            }) if self.diff_line_selectable(*entry_idx, *file_idx, *line_idx) => {
                VisualMode::Lines {
                    anchor: self.cursor,
                }
            }
            Some(DisplayRow::FileChange {
                owner: FileOwner::Dag(entry_idx),
                ..
            }) => VisualMode::Files {
                anchor: self.cursor,
                entry_idx: *entry_idx,
            },
            _ => return,
        };
        self.visual.mode = Some(mode);
        self.visual.persistent = None;
    }

    /// Exit visual mode with `v`: persist the range but don't create selections.
    pub fn exit_visual_mode(&mut self) {
        if let Some(mode) = self.visual.mode.take() {
            self.visual.persistent = self.persisted_range(&mode);
        }
    }

    /// Cancel visual mode without persisting (Esc or other action).
    pub fn cancel_visual_mode(&mut self) {
        self.visual.mode = None;
    }

    /// Space in visual mode: convert range to explicit selections and exit.
    pub fn persist_visual_selection(&mut self) {
        self.exit_visual_mode();
        self.toggle_persistent_visual_selection();
    }

    /// The range a visual mode leaves behind when it ends.
    fn persisted_range(&self, mode: &VisualMode) -> Option<PersistentVisualRange> {
        match *mode {
            VisualMode::Lines { anchor } => self.compute_line_range(anchor),
            VisualMode::Commits { anchor, head } => {
                let (lo, hi) = ordered(anchor, head);
                Some(PersistentVisualRange::Commits { lo, hi })
            }
            VisualMode::Files { anchor, entry_idx } => self.compute_file_range(anchor, entry_idx),
        }
    }

    /// Check if the cursor is in a persistent visual range (for space toggle).
    /// Space only lands here outside visual mode, where there is no active
    /// range, so the combined range checks answer for the persistent one.
    pub fn cursor_in_persistent_visual_range(&self) -> bool {
        match (&self.visual.persistent, self.rows.get(self.cursor.raw())) {
            (
                Some(PersistentVisualRange::Lines(_)),
                Some(DisplayRow::DiffLine {
                    owner: FileOwner::Dag(entry_idx),
                    file_idx,
                    line_idx,
                }),
            ) => self.in_persistent_line_range(*entry_idx, *file_idx, *line_idx),
            (
                Some(PersistentVisualRange::Commits { .. }),
                Some(DisplayRow::CommitNode { entry_idx }),
            ) => self.is_in_visual_commit_range(*entry_idx),
            (
                Some(PersistentVisualRange::Files { .. }),
                Some(DisplayRow::FileChange {
                    owner: FileOwner::Dag(entry_idx),
                    file_idx,
                }),
            ) => self.is_in_visual_file_range(*entry_idx, *file_idx),
            _ => false,
        }
    }

    /// Space on a persistent visual range: toggle into/out of explicit selections.
    pub fn toggle_persistent_visual_selection(&mut self) {
        match &self.visual.persistent {
            Some(PersistentVisualRange::Lines(_)) => self.toggle_line_visual_selection(),
            Some(PersistentVisualRange::Commits { .. }) => self.toggle_commit_visual_selection(),
            Some(PersistentVisualRange::Files { .. }) => self.toggle_file_visual_selection(),
            None => {}
        }
    }

    pub fn visual_move(&mut self, action: AppAction, page_size: usize) {
        match action {
            AppAction::MoveDown => return self.visual_step(Direction::Down),
            AppAction::MoveUp => return self.visual_step(Direction::Up),
            _ => {}
        }

        let old_cursor = self.cursor;
        self.execute_movement(action, page_size);
        if self.cursor == old_cursor {
            return;
        }

        let kept = match self.visual.mode {
            Some(VisualMode::Commits { anchor, .. }) => {
                match self.rows.get(self.cursor.raw()).and_then(|r| r.entry_idx()) {
                    Some(head) => {
                        self.set_commit_visual_head(anchor, head);
                        true
                    }
                    None => false,
                }
            }
            Some(VisualMode::Lines { anchor } | VisualMode::Files { anchor, .. }) => {
                self.clamp_visual_cursor(anchor)
            }
            None => true,
        };
        if !kept {
            self.cursor = old_cursor;
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

    /// Move the visual cursor one step: to the next commit, or to the next
    /// row of the run it is confined to.
    fn visual_step(&mut self, dir: Direction) {
        match self.visual.mode {
            Some(VisualMode::Commits { anchor, head }) => {
                let next = match dir {
                    Direction::Down => head.raw() + 1,
                    Direction::Up => match head.raw().checked_sub(1) {
                        Some(raw) => raw,
                        None => return,
                    },
                };
                if next < self.dag.nodes.len() {
                    self.set_commit_visual_head(anchor, EntryIdx::new(next));
                }
            }
            Some(VisualMode::Lines { anchor } | VisualMode::Files { anchor, .. }) => {
                let next = self
                    .visual_run(anchor)
                    .and_then(|run| self.visual_stops(self.cursor, dir, run).next());
                if let Some(row) = next {
                    self.cursor = row;
                }
            }
            None => {}
        }
    }

    fn set_commit_visual_head(&mut self, anchor: EntryIdx, head: EntryIdx) {
        self.visual.mode = Some(VisualMode::Commits { anchor, head });
        if let Some(row) = self.row_of_commit(head) {
            self.cursor = row;
        }
    }

    /// The run the visual mode anchored at `anchor` is confined to.
    fn visual_run(&self, anchor: RowIdx) -> Option<VisualRun> {
        match self.rows.get(anchor.raw())? {
            DisplayRow::DiffLine {
                owner: FileOwner::Dag(entry),
                file_idx,
                ..
            } => Some(VisualRun::Lines {
                entry: *entry,
                file: *file_idx,
            }),
            DisplayRow::FileChange {
                owner: FileOwner::Dag(entry),
                ..
            } => Some(VisualRun::Files { entry: *entry }),
            _ => None,
        }
    }

    /// Whether `row` belongs to `run`, and whether the cursor may stop on
    /// it (unselectable diff lines are passed over).
    fn run_membership(&self, row: &DisplayRow, run: VisualRun) -> (bool, bool) {
        match (row, run) {
            (
                DisplayRow::DiffLine {
                    owner: FileOwner::Dag(e),
                    file_idx,
                    line_idx,
                },
                VisualRun::Lines { entry, file },
            ) if *e == entry && *file_idx == file => {
                (true, self.diff_line_selectable(entry, file, *line_idx))
            }
            (
                DisplayRow::FileChange {
                    owner: FileOwner::Dag(e),
                    ..
                },
                VisualRun::Files { entry },
            ) if *e == entry => (true, true),
            _ => (false, false),
        }
    }

    /// The rows past `from` in `dir` that the cursor may stop on, up to
    /// where `run` ends. Graph links between rows don't end it.
    fn visual_stops(
        &self,
        from: RowIdx,
        dir: Direction,
        run: VisualRun,
    ) -> impl Iterator<Item = RowIdx> + '_ {
        let rows: Box<dyn Iterator<Item = usize>> = match dir {
            Direction::Down => Box::new((from.raw() + 1)..self.rows.len()),
            Direction::Up => Box::new((0..from.raw()).rev()),
        };
        rows.filter(|&j| !matches!(self.rows[j], DisplayRow::GraphLink { .. }))
            .map(move |j| (j, self.run_membership(&self.rows[j], run)))
            .take_while(|(_, (member, _))| *member)
            .filter(|(_, (_, stop))| *stop)
            .map(|(j, _)| RowIdx::new(j))
    }

    /// Keep the cursor inside the run after a jump: if it landed outside,
    /// pull it back to the run's last stop in the direction it went, or to
    /// the anchor when the anchor is already the last one. False when the
    /// anchor isn't in a run.
    fn clamp_visual_cursor(&mut self, anchor: RowIdx) -> bool {
        let Some(run) = self.visual_run(anchor) else {
            return false;
        };
        if let Some(row) = self.rows.get(self.cursor.raw())
            && self.run_membership(row, run) == (true, true)
        {
            return true;
        }
        let dir = if self.cursor > anchor {
            Direction::Down
        } else {
            Direction::Up
        };
        self.cursor = self.visual_stops(anchor, dir, run).last().unwrap_or(anchor);
        true
    }

    /// Whether the diff line at `(entry, file, line)` can be individually
    /// selected: the single selectability test for line visual mode.
    /// Excludes context lines and conflict-region lines (which the pick
    /// path rejects), so highlight and selection agree.
    fn diff_line_selectable(&self, entry: EntryIdx, file: FileIdx, line: DiffLineIdx) -> bool {
        self.diff_lines(FileOwner::Dag(entry), file)
            .and_then(|lines| lines.get(line.raw()))
            .is_some_and(|dl| dl.is_selectable())
    }

    /// Check if a diff line is in the visual range (active or persistent).
    pub fn is_in_visual_range(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
        row_idx: RowIdx,
    ) -> bool {
        if let Some(VisualMode::Lines { anchor }) = &self.visual.mode {
            let lo = (*anchor).min(self.cursor);
            let hi = (*anchor).max(self.cursor);
            if row_idx >= lo && row_idx <= hi {
                return true;
            }
        }
        self.in_persistent_line_range(entry_idx, file_idx, line_idx)
    }

    fn in_persistent_line_range(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) -> bool {
        let Some(PersistentVisualRange::Lines(vr)) = &self.visual.persistent else {
            return false;
        };
        let node = &self.dag.nodes[entry_idx];
        node.files
            .files()
            .and_then(|files| files.get(file_idx.raw()))
            .is_some_and(|file| {
                node.commit.unique_change_id() == vr.change_id
                    && file.path == vr.path
                    && line_idx >= vr.start_line
                    && line_idx <= vr.end_line
            })
    }

    /// Check if a commit is in the visual range (active or persistent).
    pub fn is_in_visual_commit_range(&self, entry_idx: EntryIdx) -> bool {
        let active = match self.visual.mode {
            Some(VisualMode::Commits { anchor, head }) => Some(ordered(anchor, head)),
            _ => None,
        };
        let persistent = match self.visual.persistent {
            Some(PersistentVisualRange::Commits { lo, hi }) => Some((lo, hi)),
            _ => None,
        };
        [active, persistent]
            .into_iter()
            .flatten()
            .any(|(lo, hi)| (lo..=hi).contains(&entry_idx))
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
                owner: FileOwner::Dag(entry_idx),
                file_idx,
                line_idx,
            }) = self.rows.get(idx)
            {
                if start_line.is_none() {
                    start_line = Some(*line_idx);
                    change_id = Some(self.dag.nodes[*entry_idx].commit.unique_change_id());
                    path = self.dag.nodes[*entry_idx]
                        .files
                        .files()
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
            .dag
            .nodes
            .iter()
            .filter_map(|node| {
                let cid = node.commit.unique_change_id();
                if cid != vr.change_id {
                    return None;
                }
                let fi = node.files.file_idx(&vr.path)?;
                let diff_lines = node.files.diff_lines(fi, crate::dag::DiffFormat::Git)?;
                let lines: Vec<_> = diff_lines
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| {
                        let idx = DiffLineIdx::new(*i);
                        idx >= vr.start_line && idx <= vr.end_line
                    })
                    .filter(|(_, dl)| dl.is_selectable())
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
            if !self.begin_line_selection(&vr.change_id) {
                return;
            }
            for s in &line_data {
                if let Selection::Line { file_ref, .. } = s {
                    self.selection.remove(&Selection::File(file_ref.clone()));
                }
            }
            self.selection.extend(line_data);
        }
    }

    /// Toggle all commits in the persistent visual commit range as explicit selections.
    fn toggle_commit_visual_selection(&mut self) {
        let Some(PersistentVisualRange::Commits { lo, hi }) = self.visual.persistent else {
            return;
        };
        let selections: Vec<Selection> = (lo.raw()..=hi.raw())
            .map(|i| Selection::Commit(self.dag.nodes[EntryIdx::new(i)].commit.unique_change_id()))
            .collect();

        self.selection.ensure_compatible(SelectionKind::Commit);
        if selections.iter().all(|s| self.selection.contains(s)) {
            for s in &selections {
                self.selection.remove(s);
            }
        } else {
            self.selection.extend(selections);
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
                owner: FileOwner::Dag(ei),
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

        let Some(files) = self.dag.nodes[entry_idx].files.files() else {
            return;
        };
        let change_id = self.dag.nodes[entry_idx].commit.unique_change_id();
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
            self.selection.ensure_compatible(SelectionKind::File);
            for fr in file_refs {
                // File overrides Lines within that file, same as toggling one
                // directly; other files' line selections are left alone.
                self.selection
                    .retain(|s| !matches!(s, Selection::Line { file_ref, .. } if *file_ref == fr));
                self.selection.insert(Selection::File(fr));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{App, DagNode, FileFoldKey};
    use crate::dag::{
        CommitInfo, DiffLine, DiffLineKind, DiffResult, DiffSummary, FileChange, FileStatus,
        LineStats,
    };
    use crate::graph::GraphLines;
    use crate::idx::{EntryIdx, FileIdx, RowIdx};
    use crate::keymap::AppAction;
    use crate::types::{DisplayRow, FileOwner, RepoPath, SmallVec};

    fn commit(tag: char) -> CommitInfo {
        CommitInfo::for_test(
            &format!("{tag}{tag}nnomkxrqvlypszwlwkvvqnstvzoxrs"),
            &format!("{tag}{tag}baa2cb1f0e4d3a9c8b7a6e5d4c3b2a19087654"),
        )
    }

    fn file(path: &str) -> FileChange {
        FileChange {
            path: RepoPath::new(path),
            old_path: None,
            status: FileStatus::Modified,
            has_conflict: false,
            baseline_conflicted: false,
            is_submodule: false,
            stats: LineStats::default(),
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

    /// Commits `a`, `b`, `c`; `a` is unfolded to files `x`, `y`, `z`, and
    /// `y` to its diff.
    fn app() -> App {
        let mut app = App::for_test();
        for tag in ['a', 'b', 'c'] {
            app.dag.nodes.push(DagNode::new(
                commit(tag),
                GraphLines::default(),
                SmallVec::new(),
            ));
        }
        let a = EntryIdx::new(0);
        let paths = ["x", "y", "z"];
        app.dag.nodes[a].files.set_summary(Ok(DiffSummary {
            files: paths.iter().map(|p| file(p)).collect(),
            stats: LineStats::default(),
        }));
        for path in paths {
            app.dag.nodes[a]
                .files
                .set_diff(&RepoPath::new(path), Ok(diff()));
        }
        app.dag.unfolded_commits.insert(app.change_id(a));
        app.dag.unfolded_files.insert(FileFoldKey {
            change_id: app.change_id(a),
            path: RepoPath::new("y"),
        });
        app.rebuild_rows();
        app
    }

    fn row_where(app: &App, pred: impl Fn(&DisplayRow) -> bool) -> RowIdx {
        RowIdx::new(app.rows.iter().position(pred).expect("row is displayed"))
    }

    fn diff_row(app: &App, line: usize) -> RowIdx {
        row_where(
            app,
            |r| matches!(r, DisplayRow::DiffLine { line_idx, .. } if line_idx.raw() == line),
        )
    }

    fn file_row(app: &App, file: usize) -> RowIdx {
        row_where(
            app,
            |r| matches!(r, DisplayRow::FileChange { owner: FileOwner::Dag(_), file_idx } if *file_idx == FileIdx::new(file)),
        )
    }

    #[test]
    fn commit_visual_extends_then_shrinks_back_toward_the_anchor() {
        let mut app = app();
        app.cursor = app.row_of_commit(EntryIdx::new(1)).unwrap();
        app.enter_visual_mode();

        app.visual_move(AppAction::MoveDown, 10);
        assert!(app.is_in_visual_commit_range(EntryIdx::new(2)));
        assert_eq!(app.cursor, app.row_of_commit(EntryIdx::new(2)).unwrap());

        app.visual_move(AppAction::MoveUp, 10);
        app.visual_move(AppAction::MoveUp, 10);
        assert!(app.is_in_visual_commit_range(EntryIdx::new(0)));
        assert!(!app.is_in_visual_commit_range(EntryIdx::new(2)));
        assert_eq!(app.cursor, app.row_of_commit(EntryIdx::new(0)).unwrap());
    }

    #[test]
    fn line_visual_stays_on_the_selectable_lines_of_its_file() {
        let mut app = app();
        app.cursor = diff_row(&app, 1);
        app.enter_visual_mode();

        app.visual_move(AppAction::MoveDown, 10);
        assert_eq!(app.cursor, diff_row(&app, 2));
        app.visual_move(AppAction::MoveDown, 10);
        assert_eq!(app.cursor, diff_row(&app, 2), "the next file ends the run");

        app.visual_move(AppAction::MoveUp, 10);
        app.visual_move(AppAction::MoveUp, 10);
        assert_eq!(app.cursor, diff_row(&app, 1), "the header is passed over");
    }

    #[test]
    fn a_jump_out_of_the_run_is_pulled_back_to_its_end() {
        let mut app = app();
        app.cursor = diff_row(&app, 1);
        app.enter_visual_mode();

        app.visual_move(AppAction::MoveToBottom, 10);
        assert_eq!(app.cursor, diff_row(&app, 2));

        app.visual_move(AppAction::MoveToTop, 10);
        assert_eq!(app.cursor, diff_row(&app, 1));
    }

    #[test]
    fn file_visual_moves_between_folded_files_and_stops_at_a_diff() {
        let mut app = app();
        app.cursor = file_row(&app, 0);
        app.enter_visual_mode();

        app.visual_move(AppAction::MoveDown, 10);
        assert_eq!(app.cursor, file_row(&app, 1));
        app.visual_move(AppAction::MoveDown, 10);
        assert_eq!(app.cursor, file_row(&app, 1), "y's diff ends the run");

        app.exit_visual_mode();
        assert!(app.is_in_visual_file_range(EntryIdx::new(0), FileIdx::new(0)));
        assert!(app.is_in_visual_file_range(EntryIdx::new(0), FileIdx::new(1)));
        assert!(!app.is_in_visual_file_range(EntryIdx::new(0), FileIdx::new(2)));
    }
}
