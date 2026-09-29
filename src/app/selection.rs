use std::collections::HashSet;

use super::App;
use crate::dag::DiffLineKind;
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
use crate::types::FileOwner;
use crate::types::{
    ChangeId, FileRef, FileSelectionState, RepoPath, RevisionArg, Selection, SelectionKind,
    SmallVec,
};

impl App {
    /// Resolve entry + file indices to (change_id, file_path).
    /// Returns `None` if the file list isn't loaded yet.
    fn resolve_file(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> Option<(ChangeId, RepoPath)> {
        let change_id = self.dag.nodes[entry_idx].commit.unique_change_id();
        let files = self.dag.nodes[entry_idx].files.files()?;
        Some((change_id, files[file_idx.raw()].path.clone()))
    }

    /// The blocker naming a commit's files, or `None` when every change in
    /// it can be narrowed by a line selection.
    fn line_selection_blocker(&self, change_id: &ChangeId) -> Option<String> {
        let files = self
            .dag
            .nodes
            .iter()
            .find(|node| node.commit.unique_change_id() == *change_id)
            .and_then(|node| node.files.files())?;
        line_selection_blocker(files)
    }

    /// Enter line-selection mode for a commit, scoping the selection to it.
    /// Returns `false` (having changed nothing) when the commit holds a
    /// change no line selection can narrow, because a line-mode command
    /// would carry that change along whole while the UI implied otherwise.
    /// Whole-file selection takes a different route (a jj fileset rather
    /// than a diff editor) and handles those paths correctly, so that's
    /// what the message points at.
    pub(super) fn begin_line_selection(&mut self, change_id: &ChangeId) -> bool {
        if let Some(blocker) = self.line_selection_blocker(change_id) {
            self.set_error(format!(
                "{blocker}, so it comes along whichever lines you pick - select whole files instead"
            ));
            return false;
        }
        self.clear_other_commits(change_id);
        self.selection.ensure_compatible(SelectionKind::Line);
        true
    }

    /// Drop a commit's line selections once its freshly loaded file list
    /// turns out to hold a change no line selection can narrow.
    ///
    /// [`Self::begin_line_selection`] can only judge the files known at the
    /// time. A commit rewritten under an existing selection: squashing a
    /// submodule bump into it, say: keeps its change ID, so the selection
    /// survives the refresh and would otherwise go on to run in line mode
    /// against a commit that no longer supports it.
    pub(super) fn drop_blocked_line_selection(&mut self, entry_idx: EntryIdx) {
        if self.selection_kind() != SelectionKind::Line {
            return;
        }
        let change_id = self.dag.nodes[entry_idx].commit.unique_change_id();
        if !self.selection.any(|s| *s.change_id() == change_id) {
            return;
        }
        let Some(blocker) = self.line_selection_blocker(&change_id) else {
            return;
        };
        self.clear_selection();
        self.set_error(format!(
            "{blocker}, which a line selection can't exclude - dropped it, select whole files instead"
        ));
    }

    pub fn selection_active(&self) -> bool {
        self.selection.is_active()
    }

    pub fn selection_kind(&self) -> SelectionKind {
        self.selection.kind()
    }

    /// Every selection kind currently present: what action gating tests
    /// against, so a mixed selection needs an action supporting all of it.
    pub fn selection_kinds(&self) -> crate::keymap::SelectionKindSet {
        self.selection.kinds()
    }

    pub fn selection_summary(&self) -> &crate::types::SelectionSummary {
        self.selection.summary()
    }

    pub fn explicit_selection(&self) -> Option<&HashSet<Selection>> {
        self.selection.explicit()
    }

    /// Toggle file selection. If the file belongs to a different commit than
    /// existing selections, clears the old selections first (selections are
    /// scoped to one commit at a time).
    pub fn toggle_file_selection(&mut self, entry_idx: EntryIdx, file_idx: FileIdx) {
        let Some((change_id, path)) = self.resolve_file(entry_idx, file_idx) else {
            return;
        };

        self.clear_other_commits(&change_id);
        self.selection.ensure_compatible(SelectionKind::File);

        // Clear any line-level selections for this file (File overrides Lines).
        self.selection
            .retain(|s| !matches!(s, Selection::Line { file_ref: f, .. } if f.path == path));

        let sel = Selection::File(FileRef { change_id, path });
        self.selection.toggle(sel);
    }

    /// Toggle commit selection. Behavior depends on fold state:
    /// - Folded: toggle commit-level selection (multi-commit).
    /// - Unfolded: toggle all files in commit (select all / deselect all).
    pub fn toggle_commit_selection(&mut self, entry_idx: EntryIdx) {
        if self.is_commit_unfolded(entry_idx) {
            self.toggle_commit_file_selection(entry_idx);
        } else {
            let change_id = self.dag.nodes[entry_idx].commit.unique_change_id();
            self.selection.ensure_compatible(SelectionKind::Commit);
            self.selection.toggle(Selection::Commit(change_id));
        }
    }

    /// Check if a commit is in the explicit commit selection set.
    pub fn is_commit_selected(&self, entry_idx: EntryIdx) -> bool {
        let change_id = self.dag.nodes[entry_idx].commit.unique_change_id();
        self.selection.contains(&Selection::Commit(change_id))
    }

    /// Explicitly selected commits as revisions for `jj`, in DAG order, or the
    /// commit under the cursor when nothing is selected.
    ///
    /// Selections are keyed by whole change IDs so they survive prefix-length
    /// updates, but what goes to `jj` is the shortest unique prefix: the same
    /// form the cursor path uses, and the same form shown on screen.
    pub fn selected_change_ids(&self) -> SmallVec<RevisionArg> {
        if self.selection_kind() != SelectionKind::Commit || !self.selection_active() {
            return self.selected_change_id().into_iter().collect();
        }

        let mut pending: HashSet<ChangeId> = self
            .selection
            .iter()
            .filter_map(|s| match s {
                Selection::Commit(id) => Some(id.clone()),
                _ => None,
            })
            .collect();

        // Resolved by one walk of the DAG rather than a search per selection,
        // so each node's identity is built once and the revisions come out in
        // DAG order: the selection itself is a hash set, whose order would
        // otherwise vary between identical invocations.
        let mut revisions: SmallVec<RevisionArg> = self
            .dag
            .nodes
            .iter()
            .filter(|n| pending.remove(&n.commit.unique_change_id()))
            .map(|n| n.commit.unique_prefix())
            .collect();

        // A selection can outrun the DAG stream, since stale ones are only
        // pruned once a load completes. The commit exists either way, and its
        // whole change ID still resolves.
        revisions.extend(pending.into_iter().map(|id| RevisionArg::new(id.as_str())));
        revisions
    }

    /// Toggle all files in an unfolded commit (select all / deselect all).
    fn toggle_commit_file_selection(&mut self, entry_idx: EntryIdx) {
        if self.dag.nodes[entry_idx].files.files().is_none() {
            return;
        }

        let change_id = self.dag.nodes[entry_idx].commit.unique_change_id();
        self.clear_other_commits(&change_id);
        self.selection.ensure_compatible(SelectionKind::File);

        // Collect file paths upfront to avoid borrowing loaded file state across mutations.
        let file_paths: Vec<RepoPath> = self.dag.nodes[entry_idx]
            .files
            .files()
            .into_iter()
            .flatten()
            .map(|f| f.path.clone())
            .collect();

        // If all files are already selected (File-level), deselect all.
        let all_selected = file_paths.iter().all(|p| {
            self.selection.contains(&Selection::File(FileRef {
                change_id: change_id.clone(),
                path: p.clone(),
            }))
        });

        if all_selected {
            self.selection.clear();
        } else {
            self.selection.clear();
            for p in file_paths {
                self.selection.insert(Selection::File(FileRef {
                    change_id: change_id.clone(),
                    path: p,
                }));
            }
        }
    }

    /// Replace a whole-file selection with one entry per selectable line,
    /// leaving the same set of changes selected. A `Full` file already draws
    /// its lines as selected ([`Self::is_line_selected`]), so a line toggle
    /// inside one has to deselect that line, not discard the rest of the
    /// file, and it needs the lines to exist individually to do that.
    fn expand_file_selection_to_lines(&mut self, entry_idx: EntryIdx, file_idx: FileIdx) {
        let Some((change_id, path)) = self.resolve_file(entry_idx, file_idx) else {
            return;
        };
        let file_ref = FileRef {
            change_id: change_id.clone(),
            path,
        };
        if !self.selection.contains(&Selection::File(file_ref.clone())) {
            return;
        }

        let Some(diff_lines) = self.diff_lines(FileOwner::Dag(entry_idx), file_idx) else {
            return;
        };
        let lines: Vec<Selection> = diff_lines
            .iter()
            .filter(|dl| dl.is_selectable())
            .map(|dl| Selection::Line {
                file_ref: file_ref.clone(),
                old_line: dl.old_line,
                new_line: dl.new_line,
            })
            .collect();

        self.selection.remove(&Selection::File(file_ref));
        self.selection.extend(lines);
    }

    /// Toggle a single diff line selection (added/removed only).
    pub fn toggle_line_selection(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) {
        let Some((change_id, file_path)) = self.resolve_file(entry_idx, file_idx) else {
            return;
        };

        // Extract what we need from the diff line before mutating self.
        let Some(diff_lines) = self.diff_lines(FileOwner::Dag(entry_idx), file_idx) else {
            return;
        };
        let dl = &diff_lines[line_idx.raw()];
        if !dl.is_selectable() {
            return;
        }
        let old_line = dl.old_line;
        let new_line = dl.new_line;

        if !self.begin_line_selection(&change_id) {
            return;
        }
        self.expand_file_selection_to_lines(entry_idx, file_idx);

        let sel = Selection::Line {
            file_ref: FileRef {
                change_id,
                path: file_path,
            },
            old_line,
            new_line,
        };
        self.selection.toggle(sel);
    }

    /// Toggle all added/removed lines in a hunk (triggered by space on a header line).
    pub fn toggle_hunk_selection(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        header_line_idx: DiffLineIdx,
    ) {
        let Some((change_id, file_path)) = self.resolve_file(entry_idx, file_idx) else {
            return;
        };

        // Collect hunk line data before mutating self.
        let mut hunk_lines = Vec::new();
        {
            let Some(diff_lines) = self.diff_lines(FileOwner::Dag(entry_idx), file_idx) else {
                return;
            };
            for dl in diff_lines.iter().skip(header_line_idx.raw() + 1) {
                if dl.kind == DiffLineKind::Header {
                    break;
                }
                if dl.is_selectable() {
                    hunk_lines.push(Selection::Line {
                        file_ref: FileRef {
                            change_id: change_id.clone(),
                            path: file_path.clone(),
                        },
                        old_line: dl.old_line,
                        new_line: dl.new_line,
                    });
                }
            }
        }

        if !self.begin_line_selection(&change_id) {
            return;
        }
        self.expand_file_selection_to_lines(entry_idx, file_idx);

        // If all hunk lines are already selected, deselect them. Otherwise select all.
        let all_selected = hunk_lines.iter().all(|s| self.selection.contains(s));
        if all_selected {
            for s in &hunk_lines {
                self.selection.remove(s);
            }
        } else {
            self.selection.extend(hunk_lines);
        }
    }

    /// Check if a specific diff line is selected.
    pub fn is_line_selected(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) -> bool {
        let Some((change_id, file_path)) = self.resolve_file(entry_idx, file_idx) else {
            return false;
        };
        let Some(diff_lines) = self.diff_lines(FileOwner::Dag(entry_idx), file_idx) else {
            return false;
        };
        let diff_line = &diff_lines[line_idx.raw()];

        // If the whole file is selected, all lines are implicitly selected.
        if self.selection.contains(&Selection::File(FileRef {
            change_id: change_id.clone(),
            path: file_path.clone(),
        })) {
            return diff_line.is_selectable();
        }

        self.selection.contains(&Selection::Line {
            file_ref: FileRef {
                change_id,
                path: file_path,
            },
            old_line: diff_line.old_line,
            new_line: diff_line.new_line,
        })
    }

    /// Get the selection state of a file for UI display.
    pub fn file_selection_state(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> FileSelectionState {
        let Some((change_id, file_path)) = self.resolve_file(entry_idx, file_idx) else {
            return FileSelectionState::None;
        };

        // Explicit file-level selection.
        if self.selection.contains(&Selection::File(FileRef {
            change_id: change_id.clone(),
            path: file_path.clone(),
        })) {
            return FileSelectionState::Full;
        }

        // Check if any lines for this file are selected by scanning the diff
        // and testing membership, rather than scanning all selections.
        let Some(diff_lines) = self.diff_lines(FileOwner::Dag(entry_idx), file_idx) else {
            return FileSelectionState::None;
        };
        let file_ref = FileRef {
            change_id,
            path: file_path,
        };
        let mut selected_count = 0usize;
        let mut selectable_count = 0usize;
        for dl in diff_lines {
            if !dl.is_selectable() {
                continue;
            }
            selectable_count += 1;
            if self.selection.contains(&Selection::Line {
                file_ref: file_ref.clone(),
                old_line: dl.old_line,
                new_line: dl.new_line,
            }) {
                selected_count += 1;
            }
        }

        if selected_count == 0 {
            FileSelectionState::None
        } else if selectable_count > 0 && selected_count >= selectable_count {
            FileSelectionState::Full
        } else {
            FileSelectionState::Partial
        }
    }

    /// Check if there are any line-level selections (vs only file-level).
    pub fn has_line_selections(&self) -> bool {
        self.selection.any(|s| matches!(s, Selection::Line { .. }))
    }

    /// Get the unique file paths from all selections.
    pub fn selected_file_paths(&self) -> Vec<crate::types::Str> {
        let mut paths: Vec<crate::types::Str> = self
            .selection
            .iter()
            .filter_map(|s| s.path().map(crate::types::Str::from))
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths
    }

    /// Number of currently selected items.
    pub fn selection_count(&self) -> usize {
        self.selection.len()
    }

    /// Clear all selections.
    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Clear selections from other commits if switching to a different one.
    pub(crate) fn clear_other_commits(&mut self, change_id: &ChangeId) {
        if self.selection.is_active() {
            let same_commit = self.selection.any(|s| s.change_id() == change_id);
            if !same_commit {
                self.clear_selection();
            }
        }
    }
}

/// Names the first change in `files` that no line selection can narrow, and
/// how many others share the problem, or `None` when they all can be.
/// Phrased as the subject of a sentence the caller completes, so the reason
/// stays tied to [`FileChange::line_selection_blocker`] rather than being
/// restated at each call site.
fn line_selection_blocker(files: &[crate::dag::FileChange]) -> Option<String> {
    let mut blocked = files
        .iter()
        .filter_map(|f| Some((&f.path, f.line_selection_blocker()?)));
    let (path, reason) = blocked.next()?;
    let more = match blocked.count() {
        0 => String::new(),
        n => format!(" (+{n} more)"),
    };
    Some(format!("{}{more} is {reason}", path.as_str()))
}

#[cfg(test)]
mod change_id_key_tests {
    use super::super::{App, DagNode};
    use crate::dag::CommitInfo;
    use crate::graph::GraphLines;
    use crate::idx::EntryIdx;
    use crate::types::SmallVec;

    const CHANGE_ID: &str = "uunnomkxrqvlypszwlwkvvqnstvzoxrs";
    const COMMIT_ID: &str = "7bbaa2cb1f0e4d3a9c8b7a6e5d4c3b2a19087654";

    fn app_with_one_commit() -> App {
        let mut app = App::for_test();
        app.dag.nodes.push(DagNode::new(
            CommitInfo::for_test(CHANGE_ID, COMMIT_ID),
            GraphLines::default(),
            SmallVec::new(),
        ));
        app
    }

    #[test]
    fn a_selection_survives_the_background_prefix_update() {
        // Prefix lengths land asynchronously. If the selection key were derived
        // from the displayed prefix, a commit selected beforehand would silently
        // stop matching itself once a >8-char prefix arrived.
        let mut app = app_with_one_commit();
        let idx = EntryIdx::new(0);

        app.toggle_commit_selection(idx);
        assert!(app.is_commit_selected(idx));

        app.dag.nodes[idx].commit.change_id.set_prefix_len(12);
        assert!(app.is_commit_selected(idx));
    }

    #[test]
    fn a_selected_commit_reaches_jj_as_its_short_prefix() {
        // Selections are keyed by the whole change ID; commands must still get
        // the same short prefix the cursor path and the UI use.
        let mut app = app_with_one_commit();
        let idx = EntryIdx::new(0);
        app.dag.nodes[idx].commit.change_id.set_prefix_len(2);

        app.toggle_commit_selection(idx);

        let ids = app.selected_change_ids();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].as_str(), "uu");
    }
}

