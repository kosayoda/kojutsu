use std::collections::HashMap;

use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::object_id::ObjectId;
use jj_lib::repo::Repo;
use pollster::FutureExt as _;

use super::JjRepo;
use crate::dag::ShortId;
use crate::types::{BookmarkName, CommitId as UiCommitId, RemoteName, TagName, WorkspaceName};

impl JjRepo {
    pub fn workspace_entries(&self) -> Vec<crate::dag::WorkspaceInfo> {
        let mut entries = Vec::new();
        for (ws_name, commit_id) in self.repo.view().wc_commit_ids() {
            let is_current = *ws_name == self.workspace_name;
            let commit = self.repo.store().get_commit(commit_id).ok();
            let change_id = commit
                .as_ref()
                .map(|c| ShortId::new(c.change_id().reverse_hex()));
            let description = commit.as_ref().and_then(|c| {
                let raw = c.description().trim().to_string();
                if raw.is_empty() {
                    None
                } else {
                    raw.lines().next().map(String::from)
                }
            });
            entries.push(crate::dag::WorkspaceInfo {
                name: WorkspaceName::new(ws_name.as_str()),
                // Whole hex: nothing renders this, but the background prefix
                // pass looks commits up by it.
                commit_id: Some(UiCommitId::new(commit_id.hex())),
                change_id,
                description,
                is_current,
            });
        }
        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    pub fn all_local_tags(&self) -> Vec<TagName> {
        self.repo
            .as_ref()
            .view()
            .local_tags()
            .map(|(name, _)| TagName::new(name.as_str()))
            .collect()
    }

    /// Extract rich tag details: local target + remote tracking info.
    pub fn extract_tag_details(&self) -> HashMap<TagName, crate::dag::TagDetails> {
        use crate::dag::{TagDetails, TagLocalTarget, TagPresence, TagRemoteTarget};

        let repo = self.repo.as_ref();
        let view = repo.view();

        let summary_of = |commit_id: &jj_lib::backend::CommitId| {
            let hex = commit_id.hex();
            match self.commit_detail_info(commit_id) {
                Some(info) => crate::dag::CommitSummary {
                    commit_id: UiCommitId::new(hex),
                    change_id: info.change_id,
                    short_commit_id: info.short_commit_id,
                    description: info.description,
                    divergence: info.divergence,
                },
                // Commit isn't in the repo, so its change ID is genuinely
                // unknown: leave it empty rather than showing the commit ID
                // in the change ID's place.
                None => crate::dag::CommitSummary {
                    commit_id: UiCommitId::new(&hex),
                    change_id: ShortId::new(""),
                    short_commit_id: ShortId::new(&hex),
                    description: None,
                    divergence: None,
                },
            }
        };

        // Remote tags by name, leaving out the synthetic `git` remote of a
        // colocated repo, which isn't one that can be tracked.
        let mut remotes_by_name: HashMap<String, Vec<TagRemoteTarget>> = HashMap::new();
        for (symbol, remote_ref) in view.all_remote_tags() {
            if Some(symbol.remote) == self.default_ignored_remote {
                continue;
            }
            if let Some(commit_id) = remote_ref.target.as_normal() {
                remotes_by_name
                    .entry(symbol.name.as_str().to_owned())
                    .or_default()
                    .push(TagRemoteTarget {
                        remote: RemoteName::new(symbol.remote.as_str()),
                        summary: summary_of(commit_id),
                        is_tracked: remote_ref.is_tracked(),
                    });
            }
        }

        let mut result: HashMap<TagName, TagDetails> = HashMap::new();
        for (name, target) in view.local_tags() {
            let local_target = target.as_normal().map(|commit_id| TagLocalTarget {
                summary: summary_of(commit_id),
            });
            let remote_targets = remotes_by_name.remove(name.as_str()).unwrap_or_default();
            result.insert(
                TagName::new(name.as_str()),
                TagDetails {
                    presence: TagPresence::Local,
                    local_target,
                    remote_targets,
                },
            );
        }

        // What's left is on remotes only: deleted here if a remote it
        // tracked still has it, otherwise never taken up.
        for (name, remote_targets) in remotes_by_name {
            let presence = if remote_targets.iter().any(|t| t.is_tracked) {
                TagPresence::Deleted
            } else {
                TagPresence::RemoteOnly
            };
            result.insert(
                TagName::new(&name),
                TagDetails {
                    presence,
                    local_target: None,
                    remote_targets,
                },
            );
        }

        result
    }

    /// Extract rich bookmark details: conflict targets and remote tracking info.
    /// Called during revset load in the background service thread.
    pub fn extract_bookmark_details(&self) -> HashMap<BookmarkName, crate::dag::BookmarkDetails> {
        use crate::dag::{BookmarkDetails, DiffKind};

        let repo = self.repo.as_ref();
        let view = repo.view();
        let mut result: HashMap<BookmarkName, BookmarkDetails> = HashMap::new();

        // Pre-index remote bookmarks by name to avoid O(n*m) scan.
        let mut remotes_by_name: HashMap<String, Vec<_>> = HashMap::new();
        for (symbol, remote_ref) in view.all_remote_bookmarks() {
            let Some(remote_commit_id) = remote_ref.target.as_normal() else {
                continue;
            };
            remotes_by_name
                .entry(symbol.name.as_str().to_owned())
                .or_default()
                .push((
                    symbol.remote.as_str().to_owned(),
                    remote_commit_id.clone(),
                    remote_ref.is_tracked(),
                ));
        }

        for (name, target) in view.local_bookmarks() {
            let bm_name = BookmarkName::new(name.as_str());
            let mut details = BookmarkDetails {
                conflict_targets: Vec::new(),
                remote_targets: Vec::new(),
            };

            // Conflict targets: removed (-) then added (+).
            if target.has_conflict() {
                for removed_id in target.removed_ids() {
                    if let Some(ct) = self.make_conflict_target(removed_id, DiffKind::Removed) {
                        details.conflict_targets.push(ct);
                    }
                }
                for added_id in target.added_ids() {
                    if let Some(ct) = self.make_conflict_target(added_id, DiffKind::Added) {
                        details.conflict_targets.push(ct);
                    }
                }
            }

            // Remote tracking info for this bookmark.
            let local_normal = target.as_normal();
            if let Some(remote_refs) = remotes_by_name.get(name.as_str()) {
                for (remote_name, remote_commit_id, is_tracked) in remote_refs {
                    let (behind_count, ahead_count) = if let Some(local_id) = local_normal {
                        if local_id == remote_commit_id {
                            (Some(0), Some(0))
                        } else {
                            (
                                self.count_revs_between(local_id, remote_commit_id),
                                self.count_revs_between(remote_commit_id, local_id),
                            )
                        }
                    } else {
                        (None, None)
                    };

                    // Skip fully-synced tracked remotes: no useful info to show.
                    let is_synced = behind_count == Some(0) && ahead_count == Some(0);
                    if is_synced && *is_tracked {
                        continue;
                    }

                    if let Some(rt) = self.make_remote_target(
                        remote_commit_id,
                        RemoteName::new(remote_name),
                        *is_tracked,
                        behind_count,
                        ahead_count,
                    ) {
                        details.remote_targets.push(rt);
                    }
                }
            }

            // Only store if there's something to show.
            if !details.conflict_targets.is_empty() || !details.remote_targets.is_empty() {
                result.insert(bm_name, details);
            }
        }

        result
    }

    /// Build a `BookmarkConflictTarget` from a commit ID.
    fn make_conflict_target(
        &self,
        commit_id: &BackendCommitId,
        kind: crate::dag::DiffKind,
    ) -> Option<crate::dag::BookmarkConflictTarget> {
        let info = self.commit_detail_info(commit_id)?;
        Some(crate::dag::BookmarkConflictTarget {
            kind,
            summary: crate::dag::CommitSummary {
                commit_id: UiCommitId::new(commit_id.hex()),
                change_id: info.change_id,
                short_commit_id: info.short_commit_id,
                description: info.description,
                divergence: info.divergence,
            },
        })
    }

    /// Build a `BookmarkRemoteTarget` from a remote commit ID.
    fn make_remote_target(
        &self,
        commit_id: &BackendCommitId,
        remote: RemoteName,
        is_tracked: bool,
        behind_count: Option<usize>,
        ahead_count: Option<usize>,
    ) -> Option<crate::dag::BookmarkRemoteTarget> {
        let info = self.commit_detail_info(commit_id)?;
        Some(crate::dag::BookmarkRemoteTarget {
            remote,
            summary: crate::dag::CommitSummary {
                commit_id: UiCommitId::new(commit_id.hex()),
                change_id: info.change_id,
                short_commit_id: info.short_commit_id,
                description: info.description,
                divergence: info.divergence,
            },
            is_tracked,
            behind_count,
            ahead_count,
        })
    }

    /// Count commits reachable from `to` but not from `from`.
    fn count_revs_between(&self, from: &BackendCommitId, to: &BackendCommitId) -> Option<usize> {
        use futures::StreamExt as _;
        let revset = jj_lib::revset::walk_revs(
            self.repo.as_ref(),
            std::slice::from_ref(to),
            std::slice::from_ref(from),
        )
        .ok()?;
        let count = revset
            .stream()
            .fold(0usize, |acc, item| async move {
                if item.is_ok() { acc + 1 } else { acc }
            })
            .block_on();
        Some(count)
    }
}
