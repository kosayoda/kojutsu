//! Where the DAG cursor lands once a reload brings back the commits it
//! names.
//!
//! There is one slot. A reload fills it with an anchor on the revision the
//! cursor was on; a command that says where to go replaces that with a jump;
//! moving the cursor yourself empties it. A target waits for the load it is
//! for, and for the DAG to be on screen. Whatever is in it is resolved as
//! far as the loaded data allows after each batch of repo results, so a
//! commit arriving in a later chunk, or a rewritten commit's files arriving
//! after it, still land the cursor where it belongs.

use super::carry::{Revision, Whereabouts};
use super::{App, JumpTarget, Loadable};
use crate::idx::{DiffLineIdx, EntryIdx};
use crate::types::{ActiveView, DisplayRow, FileOwner, RepoPath};

/// How many commits on each side of the cursor are remembered as fallbacks
/// for when its own commit doesn't survive a reload.
const NEIGHBOURS: usize = 16;

/// Where the DAG cursor should go once the DAG has loaded.
pub(crate) enum CursorTarget {
    /// Back to the revision the cursor was on before a reload.
    Anchor(Anchor),
    /// Where a command said the cursor should go.
    Jump {
        jump: JumpTarget,
        /// A bookmark jump made in the bookmark view first selects the
        /// bookmark there, once, and is a DAG position after that.
        bookmark_selected: bool,
    },
}

impl CursorTarget {
    /// The view whose cursor this moves while `active` is on screen.
    fn lands_in(&self, active: ActiveView) -> ActiveView {
        if self.selects_bookmark(active) {
            ActiveView::Bookmarks
        } else {
            ActiveView::Dag
        }
    }

    fn selects_bookmark(&self, active: ActiveView) -> bool {
        matches!(
            self,
            Self::Jump {
                jump: JumpTarget::Bookmark(_),
                bookmark_selected: false,
            }
        ) && active == ActiveView::Bookmarks
    }
}

/// The slot's content.
pub(crate) struct PendingCursor {
    target: CursorTarget,
    /// The DAG load it is for. A jump is set before the load that brings
    /// its commit back, and must not land on the DAG that load replaces.
    load: u64,
    /// Tells this target from one set in its place.
    id: u64,
}

