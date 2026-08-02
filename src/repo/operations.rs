use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::Context;
use futures::TryStreamExt as _;
use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::object_id::ObjectId;
use jj_lib::repo::Repo;
use pollster::FutureExt as _;

use super::{JjRepo, parse_first_line_description};
use crate::dag::{Edge, EdgeKind, ShortId};
use crate::types::{CommitId as UiCommitId, OperationId, Str, WorkspaceName};

impl JjRepo {
    /// Walk the operation log and return entries in reverse chronological order.
    /// Returns at most `limit` entries and a flag indicating whether more exist.
    pub fn operation_log(&self, limit: usize) -> Result<(Vec<crate::app::OpLogEntry>, bool)> {
        use futures::StreamExt as _;

        // Load one extra to detect whether more exist, then truncate.
        let fetch_limit = limit + 1;
        let current_op = self.repo.operation().clone();
        let current_op_id = current_op.id().hex();
        let stream = jj_lib::op_walk::walk_ancestors(&[current_op]);

        // Collect raw data + edges for graph rendering.
        struct RawOp {
            full_id: String,
            display_id: Str,
            description: Str,
            relative_time: Str,
            workspace: Option<Str>,
            user: Str,
            args: Option<Str>,
            is_snapshot: bool,
            is_current: bool,
            edges: Vec<Edge>,
        }

        let mut raw_entries = Vec::new();
        let mut stream = std::pin::pin!(stream);
        while let Some(result) = stream.next().block_on() {
            let op = result.wrap_err("failed to read operation")?;
            let meta = op.metadata();
            let id_hex = op.id().hex();
            let is_current = id_hex == current_op_id;

            let relative_time = millis_to_relative_time(meta.time.start.timestamp.0);

            let workspace: Option<Str> = meta.workspace_name.as_ref().map(|ws| ws.as_str().into());
            let user: Str = if meta.hostname.is_empty() {
                meta.username.as_str().into()
            } else {
                format!("{}@{}", meta.username, meta.hostname).into()
            };
            let display_id: Str = id_hex[..id_hex.len().min(12)].into();
            let args: Option<Str> = meta.attributes.get("args").map(|s| s.as_str().into());

            // Build edges from parent operation IDs.
            let edges: Vec<Edge> = op
                .parent_ids()
                .iter()
                .map(|pid| Edge {
                    target: UiCommitId::new(pid.hex()),
                    kind: EdgeKind::Direct,
                })
                .collect();

            raw_entries.push(RawOp {
                full_id: id_hex,
                display_id,
                description: meta.description.as_str().into(),
                relative_time,
                workspace,
                user,
                args,
                is_snapshot: meta.is_snapshot,
                is_current,
                edges,
            });
            if raw_entries.len() >= fetch_limit {
                break;
            }
        }

        let has_more = raw_entries.len() > limit;
        raw_entries.truncate(limit);

        // Mark edges to ops outside the loaded set as Missing.
        let loaded_ids: HashSet<String> = raw_entries.iter().map(|e| e.full_id.clone()).collect();
        for entry in &mut raw_entries {
            for edge in &mut entry.edges {
                if !loaded_ids.contains(edge.target.as_str()) {
                    edge.kind = EdgeKind::Missing;
                }
            }
        }

        // Render graph lines.
        let graph_input: Vec<(&str, &[Edge], char)> = raw_entries
            .iter()
            .map(|e| {
                let glyph = if e.is_current { '@' } else { '○' };
                (e.full_id.as_str(), e.edges.as_slice(), glyph)
            })
            .collect();
        let graph_lines = crate::graph::render_generic(&graph_input);

        // Build final entries with graph lines attached.
        let entries = raw_entries
            .into_iter()
            .zip(graph_lines)
            .map(|(raw, graph)| crate::app::OpLogEntry {
                id: OperationId::new(raw.display_id),
                description: raw.description,
                relative_time: raw.relative_time,
                workspace: raw.workspace.map(WorkspaceName::new),
                user: raw.user,
                args: raw.args,
                is_snapshot: raw.is_snapshot,
                is_current: raw.is_current,
                graph,
            })
            .collect();

        Ok((entries, has_more))
    }

