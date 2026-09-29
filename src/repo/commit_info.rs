use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{JjRepo, parse_first_line_description};
use crate::dag::{
    AuthorInfo, BookmarkInfo, CommitInfo, DivergenceInfo, PrefixLengthUpdate, RemoteBookmarkInfo,
    ShortId,
};
use crate::types::{BookmarkName, CommitId as UiCommitId, TagName};
use color_eyre::Result;
use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::commit::Commit;
use jj_lib::object_id::ObjectId;
use jj_lib::ref_name::RefName;
use jj_lib::repo::{ReadonlyRepo, Repo};
use pollster::FutureExt as _;

/// Pre-built lookup tables passed to `extract_commit_info` for each commit.
pub(super) struct CommitContext<'a> {
    pub dirty_bookmarks: &'a HashSet<&'a RefName>,
    pub tracking_bookmarks: &'a HashSet<&'a RefName>,
    pub remote_bookmark_map: &'a HashMap<BackendCommitId, Vec<RemoteBookmarkInfo>>,
    pub wc_commit_workspaces:
        &'a HashMap<&'a BackendCommitId, Vec<crate::dag::WorkspaceAnnotation>>,
}

pub(super) struct CommitDetailInfo {
    pub change_id: ShortId,
    pub short_commit_id: ShortId,
    pub description: Option<String>,
    /// `None` when the commit is neither divergent nor hidden, matching
    /// `CommitInfo::divergence`.
    pub divergence: Option<DivergenceInfo>,
}

impl JjRepo {
    pub(super) fn extract_commit_info(
        &self,
        commit: &Commit,
        is_immutable: bool,
        ctx: &CommitContext<'_>,
    ) -> Result<CommitInfo> {
        let repo = self.repo.as_ref();

        // Prefix lengths are computed by a background pass; until it lands
        // these display the default width.
        let change_id = ShortId::new(commit.change_id().reverse_hex());
        let commit_id = ShortId::new(commit.id().hex());

        // Description
        let raw_desc = commit.description().trim();
        let description = parse_first_line_description(commit.description());
        let full_description = if raw_desc.contains('\n') {
            Some(raw_desc.to_string())
        } else {
            None
        };

        // Author
        let sig = commit.author();
        let millis = sig.timestamp.timestamp.0;
        let tz_offset_seconds = sig.timestamp.tz_offset * 60;
        let timestamp =
            jiff::Timestamp::from_millisecond(millis).unwrap_or(jiff::Timestamp::UNIX_EPOCH);

        let author = AuthorInfo {
            name: sig.name.clone(),
            email: sig.email.clone(),
            timestamp,
            tz_offset_seconds,
        };

        // Workspaces (O(1) lookup from pre-built map)
        let workspaces = ctx
            .wc_commit_workspaces
            .get(commit.id())
            .cloned()
            .unwrap_or_default();

        // Empty status is deferred to a background thread for all commits
        // (see repo_service.rs) to avoid blocking the initial load.
        let is_merge = commit.parent_ids().len() > 1;
        let is_empty = false;

        // Conflicts
        let has_conflict = commit.has_conflict();

        // Bookmarks (with dirty/tracking status)
        let bookmarks: Vec<BookmarkInfo> = repo
            .view()
            .local_bookmarks_for_commit(commit.id())
            .map(|(name, target)| BookmarkInfo {
                name: BookmarkName::new(name.as_str()),
                is_dirty: ctx.dirty_bookmarks.contains(name),
                is_tracking: ctx.tracking_bookmarks.contains(name),
                is_conflicted: target.has_conflict(),
            })
            .collect();

        // Tags pointing at this commit.
        let tags: Vec<TagName> = repo
            .view()
            .local_tags()
            .filter(|(_, target)| target.added_ids().any(|id| id == commit.id()))
            .map(|(name, _)| TagName::new(name.as_str()))
            .collect();

        // Remote bookmarks pointing at this commit, excluding synced ones
        // (where the remote target matches the local target). Matches jj's
        // collect_distinct_refs behavior: show local + unsynced remote.
        let remote_bookmarks: Vec<RemoteBookmarkInfo> = ctx
            .remote_bookmark_map
            .get(commit.id())
            .map(|rbs| rbs.iter().filter(|rb| !rb.synced).cloned().collect())
            .unwrap_or_default();

        // Divergence, hidden status, and change ID disambiguation are
        // deferred to a background thread (see compute_divergence_info)
        // because resolve_change_id() and is_hidden() are expensive
        // per-commit index lookups.
        // Full commit ID hex for graph rendering (stable key).
        let graph_id = UiCommitId::new(commit.id().hex());

        Ok(CommitInfo {
            graph_id,
            change_id,
            commit_id,
            description,
            full_description,
            author,
            workspaces,
            is_empty,
            is_merge,
            has_conflict,
            is_immutable,
            divergence: None,
            bookmarks,
            remote_bookmarks,
            tags,
        })
    }