#[cfg(test)]
mod selected_revision_tests {
    use super::super::{App, DagNode};
    use crate::dag::CommitInfo;
    use crate::graph::GraphLines;
    use crate::idx::EntryIdx;
    use crate::types::{ChangeId, Selection, SmallVec};

    /// Distinct 32-char change IDs whose first two characters differ, so a
    /// short prefix identifies each one.
    fn change_id(tag: char) -> String {
        format!("{tag}{tag}nnomkxrqvlypszwlwkvvqnstvzoxrs")
    }

    fn commit_id(tag: char) -> String {
        format!("{tag}{tag}baa2cb1f0e4d3a9c8b7a6e5d4c3b2a19087654")
    }

    /// A DAG holding `tags` in that order, each with a 2-char unique prefix.
    fn app_with(tags: &[char]) -> App {
        let mut app = App::for_test();
        for &tag in tags {
            let mut commit = CommitInfo::for_test(&change_id(tag), &commit_id(tag));
            commit.change_id.set_prefix_len(2);
            app.dag
                .nodes
                .push(DagNode::new(commit, GraphLines::default(), SmallVec::new()));
        }
        app.rebuild_rows();
        app
    }

    fn revisions(app: &App) -> Vec<String> {
        app.selected_change_ids()
            .iter()
            .map(|r| r.as_str().to_string())
            .collect()
    }

