//! Carrying per-commit state across a reload.
//!
//! Folds and selections are keyed by commit ID, which names exactly one
//! commit, even among the copies of a divergent change, but changes when the
//! commit is rewritten. As a reload's commits arrive, each key whose commit
//! didn't come back moves to what replaced it: the new commits of the same
//! change. The change ID is taken without jj's divergence offset, which is
//! only a position among the change's commits and is renumbered by rewrites.

use std::collections::{HashMap, HashSet};

use super::{App, DagNode, FileFoldKey};
use crate::idx::EntryIdx;
use crate::types::{ChangeId, CommitId, Selection};

/// A commit from before a reload, as the reload can find it again.
pub(super) struct Revision {
    pub(super) commit_id: CommitId,
    pub(super) change_id: ChangeId,
    /// Whether its change had other commits too, so that one of those
    /// turning up says nothing about where this one went.
    pub(super) divergent: bool,
}

/// Where a commit from before a reload is now.
pub(super) enum Whereabouts {
    /// Unchanged.
    Here(EntryIdx),
    /// Replaced: these are the commits of its change, in DAG order.
    Rewritten(Vec<EntryIdx>),
    /// Not known until more of the reload arrives.
    Pending,
    /// Neither it nor a rewrite is in the complete reload.
    Gone,
}

impl App {
    /// Revisions for `entries`, each noting whether the DAG holds other
    /// commits of its change.
    pub(super) fn revisions_at(&self, entries: &[EntryIdx]) -> Vec<Revision> {
        let mut counts: HashMap<ChangeId, usize> = entries
            .iter()
            .map(|&e| (self.dag.nodes[e].commit.change_id.change_id(), 0))
            .collect();
        for node in self.dag.nodes.iter() {
            if let Some(count) = counts.get_mut(&node.commit.change_id.change_id()) {
                *count += 1;
            }
        }
        entries
            .iter()
            .map(|&e| {
                let commit = &self.dag.nodes[e].commit;
                let change_id = commit.change_id.change_id();
                Revision {
                    commit_id: commit.graph_id.clone(),
                    divergent: counts[&change_id] > 1,
                    change_id,
                }
            })
            .collect()
    }

    /// Where `revision` is in the DAG so far. `complete` says the whole
    /// reload is in, so that nothing missing can still arrive.
    pub(super) fn locate(&self, revision: &Revision, complete: bool) -> Whereabouts {
        if let Some(entry) = self.entry_by_commit_id(&revision.commit_id) {
            return Whereabouts::Here(entry);
        }
        // Among several commits of one change, this one may still come back
        // itself in a later chunk.
        if revision.divergent && !complete {
            return Whereabouts::Pending;
        }
        let copies: Vec<EntryIdx> = self
            .dag
            .nodes
            .iter_enumerated()
            .filter(|(_, node)| node.commit.change_id.change_id() == revision.change_id)
            .map(|(idx, _)| idx)
            .collect();
        match (copies.is_empty(), complete) {
            (false, _) => Whereabouts::Rewritten(copies),
            (true, true) => Whereabouts::Gone,
            (true, false) => Whereabouts::Pending,
        }
    }

