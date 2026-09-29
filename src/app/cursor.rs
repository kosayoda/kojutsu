//! Where the DAG cursor lands once a reload brings back the commits it
//! names.
//!
//! There is one slot. A reload fills it with an anchor on the revision the
//! cursor was on; a command that says where to go replaces that with a jump;
//! moving the cursor yourself empties it. Whatever is in it is resolved as
//! far as the loaded data allows after each batch of repo results, so a
//! commit arriving in a later chunk, or a rewritten commit's files arriving
//! after it, still land the cursor where it belongs.

use super::{App, JumpTarget, Loadable};
use crate::idx::{DiffLineIdx, EntryIdx, RowIdx};
use crate::types::{ActiveView, ChangeId, CommitId, DisplayRow, FileOwner, RepoPath};

/// How many commits on each side of the cursor are remembered as fallbacks
/// for when its own commit doesn't survive a reload.
const NEIGHBOURS: usize = 16;

/// Where the DAG cursor should go once the DAG has loaded.
pub(crate) enum CursorTarget {
    /// Back to the revision the cursor was on before a reload.
    Anchor(Anchor),
    /// Where a command said the cursor should go.
    Jump(JumpTarget),
}

/// A commit as a reload can find it again: by its commit ID while it is
/// unchanged, by its change ID once rewritten. The change ID is the bare
/// one; the divergence offset is computed after the commits load, so a key
/// carrying it would match nothing when it is needed.
struct Revision {
    commit_id: CommitId,
    change_id: ChangeId,
}

/// The cursor's position before a reload.
pub(crate) struct Anchor {
    revision: Revision,
    /// The file the cursor was in, if it was on a file or below one.
    file: Option<RepoPath>,
    /// The diff line it was on, by its old and new line numbers, which
    /// survive lines being added or removed elsewhere in the file.
    line: Option<(Option<u32>, Option<u32>)>,
    /// Commits to fall back on when this one is gone, nearest first: those
    /// below it (toward its parents), then those above.
    neighbours: Vec<Revision>,
}

impl App {
    /// Send the cursor somewhere once the DAG next loads, in place of
    /// whatever was pending.
    pub fn set_jump_target(&mut self, target: JumpTarget) {
        self.dag.pending_cursor = Some(CursorTarget::Jump(target));
    }