    #[test]
    fn selected_commits_come_out_in_dag_order() {
        // The selection is a hash set, so without resolving against the DAG the
        // order handed to `jj` (and shown in the command preview) would vary
        // between identical invocations. Enough commits that hash order is not
        // going to coincide with DAG order by chance.
        let tags = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'];
        let mut app = app_with(&tags);
        // Selected back to front, skipping two.
        for i in [7, 5, 4, 2, 1, 0] {
            app.toggle_commit_selection(EntryIdx::new(i));
        }

        assert_eq!(revisions(&app), ["aa", "bb", "cc", "ee", "ff", "hh"]);
    }

    #[test]
    fn the_order_does_not_depend_on_how_the_selection_was_built() {
        let order_a = {
            let mut app = app_with(&['a', 'b', 'c']);
            for i in [0, 1, 2] {
                app.toggle_commit_selection(EntryIdx::new(i));
            }
            revisions(&app)
        };
        let order_b = {
            let mut app = app_with(&['a', 'b', 'c']);
            for i in [2, 1, 0] {
                app.toggle_commit_selection(EntryIdx::new(i));
            }
            revisions(&app)
        };

        assert_eq!(order_a, order_b);
    }

    #[test]
    fn a_selection_the_dag_has_not_loaded_yet_still_resolves() {
        // Stale selections are pruned only once a load completes, so one can
        // outrun the stream. Its whole change ID is still a valid revision.
        let mut app = app_with(&['a']);
        app.toggle_commit_selection(EntryIdx::new(0));
        app.selection
            .insert(Selection::Commit(ChangeId::new(change_id('z'))));

        let revs = revisions(&app);
        assert_eq!(revs.len(), 2);
        assert_eq!(revs[0], "aa");
        assert_eq!(revs[1], change_id('z'));
    }

