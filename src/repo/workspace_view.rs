use std::collections::HashMap;

use futures::TryStreamExt as _;
use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::object_id::ObjectId;
use jj_lib::repo::Repo;
use pollster::FutureExt as _;

use super::{JjRepo, DISPLAY_ID_LEN};
use crate::dag::ShortId;
use crate::types::{BookmarkName, CommitId as UiCommitId, RemoteName, TagName, WorkspaceName};

impl JjRepo {
    pub fn workspace_entries(&self) -> Vec<crate::app::WorkspaceViewEntry> {
        let mut entries = Vec::new();
        for (ws_name, commit_id) in self.repo.view().wc_commit_ids() {
            let is_current = *ws_name == self.workspace_name;
            let commit = self.repo.store().get_commit(commit_id).ok();
            let change_id = commit.as_ref().map(|c| {
                let h = c.change_id().reverse_hex();
                ShortId {
                    display: h.get(..DISPLAY_ID_LEN).unwrap_or(&h).to_string(),
                    prefix_len: DISPLAY_ID_LEN,
                }
            });
            let description = commit.as_ref().and_then(|c| {
                let raw = c.description().trim().to_string();
                if raw.is_empty() {
                    None
                } else {
                    raw.lines().next().map(String::from)
                }
            });
            let commit_hex = commit_id.hex();
            entries.push(crate::app::WorkspaceViewEntry {
                name: WorkspaceName::new(ws_name.as_str()),
                commit_id: Some(UiCommitId::new(
                    commit_hex.get(..DISPLAY_ID_LEN).unwrap_or(&commit_hex),
                )),
                change_id,
                description,
                is_current,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
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
        use crate::dag::{TagDetails, TagLocalTarget, TagRemoteTarget};

        let repo = self.repo.as_ref();
        let view = repo.view();
        let mut result: HashMap<TagName, TagDetails> = HashMap::new();

        // Pre-index remote tags by name.
        let mut remotes_by_name: HashMap<String, Vec<(String, jj_lib::backend::CommitId)>> =
            HashMap::new();
        for (symbol, remote_ref) in view.all_remote_tags() {
            if let Some(commit_id) = remote_ref.target.as_normal() {
                remotes_by_name
                    .entry(symbol.name.as_str().to_owned())
                    .or_default()
                    .push((symbol.remote.as_str().to_owned(), commit_id.clone()));
            }
        }

        // Process local tags.
        for (name, target) in view.local_tags() {
            let tag_name = TagName::new(name.as_str());
            let local_target = target.as_normal().and_then(|commit_id| {
                let info = self.commit_detail_info(commit_id)?;
                Some(TagLocalTarget {
                    summary: crate::dag::CommitSummary {
                        commit_id: UiCommitId::new(commit_id.hex()),
                        change_id: info.change_id,
                        short_commit_id: info.short_commit_id,
                        description: info.description,
                    },
                })
            });

            let remote_targets: Vec<TagRemoteTarget> = remotes_by_name
                .get(name.as_str())
                .map(|refs| {
                    refs.iter()
                        .filter_map(|(remote, commit_id)| {
                            let info = self.commit_detail_info(commit_id)?;
                            Some(TagRemoteTarget {
                                remote: RemoteName::new(remote),
                                summary: crate::dag::CommitSummary {
                                    commit_id: UiCommitId::new(commit_id.hex()),
                                    change_id: info.change_id,
                                    short_commit_id: info.short_commit_id,
                                    description: info.description,
                                },
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            if local_target.is_some() || !remote_targets.is_empty() {
                result.insert(
                    tag_name,
                    TagDetails {
                        is_deleted: false,
                        local_target,
                        remote_targets,
                    },
                );
            }
        }

        // Remote-only tags (deleted locally).
        for (name, refs) in &remotes_by_name {
            let tag_name = TagName::new(name.as_str());
            if result.contains_key(&tag_name) {
                continue;
            }
            let remote_targets: Vec<TagRemoteTarget> = refs
                .iter()
                .map(|(remote, commit_id)| {
                    let hex = commit_id.hex();
                    let resolved = self.commit_detail_info(commit_id);
                    let (change_id, short_commit_id, description) = match resolved {
                        Some(info) => (info.change_id, info.short_commit_id, info.description),
                        None => {
                            let display = hex.get(..DISPLAY_ID_LEN).unwrap_or(&hex).to_string();
                            (
                                ShortId {
                                    display: display.clone(),
                                    prefix_len: DISPLAY_ID_LEN,
                                },
                                ShortId {
                                    display,
                                    prefix_len: DISPLAY_ID_LEN,
                                },
                                None,
                            )
                        }
                    };
                    TagRemoteTarget {
                        remote: RemoteName::new(remote),
                        summary: crate::dag::CommitSummary {
                            commit_id: UiCommitId::new(hex),
                            change_id,
                            short_commit_id,
                            description,
                        },
                    }
                })
                .collect();
            result.insert(
                tag_name,
                TagDetails {
                    is_deleted: true,
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

                    // Skip fully-synced tracked remotes — no useful info to show.
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
            },
            is_hidden: info.is_hidden,
            change_id_suffix: info.change_id_suffix,
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
            },
            is_tracked,
            behind_count,
            ahead_count,
            change_id_suffix: info.change_id_suffix,
        })
    }

    /// Count commits reachable from `to` but not from `from`.
    fn count_revs_between(&self, from: &BackendCommitId, to: &BackendCommitId) -> Option<usize> {
        let revset = jj_lib::revset::walk_revs(
            self.repo.as_ref(),
            std::slice::from_ref(to),
            std::slice::from_ref(from),
        )
        .ok()?;
        Some(
            revset
                .stream()
                .try_collect::<Vec<_>>()
                .block_on()
                .ok()?
                .len(),
        )
    }
}
