use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{parse_first_line_description, JjRepo, DISPLAY_ID_LEN};
use crate::dag::{
    AuthorInfo, BookmarkInfo, CommitInfo, DivergenceUpdate, PrefixLengthUpdate, RemoteBookmarkInfo,
    ShortId,
};
use crate::types::{BookmarkName, CommitId as UiCommitId, TagName};
use color_eyre::eyre::Context;
use color_eyre::Result;
use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::commit::Commit;
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::object_id::ObjectId;
use jj_lib::ref_name::RefName;
use jj_lib::repo::{ReadonlyRepo, Repo};
use jj_lib::repo_path::RepoPathUiConverter;
use jj_lib::revset::RevsetExtensions;

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
    pub change_id_suffix: Option<usize>,
    pub is_hidden: bool,
}

impl JjRepo {
    pub(super) fn extract_commit_info(
        &self,
        commit: &Commit,
        is_immutable: bool,
        ctx: &CommitContext<'_>,
    ) -> Result<CommitInfo> {
        let repo = self.repo.as_ref();

        // Change ID: use fixed DISPLAY_ID_LEN; accurate prefix computed in background.
        let change_id_full = commit.change_id().reverse_hex();
        let change_id = ShortId {
            display: change_id_full
                .get(..DISPLAY_ID_LEN)
                .unwrap_or(&change_id_full)
                .to_string(),
            prefix_len: DISPLAY_ID_LEN,
        };

        // Commit ID: use fixed DISPLAY_ID_LEN; accurate prefix computed in background.
        let commit_id_full = commit.id().hex();
        let commit_id = ShortId {
            display: commit_id_full
                .get(..DISPLAY_ID_LEN)
                .unwrap_or(&commit_id_full)
                .to_string(),
            prefix_len: DISPLAY_ID_LEN,
        };

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
        let is_divergent = false;
        let is_hidden = false;
        let change_id_suffix = None;

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
            is_divergent,
            is_hidden,
            change_id_suffix,
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
        cancel: &crate::repo_service::CancellationToken,
    ) -> Result<Vec<(UiCommitId, PrefixLengthUpdate)>> {
        let repo = self.repo.as_ref();
        let extensions = RevsetExtensions::default();
        let fileset_aliases_map = FilesetAliasesMap::new();
        let path_converter = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = self.revset_parse_context(&extensions, &fileset_aliases_map, &path_converter);

        let id_prefix_context = self.build_id_prefix_context(&context);
        let id_prefix_index = id_prefix_context
            .populate(repo)
            .wrap_err("failed to populate ID prefix index")?;

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

            let change_prefix_len = id_prefix_index
                .shortest_change_prefix_len(repo, commit.change_id())
                .unwrap_or(DISPLAY_ID_LEN);
            let change_id_full = commit.change_id().reverse_hex();
            let change_display_len = change_prefix_len.max(DISPLAY_ID_LEN);
            let change_display = change_id_full
                .get(..change_display_len)
                .unwrap_or(&change_id_full)
                .to_string();

            let commit_prefix_len = id_prefix_index
                .shortest_commit_prefix_len(repo, &commit_id)
                .unwrap_or(DISPLAY_ID_LEN);
            let commit_id_full = commit_id.hex();
            let commit_display_len = commit_prefix_len.max(DISPLAY_ID_LEN);
            let commit_display = commit_id_full
                .get(..commit_display_len)
                .unwrap_or(&commit_id_full)
                .to_string();

            results.push((
                id.clone(),
                PrefixLengthUpdate {
                    change_display,
                    change_prefix_len,
                    commit_display,
                    commit_prefix_len,
                },
            ));
        }

        Ok(results)
    }

    /// Batch-compute divergence and hidden status for a set of commits.
    /// Called in a background thread after the initial revset load.
    pub fn compute_divergence_info(
        repo: &Arc<ReadonlyRepo>,
        commit_ids: &[UiCommitId],
        cancel: &crate::repo_service::CancellationToken,
    ) -> Vec<(UiCommitId, DivergenceUpdate)> {
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
                DivergenceUpdate {
                    is_divergent,
                    is_hidden,
                    change_id_suffix,
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
        let change_prefix_len = prefix_index
            .shortest_change_prefix_len(repo, commit.change_id())
            .unwrap_or(DISPLAY_ID_LEN);
        let change_id_hex = commit.change_id().reverse_hex();
        let change_display_len = change_prefix_len.max(DISPLAY_ID_LEN);
        ShortId {
            display: change_id_hex
                .get(..change_display_len)
                .unwrap_or(&change_id_hex)
                .to_string(),
            prefix_len: change_prefix_len,
        }
    }

    /// Build a `ShortId` for a backend commit ID using the prefix index.
    pub(super) fn short_commit_id(
        prefix_index: &jj_lib::id_prefix::IdPrefixIndex,
        repo: &dyn jj_lib::repo::Repo,
        commit_id: &BackendCommitId,
    ) -> ShortId {
        let prefix_len = prefix_index
            .shortest_commit_prefix_len(repo, commit_id)
            .unwrap_or(DISPLAY_ID_LEN);
        let hex = commit_id.hex();
        let display_len = prefix_len.max(DISPLAY_ID_LEN);
        ShortId {
            display: hex.get(..display_len).unwrap_or(&hex).to_string(),
            prefix_len,
        }
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
        let change_id_suffix = if is_divergent {
            resolved
                .as_ref()
                .and_then(|targets| targets.find_offset(commit.id()))
        } else {
            None
        };

        let change_id_full = commit.change_id().reverse_hex();
        let change_id = ShortId {
            display: change_id_full
                .get(..DISPLAY_ID_LEN)
                .unwrap_or(&change_id_full)
                .to_string(),
            prefix_len: DISPLAY_ID_LEN,
        };

        let commit_id_hex = commit_id.hex();
        let short_commit_id = ShortId {
            display: commit_id_hex
                .get(..DISPLAY_ID_LEN)
                .unwrap_or(&commit_id_hex)
                .to_string(),
            prefix_len: DISPLAY_ID_LEN,
        };

        let description = parse_first_line_description(commit.description());

        Some(CommitDetailInfo {
            change_id,
            short_commit_id,
            description,
            change_id_suffix,
            is_hidden,
        })
    }
}