    #[test]
    fn with_nothing_selected_the_cursor_commit_is_used() {
        let app = app_with(&['a', 'b']);
        assert_eq!(revisions(&app), ["aa"]);
    }
}

#[cfg(test)]
mod submodule_line_selection_tests {
    use super::super::{App, DagNode};
    use crate::dag::{
        CommitInfo, DiffLine, DiffLineKind, DiffResult, DiffSummary, FileChange, FileStatus,
        LineStats,
    };
    use crate::graph::GraphLines;
    use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
    use crate::types::{FileSelectionState, RepoPath, SelectionKind, SmallVec};

    const CHANGE_ID: &str = "uunnomkxrqvlypszwlwkvvqnstvzoxrs";
    const COMMIT_ID: &str = "7bbaa2cb1f0e4d3a9c8b7a6e5d4c3b2a19087654";

    fn file(path: &str, is_submodule: bool) -> FileChange {
        FileChange {
            path: RepoPath::new(path),
            old_path: None,
            status: FileStatus::Modified,
            has_conflict: false,
            baseline_conflicted: false,
            is_submodule,
            stats: LineStats::default(),
        }
    }

    /// One added and one removed line, the shape both a real file diff and a
    /// submodule's before/after description take.
    fn two_line_diff() -> DiffResult {
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

    /// A single commit whose files are `paths`, each with a two-line diff.
    fn app_with_files(paths: &[(&str, bool)]) -> App {
        let mut app = App::for_test();
        app.dag.nodes.push(DagNode::new(
            CommitInfo::for_test(CHANGE_ID, COMMIT_ID),
            GraphLines::default(),
            SmallVec::new(),
        ));
        let idx = EntryIdx::new(0);
        app.dag.nodes[idx].files.set_summary(Ok(DiffSummary {
            files: paths
                .iter()
                .map(|(path, is_submodule)| file(path, *is_submodule))
                .collect(),
            stats: LineStats::default(),
        }));
        for (path, _) in paths {
            app.dag.nodes[idx]
                .files
                .set_diff(&RepoPath::new(*path), Ok(two_line_diff()));
        }
        app
    }

    /// The removed line of the first file's diff.
    const REMOVED: DiffLineIdx = DiffLineIdx::new(1);

    #[test]
    fn a_submodules_lines_cannot_be_line_selected() {
        let mut app = app_with_files(&[("sub", true)]);

        app.toggle_line_selection(EntryIdx::new(0), FileIdx::new(0), REMOVED);

        assert!(!app.selection_active());
        assert!(app.status_message.is_some());
    }

    /// The real trap: the submodule isn't the file being selected. jj carries
    /// it along whole regardless, so offering line selection anywhere in this
    /// commit would misreport what the command is about to do.
    #[test]
    fn a_submodule_blocks_line_selection_of_its_neighbours() {
        let mut app = app_with_files(&[("readme", false), ("sub", true)]);

        app.toggle_line_selection(EntryIdx::new(0), FileIdx::new(0), REMOVED);

        assert!(!app.selection_active());
    }

    #[test]
    fn a_hunk_toggle_is_blocked_the_same_way() {
        let mut app = app_with_files(&[("readme", false), ("sub", true)]);

        app.toggle_hunk_selection(EntryIdx::new(0), FileIdx::new(0), DiffLineIdx::new(0));

        assert!(!app.selection_active());
    }

    /// Whole-file selection reaches jj as a fileset rather than a diff editor,
    /// which handles submodules correctly, so it stays available.
    #[test]
    fn whole_file_selection_still_works_on_a_submodule() {
        let mut app = app_with_files(&[("sub", true)]);
        let (entry, f) = (EntryIdx::new(0), FileIdx::new(0));

        app.toggle_file_selection(entry, f);

        assert_eq!(app.selection_kind(), SelectionKind::File);
        assert!(matches!(
            app.file_selection_state(entry, f),
            FileSelectionState::Full
        ));
    }

    /// Nothing about the guard should touch a commit without submodules.
    #[test]
    fn line_selection_is_untouched_without_a_submodule() {
        let mut app = app_with_files(&[("readme", false)]);

        app.toggle_line_selection(EntryIdx::new(0), FileIdx::new(0), REMOVED);

        assert_eq!(app.selection_kind(), SelectionKind::Line);
    }

    /// A commit rewritten under a live line selection keeps its change ID, so
    /// the selection survives the refresh. If the rewrite brought in a
    /// submodule, the selection is now unhonourable and has to go: checked
    /// when the new file list lands, the first moment that's knowable.
    #[test]
    fn a_rewrite_that_adds_a_submodule_drops_the_line_selection() {
        let mut app = app_with_files(&[("readme", false)]);
        let entry = EntryIdx::new(0);
        app.toggle_line_selection(entry, FileIdx::new(0), REMOVED);
        assert_eq!(app.selection_kind(), SelectionKind::Line);

        app.dag.nodes[entry].files.set_summary(Ok(DiffSummary {
            files: vec![file("readme", false), file("sub", true)],
            stats: LineStats::default(),
        }));
        app.drop_blocked_line_selection(entry);

        assert!(!app.selection_active());
    }

    /// The same reload without a submodule must leave the selection alone.
    #[test]
    fn a_reload_keeps_a_line_selection_it_can_still_honour() {
        let mut app = app_with_files(&[("readme", false)]);
        let entry = EntryIdx::new(0);
        app.toggle_line_selection(entry, FileIdx::new(0), REMOVED);

        app.drop_blocked_line_selection(entry);

        assert_eq!(app.selection_kind(), SelectionKind::Line);
    }

    /// Both blocked paths are reported, so the message doesn't imply the one
    /// it names is the only thing in the way.
    #[test]
    fn the_message_counts_every_blocked_path() {
        let files = [file("a", true), file("readme", false), file("b", true)];
        let blocker = super::line_selection_blocker(&files).unwrap();
        assert_eq!(blocker, "a (+1 more) is a Git submodule");
    }
}