/// The cursor's position before a reload.
pub(crate) struct Anchor {
    revision: Revision,
    /// Whether it was on `@`. Then it follows `@`, even to another change:
    /// squashing or abandoning `@` gives jj a new working-copy commit, and
    /// that is where someone working at `@` still is.
    on_working_copy: bool,
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
        let target = CursorTarget::Jump {
            jump: target,
            bookmark_selected: false,
        };
        self.set_pending_cursor(target, self.dag.loads + 1);
    }

    /// Send the DAG cursor to `target` in the DAG as loaded now, replacing
    /// whatever was pending: at once if the DAG is on screen, otherwise on
    /// entering it.
    pub fn jump_now(&mut self, target: JumpTarget) {
        let target = CursorTarget::Jump {
            jump: target,
            bookmark_selected: false,
        };
        self.set_pending_cursor(target, self.dag.loads);
        self.settle_pending_cursor();
    }

    fn set_pending_cursor(&mut self, target: CursorTarget, load: u64) {
        self.dag.cursor_targets_set += 1;
        self.dag.pending_cursor = Some(PendingCursor {
            target,
            load,
            id: self.dag.cursor_targets_set,
        });
    }

    /// Run `f` as something the user did. If it moves the cursor in the
    /// view the pending target lands in, the target is dropped rather than
    /// applied later over the user's own move.
    pub(crate) fn as_user_input<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let pending = self
            .dag
            .pending_cursor
            .as_ref()
            .map(|p| (p.id, p.target.lands_in(self.active_view)));
        let (cursor, view) = (self.cursor, self.active_view);
        let result = f(self);
        if let Some((id, lands_in)) = pending
            && self.active_view == view
            && lands_in == view
            && self.cursor != cursor
            && self.dag.pending_cursor.as_ref().is_some_and(|p| p.id == id)
        {
            self.dag.pending_cursor = None;
        }
        result
    }

    /// A load that failed brings nothing a pending jump was waiting for.
    /// Left in place, it would fire on some unrelated later load.
    pub(super) fn drop_cursor_target_for_failed_load(&mut self) {
        if self
            .dag
            .pending_cursor
            .as_ref()
            .is_some_and(|p| p.load > self.dag.loads)
        {
            self.dag.pending_cursor = None;
        }
    }

    /// Anchor the DAG cursor to its revision as a reload starts replacing
    /// the nodes its rows point into.
    pub(super) fn anchor_for_reload(&mut self) {
        self.anchor_dag_cursor(self.dag.loads);
    }

    /// The DAG is leaving the screen. Its rows are about to be replaced by
    /// another view's, so anchor its cursor while they are still here, for
    /// the next reload to bring back to.
    pub(super) fn on_dag_hidden(&mut self) {
        self.anchor_dag_cursor(self.dag.loads + 1);
    }

    /// The DAG is back on screen with its saved row restored. If nothing
    /// reloaded meanwhile, that row is still exactly right, and more exact
    /// than an anchor can be (a description or graph line, say), so the
    /// anchor left on the way out is dropped. Then whatever is pending lands.
    pub(super) fn on_dag_shown(&mut self) {
        if self
            .dag
            .pending_cursor
            .as_ref()
            .is_some_and(|p| matches!(p.target, CursorTarget::Anchor(_)) && p.load > self.dag.loads)
        {
            self.dag.pending_cursor = None;
        }
        self.settle_pending_cursor();
    }

    /// Anchor the DAG cursor for `load`. A target already pending is kept:
    /// it is either a jump, which wins, or an anchor from before, which is
    /// still where the user was.
    fn anchor_dag_cursor(&mut self, load: u64) {
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
        let entries: Vec<EntryIdx> = std::iter::once(at)
            .chain(below)
            .chain(above)
            .map(EntryIdx::new)
            .collect();
        let mut revisions = self.revisions_at(&entries).into_iter();
        let revision = revisions.next().expect("the cursor's own entry");
        let anchor = Anchor {
            revision,
            on_working_copy: self.dag.nodes[entry].commit.is_working_copy(),
            file,
            line,
            neighbours: revisions.collect(),
        };
        self.set_pending_cursor(CursorTarget::Anchor(anchor), load);
    }

    /// Move the cursor to the pending target as far as what has loaded
    /// allows, and clear it once nothing more can arrive to improve on that.
    /// Runs after each batch of repo results, once rows are rebuilt, and on
    /// entering the DAG.
    pub(super) fn settle_pending_cursor(&mut self) {
        let Some(mut pending) = self.dag.pending_cursor.take() else {
            return;
        };
        if pending.load > self.dag.loads {
            self.dag.pending_cursor = Some(pending);
            return;
        }
        let complete = self.dag.stream.is_none();
        let selects_bookmark = pending.target.selects_bookmark(self.active_view);
        let settled = match &mut pending.target {
            CursorTarget::Jump {
                jump: JumpTarget::Bookmark(name),
                bookmark_selected,
            } if selects_bookmark => {
                if self.select_bookmark_row(name) {
                    // Kept as where the DAG goes when next on screen.
                    *bookmark_selected = true;
                    false
                } else {
                    self.jump_missed(complete)
                }
            }
            CursorTarget::Jump { jump, .. } => self.settle_jump(jump, complete),
            // The anchor is into DAG rows, which only exist while the DAG
            // is on screen.
            CursorTarget::Anchor(_) if self.active_view != ActiveView::Dag => false,
            CursorTarget::Anchor(anchor) => self.settle_anchor(anchor, complete),
        };
        if !settled {
            self.dag.pending_cursor = Some(pending);
        }
    }

    fn settle_jump(&mut self, jump: &JumpTarget, complete: bool) -> bool {
        let Some(entry) = self.jump_entry(jump) else {
            return self.jump_missed(complete);
        };
        // Found, but placed only once the DAG is on screen to place it in.
        if self.active_view != ActiveView::Dag {
            return false;
        }
        if let Some(row) = self.row_of_commit(entry) {
            self.cursor = row;
        }
        true
    }

    /// A jump whose commit hasn't turned up: settled, with a word to the
    /// user, once there is nothing left to arrive.
    fn jump_missed(&mut self, complete: bool) -> bool {
        if complete {
            self.set_status("jump target not in current revset");
        }
        complete
    }

    /// The DAG entry a jump names. IDs match on their whole length, so a
    /// prefix of any length works, not just one short enough to display.
    pub(crate) fn jump_entry(&self, jump: &JumpTarget) -> Option<EntryIdx> {
        self.dag
            .nodes
            .iter_enumerated()
            .find(|(_, node)| {
                let commit = &node.commit;
                match jump {
                    JumpTarget::WorkingCopy => commit.is_working_copy(),
                    JumpTarget::Bookmark(name) => commit.bookmarks.iter().any(|b| b.name == *name),
                    JumpTarget::Revision(revision) => {
                        let (prefix, offset) = match revision.as_str().split_once('/') {
                            Some((prefix, offset)) => (prefix, offset.parse::<usize>().ok()),
                            None => (revision.as_str(), None),
                        };
                        (commit.change_id.full().starts_with(prefix)
                            || commit.graph_id.as_str().starts_with(prefix))
                            // The offset picks one copy of a divergent change,
                            // once the background pass has said which is which.
                            && offset.is_none_or(|offset| {
                                commit.change_id_suffix().is_none_or(|s| s == offset)
                            })
                    }
                }
            })
            .map(|(idx, _)| idx)
    }

    /// Place the cursor on the anchor's commit, then its file, then its
    /// line, stopping where the data isn't in yet. Returns whether it is
    /// settled.
    fn settle_anchor(&mut self, anchor: &Anchor, complete: bool) -> bool {
        let working_copy = if anchor.on_working_copy {
            let entry = self.jump_entry(&JumpTarget::WorkingCopy);
            // A revset can leave `@` out: only once the stream is complete
            // is it known to be missing.
            if entry.is_none() && !complete {
                return false;
            }
            entry
        } else {
            None
        };
        let entry = match working_copy {
            Some(entry) => entry,
            None => match self.locate(&anchor.revision, complete) {
                Whereabouts::Here(entry) => entry,
                // The cursor can only be on one of the copies.
                Whereabouts::Rewritten(copies) => copies[0],
                Whereabouts::Pending => return false,
                Whereabouts::Gone => {
                    let survivor =
                        anchor
                            .neighbours
                            .iter()
                            .find_map(|n| match self.locate(n, true) {
                                Whereabouts::Here(entry) => Some(entry),
                                Whereabouts::Rewritten(copies) => Some(copies[0]),
                                Whereabouts::Pending | Whereabouts::Gone => None,
                            });
                    if let Some(row) = survivor.and_then(|e| self.row_of_commit(e)) {
                        self.cursor = row;
                    }
                    return true;
                }
            },
        };
        if let Some(row) = self.row_of_commit(entry) {
            self.cursor = row;
        }

        // The file and line are the old commit's, so they carry over only
        // to the same change.
        if self.dag.nodes[entry].commit.change_id.change_id() != anchor.revision.change_id {
            return true;
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
        let Some(row) = self.position_of(DisplayRow::FileChange { owner, file_idx }) else {
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
                self.position_of(DisplayRow::DiffLine {
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
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{change_id, chunk, entry, load};
    use super::super::{App, FileFoldKey};
    use crate::dag::DiffTarget;
    use crate::dag::{
        DagEntry, DiffLine, DiffLineKind, DiffResult, DiffSummary, DivergenceInfo, FileChange,
        LineStats,
    };
    use crate::idx::{EntryIdx, RowIdx};
    use crate::repo_service::RepoResult;
    use crate::types::{ActiveView, CommitId, DisplayRow, JumpTarget, RepoPath, RevisionArg};

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

    /// The cursor's numbers `(old, new)` on the diff line it is on.
    fn cursor_line_numbers(app: &App) -> (Option<u32>, Option<u32>) {
        let DisplayRow::DiffLine {
            owner,
            file_idx,
            line_idx,
        } = app.cursor_row().expect("cursor on a row")
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
        let commit = app.commit_id(b).clone();
        app.dag.unfolded_commits.insert(commit.clone());
        app.dag.unfolded_files.insert(FileFoldKey {
            commit_id: commit,
            path: path.clone(),
        });
        app.rebuild_rows();
        app.cursor = RowIdx::new(app.rows.len() - 1);
        assert_eq!(cursor_line_numbers(&app), (None, Some(2)));

        load(&mut app, vec![entry('a', "a1"), entry('b', "b2")], true);
        assert_eq!(cursor_commit(&app), "b2");
        assert!(matches!(
            app.cursor_row().expect("cursor on a row"),
            DisplayRow::CommitNode { .. }
        ));

        let target = DiffTarget::Commit(CommitId::new("b2"));
        app.handle_repo_result(RepoResult::DiffSummary {
            target: target.clone(),
            result: Ok(summary()),
        });
        assert!(matches!(
            app.cursor_row().expect("cursor on a row"),
            DisplayRow::FileChange { .. }
        ));

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

        app.set_jump_target(JumpTarget::Revision(RevisionArg::new("a1")));
        app.refresh(crate::repo_service::RevsetLoadKind::NoSnapshot);
        load(&mut app, vec![entry('a', "a1")], false);
        chunk(&mut app, vec![entry('b', "b1")], true);

        assert_eq!(cursor_commit(&app), "a1");
    }

    /// Shorter than the displayed form, exactly it, and longer than it: the
    /// last only works because the whole ID is matched.
    #[test]
    fn a_prefix_of_any_length_names_the_commit() {
        let mut app = App::for_test();
        let change = change_id('u');
        load(&mut app, vec![entry('u', "7bbaa2cb1f0e4d3a9c8b7a")], true);
        let found = |prefix: &str| {
            app.jump_entry(&JumpTarget::Revision(RevisionArg::new(prefix)))
                .is_some()
        };

        for prefix in ["uu", "uunnomkx", "uunnomkxrqvlyp", change.as_str()] {
            assert!(found(prefix), "change prefix {prefix:?}");
        }
        for prefix in ["7b", "7bbaa2cb", "7bbaa2cb1f0e4d3a9c8b7a"] {
            assert!(found(prefix), "commit prefix {prefix:?}");
        }
        assert!(!found("zzzz"));
    }

    fn with_bookmark(mut entry: DagEntry, name: &str) -> DagEntry {
        entry.commit.bookmarks.push(crate::dag::BookmarkInfo {
            name: crate::types::BookmarkName::new(name),
            is_dirty: false,
            is_tracking: false,
            is_conflicted: false,
        });
        entry
    }

    /// A jump names a commit the coming load brings. Settling it against
    /// the DAG still on screen (here, on an unrelated batch of results)
    /// would report it missing and drop it before that load arrives.
    #[test]
    fn a_jump_waits_for_the_load_it_was_set_for() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        put_cursor_on(&mut app, "b1");

        app.set_jump_target(JumpTarget::Revision(RevisionArg::new("c1")));
        app.apply_deferred(Default::default());
        assert_eq!(cursor_commit(&app), "b1");

        let entries = vec![entry('a', "a1"), entry('b', "b1"), entry('c', "c1")];
        load(&mut app, entries, true);
        assert_eq!(cursor_commit(&app), "c1");
        assert!(app.status_message.is_none());
    }

    /// A jump that lands while another view is on screen is a DAG position:
    /// placed on entering the DAG, and not undone by moving around in the
    /// other view meanwhile.
    #[test]
    fn a_jump_from_another_view_is_placed_on_entering_the_dag() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        put_cursor_on(&mut app, "b1");
        app.switch_view(ActiveView::Tags);

        app.set_jump_target(JumpTarget::Revision(RevisionArg::new("a1")));
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        app.as_user_input(|app| app.cursor = RowIdx::new(1));
        app.as_user_input(|app| app.switch_view(ActiveView::Dag));

        assert_eq!(cursor_commit(&app), "a1");
    }

    /// A bookmark command run from the bookmark view keeps that view on
    /// the bookmark it acted on, wherever the reload lists it, and the DAG
    /// goes to its commit when next on screen.
    #[test]
    fn a_bookmark_jump_in_the_bookmark_view_selects_the_bookmark() {
        let mut app = App::for_test();
        let entries = || {
            vec![
                with_bookmark(entry('a', "a1"), "main"),
                with_bookmark(entry('b', "b1"), "dev"),
            ]
        };
        load(&mut app, entries(), true);
        app.switch_view(ActiveView::Bookmarks);
        let bookmark_at_cursor = |app: &App| {
            let DisplayRow::BookmarkItem { bookmark_idx } =
                app.cursor_row().expect("cursor on a row")
            else {
                panic!("cursor not on a bookmark");
            };
            app.views.bookmark_entries[bookmark_idx.raw()]
                .name
                .as_str()
                .to_string()
        };
        // Whichever the cursor isn't on, and with the DAG on the other
        // commit, so landing in either place shows the jump.
        let (other, other_commit, dag_commit) = if bookmark_at_cursor(&app) == "main" {
            ("dev", "b1", "a1")
        } else {
            ("main", "a1", "b1")
        };
        app.switch_view(ActiveView::Dag);
        put_cursor_on(&mut app, dag_commit);
        app.switch_view(ActiveView::Bookmarks);

        app.set_jump_target(JumpTarget::Bookmark(crate::types::BookmarkName::new(other)));
        load(&mut app, entries(), true);
        assert_eq!(bookmark_at_cursor(&app), other);

        app.switch_view(ActiveView::Dag);
        assert_eq!(cursor_commit(&app), other_commit);
    }

    /// Leaving the DAG anchors its cursor, so a reload while another view
    /// is on screen still brings it back to the same revision rather than
    /// the same row number.
    #[test]
    fn a_reload_while_away_returns_to_the_same_revision() {
        let mut app = App::for_test();
        let entries = vec![entry('a', "a1"), entry('b', "b1"), entry('c', "c1")];
        load(&mut app, entries, true);
        put_cursor_on(&mut app, "c1");
        app.switch_view(ActiveView::Tags);

        let entries = vec![
            entry('n', "n1"),
            entry('a', "a1"),
            entry('b', "b1"),
            entry('c', "c1"),
        ];
        load(&mut app, entries, true);
        app.switch_view(ActiveView::Dag);

        assert_eq!(cursor_commit(&app), "c1");
    }

    /// Without a reload the saved row is still right, and finer than an
    /// anchor: a description line would come back as its commit.
    #[test]
    fn returning_without_a_reload_keeps_the_exact_row() {
        let mut app = App::for_test();
        let mut described = entry('a', "a1");
        described.commit.full_description = Some("subject\nbody".into());
        load(&mut app, vec![described], true);
        let a = EntryIdx::new(0);
        app.dag.unfolded_commits.insert(app.commit_id(a).clone());
        app.rebuild_rows();
        app.cursor = app
            .rows
            .iter()
            .position(|r| matches!(r, DisplayRow::DescriptionLine { .. }))
            .map(RowIdx::new)
            .expect("a description line");

        app.switch_view(ActiveView::Tags);
        app.switch_view(ActiveView::Dag);

        assert!(matches!(
            app.cursor_row().expect("cursor on a row"),
            DisplayRow::DescriptionLine { .. }
        ));
    }

    /// A failed load brings nothing a jump was waiting for; the next load,
    /// whatever it is for, restores the cursor instead.
    #[test]
    fn a_failed_load_drops_the_jump_it_was_for() {
        use crate::repo_service::{RepoError, RepoErrorKind};

        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        put_cursor_on(&mut app, "b1");

        app.set_jump_target(JumpTarget::Revision(RevisionArg::new("a1")));
        app.handle_repo_result(RepoResult::Revset {
            revset: "bad(".into(),
            result: Err(RepoError::new(RepoErrorKind::Revset, "parse error")),
        });
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);

        assert_eq!(cursor_commit(&app), "b1");
    }

    fn working_copy(mut entry: DagEntry) -> DagEntry {
        entry
            .commit
            .workspaces
            .push(crate::dag::WorkspaceAnnotation {
                name: crate::types::WorkspaceName::new("default"),
                is_current: true,
            });
        entry
    }

    /// Squashing `@` into its parent leaves jj a new, empty working-copy
    /// commit of a new change. Someone who was at `@` is still at `@`.
    #[test]
    fn a_cursor_on_the_working_copy_follows_it_to_a_new_change() {
        let mut app = App::for_test();
        load(
            &mut app,
            vec![working_copy(entry('w', "w1")), entry('p', "p1")],
            true,
        );
        put_cursor_on(&mut app, "w1");

        load(
            &mut app,
            vec![working_copy(entry('x', "x1")), entry('p', "p2")],
            true,
        );

        assert_eq!(cursor_commit(&app), "x1");
    }

    /// Elsewhere, a new `@` is none of the cursor's business.
    #[test]
    fn a_cursor_elsewhere_stays_put_when_the_working_copy_moves() {
        let mut app = App::for_test();
        let entries = vec![working_copy(entry('w', "w1")), entry('b', "b1")];
        load(&mut app, entries, true);
        put_cursor_on(&mut app, "b1");

        let entries = vec![
            working_copy(entry('x', "x1")),
            entry('w', "w2"),
            entry('b', "b1"),
        ];
        load(&mut app, entries, true);

        assert_eq!(cursor_commit(&app), "b1");
    }

    /// A revision naming one copy of a divergent change picks that copy.
    #[test]
    fn a_revision_offset_picks_its_copy_of_a_divergent_change() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('u', "u1"), entry('u', "u2")], true);
        for (i, suffix) in [(0, 1), (1, 2)] {
            app.dag.nodes[EntryIdx::new(i)].commit.divergence = Some(DivergenceInfo {
                is_divergent: true,
                is_hidden: false,
                suffix: Some(suffix),
            });
        }

        let jump = JumpTarget::Revision(RevisionArg::new("uu/2"));
        assert_eq!(app.jump_entry(&jump), Some(EntryIdx::new(1)));
    }

    /// With several commits of the change about, a copy missing from the
    /// first chunk may yet arrive itself: the cursor waits for it rather
    /// than settling on its sibling.
    #[test]
    fn a_cursor_on_a_divergent_copy_waits_for_it() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('u', "u1"), entry('u', "u2")], true);
        put_cursor_on(&mut app, "u2");

        load(&mut app, vec![entry('u', "u1")], false);
        chunk(&mut app, vec![entry('u', "u2")], true);

        assert_eq!(cursor_commit(&app), "u2");
    }

    /// A plugin moving the DAG cursor from another view moves the DAG's,
    /// not the cursor of the view on screen.
    #[test]
    fn jumping_now_from_another_view_moves_only_the_dag() {
        let mut app = App::for_test();
        let entries = vec![entry('a', "a1"), entry('b', "b1"), entry('c', "c1")];
        load(&mut app, entries, true);
        app.switch_view(ActiveView::Tags);
        let tags_cursor = app.cursor;

        app.jump_now(JumpTarget::Revision(RevisionArg::new("c1")));
        assert_eq!(app.cursor, tags_cursor);

        app.switch_view(ActiveView::Dag);
        assert_eq!(cursor_commit(&app), "c1");
    }

    /// Showing a commit replaces whatever was pending, so a jump still
    /// waiting on a load can't pull the cursor off it afterwards.
    #[test]
    fn jumping_now_replaces_a_pending_target() {
        let mut app = App::for_test();
        let entries = || vec![entry('a', "a1"), entry('b', "b1"), entry('c', "c1")];
        load(&mut app, entries(), true);
        app.set_jump_target(JumpTarget::Revision(RevisionArg::new("a1")));

        app.jump_now(JumpTarget::Revision(RevisionArg::new("c1")));
        assert_eq!(cursor_commit(&app), "c1");
        load(&mut app, entries(), true);

        assert_eq!(cursor_commit(&app), "c1");
    }
}
