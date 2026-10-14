//! Placeholder rows: where content belongs that is still loading, or that
//! failed to load and can be asked for again.

use super::{App, Loadable};
use crate::repo_service::{RepoError, RepoRequest, RevsetLoadKind};
use crate::types::{ActiveView, ContentSlot, FileOwner};

/// How the content a placeholder stands for is doing.
pub enum SlotState<'a> {
    Loading,
    Failed(&'a RepoError),
    /// Loaded, or never asked for: nothing to hold a place for.
    Settled,
}

impl SlotState<'_> {
    /// Whether the slot needs its placeholder row.
    pub fn needs_row(&self) -> bool {
        !matches!(self, Self::Settled)
    }
}

fn state_of<T>(load: &Loadable<T>) -> SlotState<'_> {
    match load {
        Loadable::Loading => SlotState::Loading,
        Loadable::Failed(error) => SlotState::Failed(error),
        Loadable::NotRequested | Loadable::Loaded(_) => SlotState::Settled,
    }
}

impl App {
    /// How the content for `slot` is doing.
    pub fn slot_state(&self, slot: ContentSlot) -> SlotState<'_> {
        match slot {
            ContentSlot::Files(owner) => self
                .file_tree(owner)
                .map_or(SlotState::Settled, |tree| state_of(tree.summary())),
            ContentSlot::Diff(owner, file_idx) => {
                let diff = self
                    .file_tree(owner)
                    .and_then(|tree| tree.diff(file_idx))
                    .map_or(SlotState::Settled, state_of);
                let hunks = match owner {
                    FileOwner::Dag(entry_idx) => self.dag.nodes[entry_idx]
                        .conflict_hunks(file_idx)
                        .map_or(SlotState::Settled, state_of),
                    FileOwner::EvoLog(_) | FileOwner::Interdiff => SlotState::Settled,
                };
                // A conflicted file needs both; either failing is a failure.
                match (diff, hunks) {
                    (failed @ SlotState::Failed(_), _) | (_, failed @ SlotState::Failed(_)) => {
                        failed
                    }
                    (SlotState::Loading, _) | (_, SlotState::Loading) => SlotState::Loading,
                    (SlotState::Settled, SlotState::Settled) => SlotState::Settled,
                }
            }
            ContentSlot::Operation(op_log_idx) => self
                .op_log
                .entries
                .get(op_log_idx.raw())
                .and_then(|entry| self.op_log.details.get(&entry.id))
                .map_or(SlotState::Settled, state_of),
            ContentSlot::View => match self.active_view {
                ActiveView::Dag => state_of(&self.revset.load_state),
                ActiveView::Operations => state_of(&self.op_log.load_state),
                ActiveView::Evolog => state_of(&self.evolog.load_state),
                ActiveView::Annotate => state_of(&self.annotate.lines),
                _ => SlotState::Settled,
            },
        }
    }

    /// Ask again for content that failed to load. Its placeholder stays in
    /// place and reads as loading once the request is on its way.
    pub(super) fn retry_slot(&mut self, slot: ContentSlot) {
        if !matches!(self.slot_state(slot), SlotState::Failed(_)) {
            return;
        }
        match slot {
            ContentSlot::Files(owner) => {
                let request = self
                    .file_tree_mut(owner)
                    .and_then(|tree| tree.request_summary());
                self.pending_repo_requests.extend(request);
            }
            ContentSlot::Diff(owner, file_idx) => self.request_file_contents(owner, file_idx),
            ContentSlot::Operation(op_log_idx) => {
                let Some(op_id) = self
                    .op_log
                    .entries
                    .get(op_log_idx.raw())
                    .map(|entry| entry.id.clone())
                else {
                    return;
                };
                if self
                    .op_log
                    .details
                    .get_mut(&op_id)
                    .is_some_and(Loadable::begin)
                {
                    self.pending_repo_requests
                        .push(RepoRequest::OpDiff { op_id });
                }
            }
            ContentSlot::View => match self.active_view {
                ActiveView::Dag => self.refresh(RevsetLoadKind::Snapshot),
                ActiveView::Operations => {
                    if self.op_log.load_state.begin() {
                        self.pending_repo_requests.push(RepoRequest::Operations {
                            limit: self.op_log.limit,
                        });
                    }
                }
                ActiveView::Evolog => {
                    if let Some(commit_id) = self.evolog.commit_id.clone()
                        && self.evolog.load_state.begin()
                    {
                        self.pending_repo_requests
                            .push(RepoRequest::EvolutionLog { commit_id });
                    }
                }
                ActiveView::Annotate => {
                    if let Some(target) = self.annotate.target.clone() {
                        self.request_annotation(target.commit_id, target.path);
                    }
                }
                _ => {}
            },
        }
    }
}