    /// Move the state of commits missing from the DAG to their rewrites, and
    /// forget it once the reload is `complete` without either turning up.
    /// `old_nodes` holds the pre-reload commits that haven't come back, and
    /// `divergent_before` the changes that had several commits before.
    /// Returns whether a selection had to be dropped, for the caller to say
    /// so once the load's own status is settled.
    #[must_use]
    pub(super) fn carry_commit_state(
        &mut self,
        old_nodes: &HashMap<CommitId, DagNode>,
        divergent_before: &HashSet<ChangeId>,
        complete: bool,
    ) -> bool {
        let stranded: HashSet<CommitId> = self
            .dag
            .unfolded_commits
            .iter()
            .chain(self.dag.unfolded_files.iter().map(|k| &k.commit_id))
            .chain(self.selection.iter().map(Selection::commit_id))
            .filter(|id| !self.dag.commit_index.contains_key(*id))
            .cloned()
            .collect();
        if stranded.is_empty() {
            return false;
        }

        let mut dropped_selection = false;
        for old in &stranded {
            // A key for a commit that wasn't in the DAG before either has
            // nothing to find a rewrite by.
            let whereabouts = match old_nodes.get(old) {
                Some(node) => {
                    let change_id = node.commit.change_id.change_id();
                    self.locate(
                        &Revision {
                            commit_id: old.clone(),
                            divergent: divergent_before.contains(&change_id),
                            change_id,
                        },
                        complete,
                    )
                }
                None if complete => Whereabouts::Gone,
                None => Whereabouts::Pending,
            };
            match whereabouts {
                Whereabouts::Rewritten(copies) => {
                    let to: Vec<CommitId> =
                        copies.iter().map(|&e| self.commit_id(e).clone()).collect();
                    dropped_selection |= self.move_commit_state(old, &to);
                }
                Whereabouts::Gone => self.forget_commit_state(old),
                Whereabouts::Here(_) | Whereabouts::Pending => {}
            }
        }
        dropped_selection
    }

    /// Move `old`'s state to `to`, its rewrites. Folds go to each of them;
    /// a selection only to a single one, since spread over several it would
    /// change what a command acts on. Returns whether a selection was
    /// dropped for that reason.
    fn move_commit_state(&mut self, old: &CommitId, to: &[CommitId]) -> bool {
        if self.dag.unfolded_commits.remove(old) {
            self.dag.unfolded_commits.extend(to.iter().cloned());
        }
        let files: Vec<FileFoldKey> = self
            .dag
            .unfolded_files
            .iter()
            .filter(|k| k.commit_id == *old)
            .cloned()
            .collect();
        for key in files {
            self.dag.unfolded_files.remove(&key);
            self.dag
                .unfolded_files
                .extend(to.iter().map(|commit_id| FileFoldKey {
                    commit_id: commit_id.clone(),
                    path: key.path.clone(),
                }));
        }

        let selected: Vec<Selection> = self
            .selection
            .iter()
            .filter(|s| s.commit_id() == old)
            .cloned()
            .collect();
        if selected.is_empty() {
            return false;
        }
        self.selection.retain(|s| s.commit_id() != old);
        match to {
            [one] => {
                self.selection
                    .extend(selected.iter().map(|s| s.moved_to(one)));
                false
            }
            _ => true,
        }
    }

    fn forget_commit_state(&mut self, old: &CommitId) {
        self.dag.unfolded_commits.remove(old);
        self.dag.unfolded_files.retain(|k| k.commit_id != *old);
        self.selection.retain(|s| s.commit_id() != old);
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{chunk, entry, load};
    use super::super::{App, FileFoldKey};
    use crate::dag::DivergenceInfo;
    use crate::idx::EntryIdx;
    use crate::types::{CommitId, FileRef, RepoPath, Selection, SelectionKind};

    fn entry_of(app: &App, commit: &str) -> EntryIdx {
        app.entry_by_commit_id(&CommitId::new(commit))
            .expect("commit is loaded")
    }

    fn unfold(app: &mut App, commit: &str) {
        let commit_id = CommitId::new(commit);
        app.dag.unfolded_commits.insert(commit_id.clone());
        app.dag.unfolded_files.insert(FileFoldKey {
            commit_id,
            path: RepoPath::new("f"),
        });
    }

    fn file_unfolded(app: &App, commit: &str) -> bool {
        app.dag.unfolded_files.contains(&FileFoldKey {
            commit_id: CommitId::new(commit),
            path: RepoPath::new("f"),
        })
    }

    /// A rewrite keeps the change and replaces the commit; what was open or
    /// picked on the commit follows it to the rewrite.
    #[test]
    fn folds_and_selections_follow_a_rewrite() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        unfold(&mut app, "b1");
        app.toggle_commit_selection(entry_of(&app, "a1"));

        load(&mut app, vec![entry('a', "a2"), entry('b', "b2")], true);

        assert!(app.is_commit_unfolded(entry_of(&app, "b2")));
        assert!(file_unfolded(&app, "b2"));
        assert!(app.is_commit_selected(entry_of(&app, "a2")));
        assert!(app.dag.unfolded_commits.len() == 1 && app.selection_count() == 1);
    }