    /// Compute shortest unique prefix lengths for a set of commits.
    /// Called in a background thread after the initial revset load.
    /// Returns (commit_hex_id, change_display, change_prefix_len, commit_display, commit_prefix_len).
    pub fn compute_prefix_lengths(
        &self,
        commit_ids: &[UiCommitId],
        cancel: &super::CancellationToken,
    ) -> Result<Vec<(UiCommitId, PrefixLengthUpdate)>> {
        let repo = self.repo.as_ref();
        let id_prefix_index = self.id_prefix_index()?;

        let mut results = Vec::new();
        for (i, id) in commit_ids.iter().enumerate() {
            if i % 100 == 0 && cancel.is_cancelled() {
                break;
            }
            let Some(commit_id) = BackendCommitId::try_from_hex(id.as_str()) else {
                continue;
            };
            let Ok(commit) = repo.store().get_commit(&commit_id) else {
                continue;
            };

            // Skip rather than report a made-up length: the ID keeps its
            // placeholder width, which is honest about not knowing.
            let (Ok(change_prefix_len), Ok(commit_prefix_len)) = (
                id_prefix_index.shortest_change_prefix_len(repo, commit.change_id()),
                id_prefix_index.shortest_commit_prefix_len(repo, &commit_id),
            ) else {
                tracing::warn!("prefix computation failed for {id}");
                continue;
            };

            results.push((
                id.clone(),
                PrefixLengthUpdate {
                    change_prefix_len,
                    commit_prefix_len,
                },
            ));
        }

        Ok(results)
    }

    /// Call `on_empty` for each of `commit_ids` that changes nothing, as
    /// it is found. Each check is a tree diff, so the token is checked
    /// before every one.
    pub fn find_empty_commits(
        repo: &Arc<ReadonlyRepo>,
        commit_ids: &[UiCommitId],
        cancel: &super::CancellationToken,
        mut on_empty: impl FnMut(&UiCommitId),
    ) {
        for id in commit_ids {
            if cancel.is_cancelled() {
                return;
            }
            let Some(backend_id) = BackendCommitId::try_from_hex(id.as_str()) else {
                continue;
            };
            let Ok(commit) = repo.store().get_commit(&backend_id) else {
                continue;
            };
            if commit
                .is_empty(repo.as_ref())
                .block_on()
                .inspect_err(|e| tracing::warn!("is_empty check failed: {e}"))
                .unwrap_or(false)
            {
                on_empty(id);
            }
        }
    }

    /// Batch-compute divergence and hidden status for a set of commits.
    /// Called in a background thread after the initial revset load.
    pub fn compute_divergence_info(
        repo: &Arc<ReadonlyRepo>,
        commit_ids: &[UiCommitId],
        cancel: &super::CancellationToken,
    ) -> Vec<(UiCommitId, DivergenceInfo)> {
        let mut results = Vec::new();
        for (i, id) in commit_ids.iter().enumerate() {
            if i % 100 == 0 && cancel.is_cancelled() {
                return results;
            }
            let Some(commit_id) = BackendCommitId::try_from_hex(id.as_str()) else {
                continue;
            };
            let Ok(commit) = repo.store().get_commit(&commit_id) else {
                continue;
            };
            let resolved = repo.resolve_change_id(commit.change_id()).ok().flatten();
            let is_divergent = resolved
                .as_ref()
                .is_some_and(|targets| targets.is_divergent());
            let is_hidden = commit
                .is_hidden(repo.as_ref())
                .inspect_err(|e| tracing::warn!("is_hidden check failed: {e}"))
                .unwrap_or(false);
            if !is_divergent && !is_hidden {
                continue;
            }
            let change_id_suffix = resolved
                .as_ref()
                .and_then(|targets| targets.find_offset(commit.id()));
            results.push((
                id.clone(),
                DivergenceInfo {
                    is_divergent,
                    is_hidden,
                    suffix: change_id_suffix,
                },
            ));
        }
        results
    }

    /// Build a `ShortId` for a commit's change ID using the prefix index.
    pub(super) fn short_change_id(
        prefix_index: &jj_lib::id_prefix::IdPrefixIndex,
        repo: &dyn jj_lib::repo::Repo,
        commit: &Commit,
    ) -> ShortId {
        let mut id = ShortId::new(commit.change_id().reverse_hex());
        if let Ok(len) = prefix_index.shortest_change_prefix_len(repo, commit.change_id()) {
            id.set_prefix_len(len);
        }
        id
    }

    /// Build a `ShortId` for a backend commit ID using the prefix index.
    pub(super) fn short_commit_id(
        prefix_index: &jj_lib::id_prefix::IdPrefixIndex,
        repo: &dyn jj_lib::repo::Repo,
        commit_id: &BackendCommitId,
    ) -> ShortId {
        let mut id = ShortId::new(commit_id.hex());
        if let Ok(len) = prefix_index.shortest_commit_prefix_len(repo, commit_id) {
            id.set_prefix_len(len);
        }
        id
    }

    /// Shared commit metadata extraction for bookmark detail rows.
    pub(super) fn commit_detail_info(
        &self,
        commit_id: &BackendCommitId,
    ) -> Option<CommitDetailInfo> {
        let repo = self.repo.as_ref();
        let commit = repo.store().get_commit(commit_id).ok()?;
        let is_hidden = commit
            .is_hidden(repo)
            .inspect_err(|e| tracing::warn!("is_hidden check failed: {e}"))
            .unwrap_or(false);

        let resolved = repo.resolve_change_id(commit.change_id()).ok().flatten();
        let is_divergent = resolved
            .as_ref()
            .is_some_and(|targets| targets.is_divergent());
        // A hidden commit needs the offset just as much as a divergent one:
        // its bare change ID resolves to whichever commit superseded it.
        let change_id_suffix = (is_divergent || is_hidden)
            .then(|| {
                resolved
                    .as_ref()
                    .and_then(|targets| targets.find_offset(commit.id()))
            })
            .flatten();

        let change_id = ShortId::new(commit.change_id().reverse_hex());
        let short_commit_id = ShortId::new(commit_id.hex());

        let description = parse_first_line_description(commit.description());

        let divergence = (is_divergent || is_hidden).then_some(DivergenceInfo {
            is_divergent,
            is_hidden,
            suffix: change_id_suffix,
        });

        Some(CommitDetailInfo {
            change_id,
            short_commit_id,
            description,
            divergence,
        })
    }
}