    /// Load the evolution log (predecessor chain) for a commit.
    pub fn evolution_log(&self, commit_id_hex: &str) -> Result<Vec<crate::app::EvoLogEntry>> {
        let repo = self.repo.as_ref();
        let commit_id = BackendCommitId::try_from_hex(commit_id_hex)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit id hex"))?;

        // Build ID prefix context for disambiguation.
        let prefix_index = self.id_prefix_index()?;

        struct RawEntry {
            full_id: String,
            change_id: ShortId,
            description: Option<String>,
            author: Str,
            relative_time: Str,
            op_description: Option<Str>,
            is_current: bool,
            predecessor_ids: Vec<UiCommitId>,
            edges: Vec<Edge>,
        }

        let mut raw_entries = Vec::new();
        let mut is_first = true;
        let predecessor_entries: Vec<_> = jj_lib::evolution::walk_predecessors(repo, &[commit_id])
            .try_collect()
            .block_on()?;
        for entry in predecessor_entries {
            let commit = &entry.commit;

            let change_id = Self::short_change_id(&prefix_index, repo, commit);

            let description = parse_first_line_description(commit.description());

            let sig = commit.author();
            let author: Str = if sig.name.is_empty() {
                sig.email.as_str().into()
            } else {
                sig.name.as_str().into()
            };

            let relative_time = millis_to_relative_time(sig.timestamp.timestamp.0);

            let op_description: Option<Str> = entry
                .operation
                .as_ref()
                .map(|op| op.metadata().description.as_str().into());

            let full_id = commit.id().hex();
            let pred_ids: Vec<UiCommitId> = entry
                .predecessor_ids()
                .iter()
                .map(|pid| UiCommitId::new(pid.hex()))
                .collect();
            let edges: Vec<Edge> = pred_ids
                .iter()
                .map(|pid| Edge {
                    target: pid.clone(),
                    kind: EdgeKind::Direct,
                })
                .collect();

            let is_current = is_first;
            is_first = false;

            raw_entries.push(RawEntry {
                full_id,
                change_id,
                description,
                author,
                relative_time,
                op_description,
                is_current,
                predecessor_ids: pred_ids,
                edges,
            });
        }

        let loaded_ids: HashSet<String> = raw_entries.iter().map(|e| e.full_id.clone()).collect();
        for entry in &mut raw_entries {
            for edge in &mut entry.edges {
                if !loaded_ids.contains(edge.target.as_str()) {
                    edge.kind = EdgeKind::Missing;
                }
            }
        }

        let graph_input: Vec<(&str, &[Edge], char)> = raw_entries
            .iter()
            .map(|e| {
                let glyph = if e.is_current { '@' } else { '○' };
                (e.full_id.as_str(), e.edges.as_slice(), glyph)
            })
            .collect();
        let graph_lines = crate::graph::render_generic(&graph_input);

        let entries = raw_entries
            .into_iter()
            .zip(graph_lines)
            .map(|(raw, graph)| crate::app::EvoLogEntry {
                commit_id: UiCommitId::new(raw.full_id),
                change_id: raw.change_id,
                description: raw.description,
                author: raw.author,
                relative_time: raw.relative_time,
                op_description: raw.op_description,
                is_current: raw.is_current,
                predecessor_ids: raw.predecessor_ids,
                graph,
            })
            .collect();

        Ok(entries)
    }

    /// Compute the diff between an operation and its parent.
    pub fn op_diff(&self, op_id_hex: &str) -> Result<Vec<crate::app::OpDetailLine>> {
        use crate::app::{OpDetailLine, OpDiffBookmark, OpDiffCommit, OpDiffWorkingCopy};
        use crate::dag::DiffKind;

        let op_store = self.repo.op_store();
        let prefix = jj_lib::object_id::HexPrefix::try_from_hex(op_id_hex)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid operation ID hex"))?;
        let resolution = op_store
            .resolve_operation_id_prefix(&prefix)
            .block_on()
            .wrap_err("failed to resolve operation ID prefix")?;
        let op_id = match resolution {
            jj_lib::object_id::PrefixResolution::SingleMatch(id) => id,
            jj_lib::object_id::PrefixResolution::AmbiguousMatch => {
                color_eyre::eyre::bail!("ambiguous operation ID prefix: {op_id_hex}");
            }
            jj_lib::object_id::PrefixResolution::NoMatch => {
                color_eyre::eyre::bail!("no operation matches prefix: {op_id_hex}");
            }
        };

        // Read the operation and its parent.
        let op_data = op_store
            .read_operation(&op_id)
            .block_on()
            .wrap_err("failed to read operation")?;
        let Some(parent_id) = op_data.parents.first() else {
            // Root operation — nothing to diff.
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

/// Format a jj timestamp as an absolute date string (e.g. "2026-05-15 13:11:51 +02:00").
pub(super) fn format_absolute_time(ts: &jj_lib::backend::Timestamp) -> String {
    let secs = ts.timestamp.0 / 1000;
    let nanos = ((ts.timestamp.0 % 1000) * 1_000_000) as u32;
    let offset_secs = ts.tz_offset * 60;
    match chrono::DateTime::from_timestamp(secs, nanos) {
        Some(utc) => {
            let offset = chrono::FixedOffset::east_opt(offset_secs)
                .unwrap_or(chrono::FixedOffset::east_opt(0).unwrap());
            utc.with_timezone(&offset)
                .format("%Y-%m-%d %H:%M:%S %:z")
                .to_string()
        }
        None => "unknown".to_string(),
    }
}

pub fn millis_to_relative_time(millis: i64) -> Str {
    let secs = millis / 1000;
    let nanos = ((millis % 1000) * 1_000_000) as u32;
    match chrono::DateTime::from_timestamp(secs, nanos) {
        Some(dt) => format_relative_time(dt).into(),
        None => "unknown".into(),
    }
}

fn format_relative_time(dt: chrono::DateTime<chrono::Utc>) -> Cow<'static, str> {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(dt);

    if duration.num_seconds() < 0 {
        return "just now".into();
    }

    let secs = duration.num_seconds();
    if secs < 60 {
        return if secs == 1 {
            "1 second ago".into()
        } else {
            format!("{secs} seconds ago").into()
        };
    }
    let mins = duration.num_minutes();
    if mins < 60 {
        return if mins == 1 {
            "1 minute ago".into()
        } else {
            format!("{mins} minutes ago").into()
        };
    }
    let hours = duration.num_hours();
    if hours < 24 {
        return if hours == 1 {
            "1 hour ago".into()
        } else {
            format!("{hours} hours ago").into()
        };
    }
    let days = duration.num_days();
    if days < 30 {
        return if days == 1 {
            "1 day ago".into()
        } else {
            format!("{days} days ago").into()
        };
    }
    let months = days / 30;
    if months < 12 {
        return if months == 1 {
            "1 month ago".into()
        } else {
            format!("{months} months ago").into()
        };
    }
    let years = days / 365;
    if years == 1 {
        "1 year ago".into()
    } else {
        format!("{years} years ago").into()
    }
}