    /// The carried fold asks for the rewrite's files, which the old
    /// commit's cache can't stand in for.
    #[test]
    fn a_carried_fold_requests_the_rewrites_files() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('b', "b1")], true);
        unfold(&mut app, "b1");
        app.take_repo_requests();

        load(&mut app, vec![entry('b', "b2")], true);

        assert!(app.take_repo_requests().iter().any(|r| matches!(
            r,
            crate::repo_service::RepoRequest::DiffSummary { target }
                if *target == crate::dag::DiffTarget::Commit(CommitId::new("b2"))
        )));
    }

    /// Each copy of a divergent change is its own commit, and keeps its own
    /// state, including when its offset (computed after the load) arrives.
    #[test]
    fn copies_of_a_divergent_change_keep_their_own_state() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('u', "u1"), entry('u', "u2")], true);
        unfold(&mut app, "u2");
        app.dag.nodes[EntryIdx::new(1)].commit.divergence = Some(DivergenceInfo {
            is_divergent: true,
            is_hidden: false,
            suffix: Some(1),
        });

        assert!(app.is_commit_unfolded(entry_of(&app, "u2")));
        assert!(!app.is_commit_unfolded(entry_of(&app, "u1")));
    }

    /// With several commits of the change about, a copy missing from the
    /// first chunk may yet arrive itself; its state isn't handed to its
    /// sibling in the meantime.
    #[test]
    fn a_divergent_copy_is_waited_for_rather_than_mistaken_for_a_rewrite() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('u', "u1"), entry('u', "u2")], true);
        unfold(&mut app, "u2");

        load(&mut app, vec![entry('u', "u1")], false);
        assert!(!app.is_commit_unfolded(entry_of(&app, "u1")));
        chunk(&mut app, vec![entry('u', "u2")], true);

        assert!(app.is_commit_unfolded(entry_of(&app, "u2")));
        assert!(!app.is_commit_unfolded(entry_of(&app, "u1")));
    }

    /// A rewrite that leaves several copies has no one commit to carry to.
    /// Folds open on each; a selection spread across them would change what
    /// a command acts on, so it is dropped, with word to the user.
    #[test]
    fn a_rewrite_into_several_copies_opens_each_and_drops_the_selection() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('u', "u1"), entry('b', "b1")], true);
        app.toggle_commit_selection(entry_of(&app, "b1"));
        app.toggle_commit_selection(entry_of(&app, "u1"));
        assert_eq!(app.selection_count(), 2);
        unfold(&mut app, "u1");

        let entries = vec![entry('u', "u2"), entry('u', "u3"), entry('b', "b1")];
        load(&mut app, entries, true);

        assert!(app.is_commit_unfolded(entry_of(&app, "u2")));
        assert!(app.is_commit_unfolded(entry_of(&app, "u3")));
        assert_eq!(app.selection_count(), 1);
        assert!(app.is_commit_selected(entry_of(&app, "b1")));
        assert!(app.status_message.is_some());
    }

    /// A commit gone without a rewrite takes its state with it once the
    /// load is complete.
    #[test]
    fn an_abandoned_commits_state_is_forgotten() {
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        unfold(&mut app, "b1");
        app.selection.insert(Selection::File(FileRef {
            commit_id: CommitId::new("b1"),
            path: RepoPath::new("f"),
        }));

        load(&mut app, vec![entry('a', "a1")], true);

        assert!(app.dag.unfolded_commits.is_empty());
        assert!(app.dag.unfolded_files.is_empty());
        assert_eq!(app.selection_kind(), SelectionKind::Commit);
        assert!(!app.selection_active());
    }
}
