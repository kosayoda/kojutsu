use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::Context;
use futures::TryStreamExt as _;
use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::object_id::ObjectId;
use jj_lib::repo::Repo;
use pollster::FutureExt as _;

use super::{JjRepo, parse_first_line_description};
use crate::dag::ShortId;
use crate::history::{
    EvoLogEntry, OpDetailLine, OpDiffBookmark, OpDiffCommit, OpDiffWorkingCopy, OpLogEntry,
};
use crate::types::{CommitId as UiCommitId, OperationId, Str, WorkspaceName};

impl JjRepo {
    /// Walk the operation log and return entries in reverse chronological order.
    /// Returns at most `limit` entries and a flag indicating whether more exist.
    pub fn operation_log(&self, limit: usize) -> Result<(Vec<OpLogEntry>, bool)> {
        use futures::StreamExt as _;

        let current_op = self.repo.operation().clone();
        let current_op_id = current_op.id().clone();
        let stream = jj_lib::op_walk::walk_ancestors(&[current_op]);

        let mut entries = Vec::new();
        let mut has_more = false;
        let mut stream = std::pin::pin!(stream);
        while let Some(result) = stream.next().block_on() {
            // Read one past the limit only to learn whether more exist.
            if entries.len() == limit {
                has_more = true;
                break;
            }
            let op = result.wrap_err("failed to read operation")?;
            let meta = op.metadata();
            let user: Str = if meta.hostname.is_empty() {
                meta.username.as_str().into()
            } else {
                format!("{}@{}", meta.username, meta.hostname).into()
            };
            entries.push(OpLogEntry {
                id: OperationId::new(op.id().hex()),
                parent_ids: op
                    .parent_ids()
                    .iter()
                    .map(|id| OperationId::new(id.hex()))
                    .collect(),
                description: meta.description.as_str().into(),
                relative_time: crate::time::relative(meta.time.start.timestamp.0),
                workspace: meta
                    .workspace_name
                    .as_ref()
                    .map(|ws| WorkspaceName::new(ws.as_str())),
                user,
                args: meta.attributes.get("args").map(|s| s.as_str().into()),
                is_snapshot: meta.is_snapshot,
                is_current: *op.id() == current_op_id,
            });
        }
        Ok((entries, has_more))
    }