    /// Run `f` as something the user did. If it moves the cursor or changes
    /// view, the pending target is dropped rather than applied later over
    /// the user's own move. A target `f` sets itself is kept.
    pub(crate) fn as_user_input<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let pending = self.dag.pending_cursor.take();
        let (cursor, view) = (self.cursor, self.active_view);
        let result = f(self);
        if self.dag.pending_cursor.is_none() && self.cursor == cursor && self.active_view == view {
            self.dag.pending_cursor = pending;
        }
        result
    }

    /// Anchor the DAG cursor to its revision before a reload replaces the
    /// nodes its rows point into. A target already pending is kept: it is
    /// either a jump, which wins, or an anchor from a reload this one
    /// interrupted, which is still where the user was.
    pub(super) fn anchor_dag_cursor(&mut self) {
        if self.dag.pending_cursor.is_some() || self.active_view != ActiveView::Dag {
            return;
        }
        let Some(row) = self.rows.get(self.cursor.raw()).copied() else {
            return;
        };
        let Some(entry) = row.entry_idx() else {
            return;
        };
        let file = row
            .dag_file()
            .and_then(|(_, file_idx)| self.dag.nodes[entry].files.file(file_idx))
            .map(|file| file.path.clone());
        let line = match row {
            DisplayRow::DiffLine {
                owner,
                file_idx,
                line_idx,
            } => self
                .diff_lines(owner, file_idx)
                .and_then(|lines| lines.get(line_idx.raw()))
                .map(|dl| (dl.old_line, dl.new_line))
                // A header has no line numbers to find it again by.
                .filter(|numbers| *numbers != (None, None)),
            _ => None,
        };
        let at = entry.raw();
        let below = (at + 1)..self.dag.nodes.len().min(at + 1 + NEIGHBOURS);
        let above = (at.saturating_sub(NEIGHBOURS)..at).rev();
        let neighbours = below
            .chain(above)
            .map(|i| self.revision_at(EntryIdx::new(i)))
            .collect();
        self.dag.pending_cursor = Some(CursorTarget::Anchor(Anchor {
            revision: self.revision_at(entry),
            file,
            line,
            neighbours,
        }));
    }

    /// Move the cursor to the pending target as far as what has loaded
    /// allows, and clear it once nothing more can arrive to improve on that.
    /// Runs after each batch of repo results, once rows are rebuilt.
    pub(super) fn settle_pending_cursor(&mut self) {
        // Asked for but not started: the DAG is still the old one.
        if matches!(self.revset.load_state, Loadable::Loading) {
            return;
        }
        let Some(target) = self.dag.pending_cursor.take() else {
            return;
        };
        let complete = self.dag.stream.is_none();
        let settled = match &target {
            CursorTarget::Jump(jump) => self.settle_jump(jump, complete),
            // The anchor is into DAG rows, which only exist while the DAG
            // is on screen.
            CursorTarget::Anchor(_) if self.active_view != ActiveView::Dag => false,
            CursorTarget::Anchor(anchor) => self.settle_anchor(anchor, complete),
        };
        if !settled {
            self.dag.pending_cursor = Some(target);
        }
    }

    fn settle_jump(&mut self, jump: &JumpTarget, complete: bool) -> bool {
        let found = match jump {
            JumpTarget::WorkingCopy => self.jump_to_working_copy(),
            JumpTarget::Bookmark(name) => self.jump_to_bookmark(name),
            JumpTarget::Prefix(prefix) => self.jump_to_change_id(prefix),
        };
        if !found && complete {
            self.set_status("jump target not in current revset");
        }
        found || complete
    }

    /// Place the cursor on the anchor's commit, then its file, then its
    /// line, stopping where the data isn't in yet. Returns whether it is
    /// settled.
    fn settle_anchor(&mut self, anchor: &Anchor, complete: bool) -> bool {
        let Some(entry) = self.find_revision(&anchor.revision) else {
            // It may still arrive in a later chunk; only once the stream
            // is complete is it gone.
            if complete
                && let Some(row) = anchor
                    .neighbours
                    .iter()
                    .find_map(|n| self.find_revision(n))
                    .and_then(|e| self.row_of_commit(e))
            {
                self.cursor = row;
            }
            return complete;
        };
        if let Some(row) = self.row_of_commit(entry) {
            self.cursor = row;
        }

        let Some(path) = &anchor.file else {
            return true;
        };
        if !self.is_commit_unfolded(entry) {
            return true;
        }
        let files = &self.dag.nodes[entry].files;
        let file_idx = match files.summary() {
            // A rewritten commit's files are fetched again.
            Loadable::Loading => return false,
            Loadable::Loaded(_) => files.file_idx(path),
            Loadable::NotRequested | Loadable::Failed(_) => None,
        };
        let Some(file_idx) = file_idx else {
            return true;
        };
        let owner = FileOwner::Dag(entry);
        let Some(row) = self.find_row(DisplayRow::FileChange { owner, file_idx }) else {
            return true;
        };
        self.cursor = row;

        let Some(numbers) = anchor.line else {
            return true;
        };
        if !self.is_file_unfolded(owner, file_idx) {
            return true;
        }
        if matches!(files.diff(file_idx), Some(Loadable::Loading)) {
            return false;
        }
        let row = self
            .diff_lines(owner, file_idx)
            .and_then(|lines| {
                lines
                    .iter()
                    .position(|dl| (dl.old_line, dl.new_line) == numbers)
            })
            .and_then(|line_idx| {
                self.find_row(DisplayRow::DiffLine {
                    owner,
                    file_idx,
                    line_idx: DiffLineIdx::new(line_idx),
                })
            });
        if let Some(row) = row {
            self.cursor = row;
        }
        true
    }

    fn revision_at(&self, entry: EntryIdx) -> Revision {
        let commit = &self.dag.nodes[entry].commit;
        Revision {
            commit_id: commit.graph_id.clone(),
            change_id: commit.change_id.change_id(),
        }
    }

    /// The entry of `revision` itself if unchanged, or of its rewrite.
    fn find_revision(&self, revision: &Revision) -> Option<EntryIdx> {
        self.entry_by_commit_id(&revision.commit_id).or_else(|| {
            self.dag
                .nodes
                .iter_enumerated()
                .find(|(_, node)| node.commit.change_id.change_id() == revision.change_id)
                .map(|(idx, _)| idx)
        })
    }

    fn find_row(&self, row: DisplayRow) -> Option<RowIdx> {
        self.rows.iter().position(|r| *r == row).map(RowIdx::new)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{App, FileFoldKey};
    use crate::dag::DiffTarget;
    use crate::dag::{
        CommitInfo, DagEntry, DiffLine, DiffLineKind, DiffResult, DiffSummary, DivergenceInfo,
        FileChange, LineStats,
    };
    use crate::idx::{EntryIdx, RowIdx};
    use crate::repo_service::{RepoResult, RevsetData};
    use crate::types::{CommitId, DisplayRow, JumpTarget, RepoPath};

    fn change_id(tag: char) -> String {
        format!("{tag}{tag}nnomkxrqvlypszwlwkvvqnstvzoxrs")
    }

    /// Change `tag` as the commit `commit`: two entries with the same tag
    /// and different commits are one change before and after a rewrite.
    fn entry(tag: char, commit: &str) -> DagEntry {
        DagEntry {
            commit: CommitInfo::for_test(&change_id(tag), commit),
            edges: Vec::new(),
        }
    }

    fn load(app: &mut App, entries: Vec<DagEntry>, done: bool) {
        app.handle_repo_result(RepoResult::Revset {
            revset: "all()".into(),
            result: Ok(Box::new(RevsetData {
                revset: "all()".into(),
                repo_root: String::new(),
                entries,
                remote_bookmarks: Vec::new(),
                remotes: Vec::new(),
                all_tags: Vec::new(),
                tag_details: Default::default(),
                bookmark_details: Default::default(),
                workspace_entries: Vec::new(),
                warnings: Vec::new(),
                run_jobs: None,
                done,
            })),
        });
    }

    fn chunk(app: &mut App, entries: Vec<DagEntry>, done: bool) {
        app.handle_repo_result(RepoResult::RevsetChunk { entries, done });
    }

    fn cursor_commit(app: &App) -> String {
        let entry = app.selected_entry_idx().expect("cursor on a commit");
        app.dag.nodes[entry].commit.graph_id.as_str().to_string()
    }

    fn put_cursor_on(app: &mut App, commit: &str) {
        let entry = app.entry_by_commit_id(&CommitId::new(commit)).unwrap();
        app.cursor = app.row_of_commit(entry).unwrap();
    }

    fn line(kind: DiffLineKind, old_line: Option<u32>, new_line: Option<u32>) -> DiffLine {
        DiffLine {
            kind,
            content: "x".into(),
            tokens: vec![],
            old_line,
            new_line,
            conflict_region: false,
        }
    }

    fn diff(lines: Vec<DiffLine>) -> DiffResult {
        DiffResult {
            git: lines.clone(),
            color_words: lines,
        }
    }

    fn summary() -> DiffSummary {
        DiffSummary {
            files: vec![FileChange::for_test("f")],
            stats: LineStats::default(),
        }
    }

    fn cursor_row(app: &App) -> DisplayRow {
        app.rows[app.cursor.raw()]
    }

    /// The cursor's numbers `(old, new)` on the diff line it is on.
    fn cursor_line_numbers(app: &App) -> (Option<u32>, Option<u32>) {
        let DisplayRow::DiffLine {
            owner,
            file_idx,
            line_idx,
        } = cursor_row(app)
        else {
            panic!("cursor not on a diff line");
        };
        let dl = &app.diff_lines(owner, file_idx).unwrap()[line_idx.raw()];
        (dl.old_line, dl.new_line)
    }

    /// Rewriting the commit under the cursor gives it a new commit ID and
    /// fetches its files again. The cursor follows the change, then its
    /// file and line as each arrives, finding the line by its numbers after
    /// lines were added above it.
    #[test]
    fn a_rewritten_commit_gets_its_file_and_line_back_as_they_load() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        let b = EntryIdx::new(1);
        let path = RepoPath::new("f");
        let files = &mut app.dag.nodes[b].files;
        files.set_summary(Ok(summary()));
        files.set_diff(
            &path,
            Ok(diff(vec![
                line(DiffLineKind::Header, None, None),
                line(DiffLineKind::Added, None, Some(1)),
                line(DiffLineKind::Added, None, Some(2)),
            ])),
        );
        let change = app.change_id(b);
        app.dag.unfolded_commits.insert(change.clone());
        app.dag.unfolded_files.insert(FileFoldKey {
            change_id: change,
            path: path.clone(),
        });
        app.rebuild_rows();
        app.cursor = RowIdx::new(app.rows.len() - 1);
        assert_eq!(cursor_line_numbers(&app), (None, Some(2)));

        load(&mut app, vec![entry('a', "a1"), entry('b', "b2")], true);
        assert_eq!(cursor_commit(&app), "b2");
        assert!(matches!(cursor_row(&app), DisplayRow::CommitNode { .. }));

        let target = DiffTarget::Commit(CommitId::new("b2"));
        app.handle_repo_result(RepoResult::DiffSummary {
            target: target.clone(),
            result: Ok(summary()),
        });
        assert!(matches!(cursor_row(&app), DisplayRow::FileChange { .. }));

        app.handle_repo_result(RepoResult::FileDiff {
            target,
            path,
            result: Ok(diff(vec![
                line(DiffLineKind::Header, None, None),
                line(DiffLineKind::Removed, Some(1), None),
                line(DiffLineKind::Added, None, Some(1)),
                line(DiffLineKind::Added, None, Some(2)),
            ])),
        });
        assert_eq!(cursor_line_numbers(&app), (None, Some(2)));
    }

    /// Divergence offsets are computed after the commits load, so a key
    /// that carried one would never match the reloaded commit.
    #[test]
    fn a_divergent_commit_is_found_before_its_offset_is_known() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        app.dag.nodes[EntryIdx::new(1)].commit.divergence = Some(DivergenceInfo {
            is_divergent: true,
            is_hidden: false,
            suffix: Some(1),
        });
        put_cursor_on(&mut app, "b1");

        // A new commit on top, so the old index names the wrong one.
        let entries = vec![entry('n', "n1"), entry('a', "a1"), entry('b', "b2")];
        load(&mut app, entries, true);
        assert_eq!(cursor_commit(&app), "b2");
    }

    /// A commit that is gone leaves the cursor on its nearest survivor on
    /// the parents' side, where jj itself moves `@` when abandoning it.
    #[test]
    fn an_abandoned_commit_hands_the_cursor_to_its_neighbour_below() {
        let mut app = App::for_test();
        let entries = || vec![entry('a', "a1"), entry('b', "b1"), entry('c', "c1")];
        load(&mut app, entries(), true);
        put_cursor_on(&mut app, "b1");

        // A new commit on top as well, so the old index names the wrong one.
        load(
            &mut app,
            vec![entry('n', "n1"), entry('a', "a1"), entry('c', "c1")],
            true,
        );
        assert_eq!(cursor_commit(&app), "c1");
    }

    /// A commit deep enough to arrive in a later chunk is waited for, but
    /// not over the user's own move in the meantime.
    #[test]
    fn moving_while_loading_is_not_undone_by_a_later_chunk() {
        let mut app = App::for_test();
        let entries = vec![entry('a', "a1"), entry('b', "b1"), entry('c', "c1")];
        load(&mut app, entries, true);
        put_cursor_on(&mut app, "b1");

        load(&mut app, vec![entry('a', "a1"), entry('c', "c1")], false);
        assert_eq!(cursor_commit(&app), "c1", "held at the old row meanwhile");
        app.as_user_input(|app| put_cursor_on(app, "a1"));
        chunk(&mut app, vec![entry('b', "b1")], true);

        assert_eq!(cursor_commit(&app), "a1");
    }

    /// Without the move, the later chunk does bring the cursor back.
    #[test]
    fn a_commit_in_a_later_chunk_is_waited_for() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        put_cursor_on(&mut app, "b1");

        load(&mut app, vec![entry('a', "a1")], false);
        chunk(&mut app, vec![entry('b', "b1")], true);

        assert_eq!(cursor_commit(&app), "b1");
    }

    /// A command's jump replaces the anchor, so the cursor's old commit
    /// turning up in a later chunk doesn't pull it back.
    #[test]
    fn a_jump_is_not_overridden_by_the_old_position_arriving_later() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        put_cursor_on(&mut app, "b1");

        app.set_jump_target(JumpTarget::Prefix("a1".into()));
        app.refresh(crate::repo_service::RevsetLoadKind::NoSnapshot);
        load(&mut app, vec![entry('a', "a1")], false);
        chunk(&mut app, vec![entry('b', "b1")], true);

        assert_eq!(cursor_commit(&app), "a1");
    }
}