    /// Load the evolution log (predecessor chain) for a commit.
    pub fn evolution_log(&self, commit_id: &UiCommitId) -> Result<Vec<EvoLogEntry>> {
        let repo = self.repo.as_ref();
        let commit_id = super::parse_commit_id(commit_id.as_str())?;
        let prefix_index = self.id_prefix_index()?;

        let steps: Vec<_> = jj_lib::evolution::walk_predecessors(repo, &[commit_id])
            .try_collect()
            .block_on()?;
        let entries = steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                let commit = &step.commit;
                let sig = commit.author();
                let author: Str = if sig.name.is_empty() {
                    sig.email.as_str().into()
                } else {
                    sig.name.as_str().into()
                };
                EvoLogEntry {
                    commit_id: UiCommitId::new(commit.id().hex()),
                    change_id: Self::short_change_id(&prefix_index, repo, commit),
                    description: parse_first_line_description(commit.description()),
                    author,
                    relative_time: crate::time::relative(sig.timestamp.timestamp.0),
                    op_description: step
                        .operation
                        .as_ref()
                        .map(|op| op.metadata().description.as_str().into()),
                    // The walk starts at the commit asked about: its newest version.
                    is_current: i == 0,
                    predecessor_ids: step
                        .predecessor_ids()
                        .iter()
                        .map(|id| UiCommitId::new(id.hex()))
                        .collect(),
                }
            })
            .collect();
        Ok(entries)
    }

    /// Compute the diff between an operation and its parent.
    pub fn op_diff(&self, op_id_hex: &str) -> Result<Vec<OpDetailLine>> {
        use crate::dag::DiffKind;

        let op_store = self.repo.op_store();
        let prefix = jj_lib::object_id::HexPrefix::try_from_hex(op_id_hex).ok_or_else(|| {
            color_eyre::eyre::eyre!("can't parse `{op_id_hex}` as an operation ID")
        })?;
        let resolution = op_store
            .resolve_operation_id_prefix(&prefix)
            .block_on()
            .wrap_err("failed to resolve operation ID prefix")?;
        let op_id = match resolution {
            jj_lib::object_id::PrefixResolution::SingleMatch(id) => id,
            jj_lib::object_id::PrefixResolution::AmbiguousMatch => {
                color_eyre::eyre::bail!("ambiguous operation ID prefix `{op_id_hex}`");
            }
            jj_lib::object_id::PrefixResolution::NoMatch => {
                color_eyre::eyre::bail!("no operation matches prefix `{op_id_hex}`");
            }
        };

        // Read the operation and its parent.
        let op_data = op_store
            .read_operation(&op_id)
            .block_on()
            .wrap_err("failed to read operation")?;
        let Some(parent_id) = op_data.parents.first() else {
            // Root operation: nothing to diff.
            return Ok(Vec::new());
        };
        let parent_data = op_store
            .read_operation(parent_id)
            .block_on()
            .wrap_err("failed to read parent operation")?;
        let parent_view = op_store
            .read_view(&parent_data.view_id)
            .block_on()
            .wrap_err("failed to read parent view")?;
        let current_view = op_store
            .read_view(&op_data.view_id)
            .block_on()
            .wrap_err("failed to read current view")?;

        let mut lines = Vec::new();
        let store = self.repo.store();

        // Build prefix index for shortest unique ID computation.
        let prefix_index = self.id_prefix_index()?;

        // --- Changed commits ---
        // Walk all commits reachable from new heads but not old (added),
        // and vice versa (removed). This matches jj op show behavior.
        let new_heads: Vec<_> = current_view.head_ids.iter().cloned().collect();
        let old_heads: Vec<_> = parent_view.head_ids.iter().cloned().collect();
        let added_commits: Vec<BackendCommitId> =
            jj_lib::revset::walk_revs(self.repo.as_ref(), &new_heads, &old_heads)
                .inspect_err(|e| tracing::warn!("failed to walk added commits: {e}"))
                .and_then(|revset| revset.stream().try_collect::<Vec<_>>().block_on())
                .unwrap_or_default();
        let removed_commits: Vec<BackendCommitId> =
            jj_lib::revset::walk_revs(self.repo.as_ref(), &old_heads, &new_heads)
                .inspect_err(|e| tracing::warn!("failed to walk removed commits: {e}"))
                .and_then(|revset| revset.stream().try_collect::<Vec<_>>().block_on())
                .unwrap_or_default();

        if !added_commits.is_empty() || !removed_commits.is_empty() {
            lines.push(OpDetailLine::SectionHeader("Changed commits:".into()));
            for commit_id in &added_commits {
                let (change_id, commit_id, desc) =
                    self.short_commit_info(store, commit_id, &prefix_index);
                lines.push(OpDetailLine::Commit(OpDiffCommit {
                    change_id,
                    commit_id,
                    description: desc,
                    kind: DiffKind::Added,
                }));
            }
            for commit_id in &removed_commits {
                let (change_id, commit_id, desc) =
                    self.short_commit_info(store, commit_id, &prefix_index);
                lines.push(OpDetailLine::Commit(OpDiffCommit {
                    change_id,
                    commit_id,
                    description: desc,
                    kind: DiffKind::Removed,
                }));
            }
        }

        // Helper: build a ShortId for a commit ID using the prefix index.
        let short_commit_id = |id: &BackendCommitId| -> ShortId {
            let mut short = ShortId::new(id.hex());
            if let Ok(len) = prefix_index.shortest_commit_prefix_len(self.repo.as_ref(), id) {
                short.set_prefix_len(len);
            }
            short
        };

        // --- Changed working copies ---
        let mut wc_changed = false;
        for (ws, new_id) in &current_view.wc_commit_ids {
            let old_id = parent_view.wc_commit_ids.get(ws);
            if old_id != Some(new_id) {
                if !wc_changed {
                    lines.push(OpDetailLine::SectionHeader("Changed working copy:".into()));
                    wc_changed = true;
                }
                lines.push(OpDetailLine::WorkingCopy(OpDiffWorkingCopy {
                    workspace: WorkspaceName::new(ws.as_str()),
                    new_commit: Some(short_commit_id(new_id)),
                    old_commit: old_id.map(&short_commit_id),
                }));
            }
        }
        // Removed workspaces.
        for (ws, old_id) in &parent_view.wc_commit_ids {
            if !current_view.wc_commit_ids.contains_key(ws) {
                if !wc_changed {
                    lines.push(OpDetailLine::SectionHeader("Changed working copy:".into()));
                    wc_changed = true;
                }
                lines.push(OpDetailLine::WorkingCopy(OpDiffWorkingCopy {
                    workspace: WorkspaceName::new(ws.as_str()),
                    new_commit: None,
                    old_commit: Some(short_commit_id(old_id)),
                }));
            }
        }

        // --- Changed local bookmarks ---
        let mut bm_changed = false;
        let all_bm_names: std::collections::BTreeSet<_> = current_view
            .local_bookmarks
            .keys()
            .chain(parent_view.local_bookmarks.keys())
            .collect();
        for name in all_bm_names {
            let cur = current_view.local_bookmarks.get(name);
            let prev = parent_view.local_bookmarks.get(name);
            if cur == prev {
                continue;
            }
            if !bm_changed {
                lines.push(OpDetailLine::SectionHeader("Changed bookmarks:".into()));
                bm_changed = true;
            }
            let new_target = cur.and_then(|t| t.as_normal()).map(&short_commit_id);
            let old_target = prev.and_then(|t| t.as_normal()).map(&short_commit_id);
            lines.push(OpDetailLine::Bookmark(OpDiffBookmark {
                name: Str::from(name.as_str()),
                new_target,
                old_target,
            }));
        }

        Ok(lines)
    }

    /// Get change ID, short commit hex, and description for display.
    fn short_commit_info(
        &self,
        store: &Arc<jj_lib::store::Store>,
        commit_id: &BackendCommitId,
        prefix_index: &jj_lib::id_prefix::IdPrefixIndex,
    ) -> (ShortId, ShortId, Option<String>) {
        let repo = self.repo.as_ref();
        let commit = store
            .get_commit(commit_id)
            .inspect_err(|e| tracing::warn!("failed to load commit for op diff: {e}"))
            .ok();

        let change_id = commit
            .as_ref()
            .map(|c| {
                let mut id = ShortId::new(c.change_id().reverse_hex());
                // Leave the placeholder width on failure rather than assert a
                // prefix length we did not compute.
                match prefix_index.shortest_change_prefix_len(repo, c.change_id()) {
                    Ok(len) => id.set_prefix_len(len),
                    Err(e) => tracing::warn!("change prefix computation failed: {e}"),
                }
                id
            })
            // No commit behind this entry; render nothing rather than a stub ID.
            .unwrap_or_else(|| ShortId::new(""));

        let mut short_commit = ShortId::new(commit_id.hex());
        match prefix_index.shortest_commit_prefix_len(repo, commit_id) {
            Ok(len) => short_commit.set_prefix_len(len),
            Err(e) => tracing::warn!("commit prefix computation failed: {e}"),
        }

        let desc = commit.map(|c| {
            let is_empty = c
                .is_empty(self.repo.as_ref())
                .block_on()
                .inspect_err(|e| tracing::warn!("is_empty check failed: {e}"))
                .unwrap_or(false);
            let raw = c.description().trim().to_string();
            let first_line = if raw.is_empty() {
                "(no description set)".to_string()
            } else {
                raw.lines().next().unwrap_or_default().to_string()
            };
            if is_empty {
                format!("(empty) {first_line}")
            } else {
                first_line
            }
        });
        (change_id, short_commit, desc)
    }
}
