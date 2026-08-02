use std::collections::{HashMap, HashSet};

use color_eyre::Result;
use color_eyre::eyre::Context;
use futures::TryStreamExt as _;
use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::graph::{GraphEdgeType, TopoGroupedGraph};
use jj_lib::object_id::ObjectId;
use jj_lib::ref_name::RefName;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPathUiConverter;
use jj_lib::revset::{RevsetDiagnostics, RevsetParseContext, SymbolResolver};
use pollster::FutureExt as _;

use super::JjRepo;
use crate::dag::{DagEntry, Edge, EdgeKind, RemoteBookmarkInfo, RevsetResult};
use crate::types::{BookmarkName, CommitId as UiCommitId, RemoteName, WorkspaceName};

use super::commit_info::CommitContext;

/// Entries in the first chunk of a streamed revset load — kept small so the
/// first screenful appears quickly.
const FIRST_CHUNK_SIZE: usize = 200;
/// Entries per subsequent chunk of a streamed revset load.
const CHUNK_SIZE: usize = 2000;

impl JjRepo {
    /// Resolve a revset to a single commit's hex ID.
    pub fn resolve_single_commit(&self, revset_str: &str) -> Result<String> {
        let repo = self.repo.as_ref();
        let context = self.revset_parse_context();
        let mut diagnostics = RevsetDiagnostics::new();
        let parsed = jj_lib::revset::parse(&mut diagnostics, revset_str, &context)
            .wrap_err_with(|| format!("failed to parse revset: {revset_str}"))?;
        let symbol_resolver = SymbolResolver::new(
            repo,
            &[] as &[Box<dyn jj_lib::revset::SymbolResolverExtension>],
        );
        let resolved = parsed
            .resolve_user_expression(repo, &symbol_resolver)
            .wrap_err("failed to resolve revset symbols")?;
        let revset = resolved
            .evaluate(repo)
            .wrap_err("failed to evaluate revset")?;
        let mut ids: Vec<BackendCommitId> = revset.stream().try_collect().block_on()?;
        match ids.len() {
            0 => color_eyre::eyre::bail!("revset '{revset_str}' matched no commits"),
            1 => Ok(ids.remove(0).hex()),
            n => color_eyre::eyre::bail!("revset '{revset_str}' matched {n} commits, expected 1"),
        }
    }

    /// Convert a filesystem path to a repo-internal path string.
    pub fn parse_file_path(&self, input: &str) -> Result<String> {
        let path_converter = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let repo_path = path_converter
            .parse_file_path(input)
            .map_err(|e| color_eyre::eyre::eyre!("invalid file path '{input}': {e}"))?;
        Ok(repo_path.as_internal_file_string().to_string())
    }

    /// Evaluate a revset string and return DAG entries in topological order
    /// with graph edges for rendering.
    pub fn evaluate_revset(&self, revset_str: &str) -> Result<RevsetResult> {
        let mut entries = Vec::new();
        let warnings =
            self.evaluate_revset_streaming(revset_str, |mut chunk, _done, _warnings| {
                entries.append(&mut chunk);
                true
            })?;
        Ok(RevsetResult { entries, warnings })
    }

    /// Evaluate a revset string and deliver DAG entries in chunks via
    /// `on_chunk(entries, done, warnings)`, so the first screenful can be
    /// shown before the full (possibly huge) revset finishes loading.
    ///
    /// The first chunk holds [`FIRST_CHUNK_SIZE`] entries, later ones
    /// [`CHUNK_SIZE`]. `done` is `true` on the final chunk (which may be
    /// empty). `warnings` is complete before the first call. Returning
    /// `false` from `on_chunk` stops the stream early (cancellation).
    ///
    /// Errors are returned only for setup failures (parse/resolve/evaluate)
    /// and stream failures; chunks delivered before a stream failure remain
    /// valid.
    pub fn evaluate_revset_streaming(
        &self,
        revset_str: &str,
        mut on_chunk: impl FnMut(Vec<DagEntry>, bool, &[String]) -> bool,
    ) -> Result<Vec<String>> {
        let repo = self.repo.as_ref();

        // Shared context pieces
        let context = self.revset_parse_context();

        // Parse -> Resolve -> Evaluate the user's revset
        let mut diagnostics = RevsetDiagnostics::new();
        let parsed = jj_lib::revset::parse(&mut diagnostics, revset_str, &context)
            .wrap_err_with(|| format!("failed to parse revset: {revset_str}"))?;

        let symbol_resolver = SymbolResolver::new(
            repo,
            &[] as &[Box<dyn jj_lib::revset::SymbolResolverExtension>],
        );
        let resolved = parsed
            .resolve_user_expression(repo, &symbol_resolver)
            .wrap_err("failed to resolve revset symbols")?;
        let revset = resolved
            .evaluate(repo)
            .wrap_err("failed to evaluate revset")?;

        // Evaluate the immutable() revset for tagging commits.
        // The evaluated revset must stay alive for containing_fn() to borrow from.
        let mut warnings = Vec::new();
        let immutable_revset = self.evaluate_immutable(&context, &symbol_resolver, &mut warnings);
        let is_immutable = immutable_revset.as_ref().map(|r| r.containing_fn());

        // Wrap the graph stream with TopoGroupedGraph for proper
        // branch grouping, then prioritize branches matching the config
        // (default: present(@)) so they appear on the leftmost column.
        let graph_stream = revset.stream_graph();
        let mut topo_iter = TopoGroupedGraph::new(graph_stream, |id: &BackendCommitId| id);

        // Evaluate the log-graph-prioritize revset and call prioritize_branch()
        // only for commits that are actually in the log revset.
        let prioritize_revset = self.evaluate_prioritize(&context, &symbol_resolver, &mut warnings);
        if let Some(ref prio) = prioritize_revset {
            let is_in_log = revset.containing_fn();
            let prio_ids: Vec<BackendCommitId> = prio.stream().try_collect().block_on()?;
            for commit_id in prio_ids {
                if is_in_log(&commit_id).unwrap_or(false) {
                    topo_iter.prioritize_branch(commit_id);
                }
            }
        }

        // ID prefix disambiguation is deferred to a background thread
        // (see compute_prefix_lengths) to avoid evaluating a second revset
        // during the initial load. We use DISPLAY_ID_LEN as a placeholder.

        // Pre-build sets of local bookmarks that (a) have any tracked remote,
        // and (b) differ from their tracked remote counterpart.
        // O(M) once, then O(1) per bookmark lookup.
        let mut tracking_bookmarks: HashSet<&RefName> = HashSet::new();
        let mut dirty_bookmarks: HashSet<&RefName> = HashSet::new();
        for (symbol, remote_ref) in repo.view().all_remote_bookmarks() {
            if remote_ref.is_tracked() {
                tracking_bookmarks.insert(symbol.name);
                if *repo.view().get_local_bookmark(symbol.name) != remote_ref.target {
                    dirty_bookmarks.insert(symbol.name);
                }
            }
        }

        // Pre-build a map from commit ID to remote bookmarks pointing at it.
        // Each entry includes a `synced` flag indicating whether the remote
        // target matches the local target (matching jj's collect_distinct_refs).
        // O(M) once per refresh, then O(1) per commit lookup.
        let mut remote_bookmark_map: HashMap<BackendCommitId, Vec<RemoteBookmarkInfo>> =
            HashMap::new();
        for (symbol, remote_ref) in repo.view().all_remote_bookmarks() {
            if let Some(commit_id) = remote_ref.target.as_normal() {
                let synced = remote_ref.is_tracked()
                    && *repo.view().get_local_bookmark(symbol.name) == remote_ref.target;
                remote_bookmark_map
                    .entry(commit_id.clone())
                    .or_default()
                    .push(RemoteBookmarkInfo {
                        name: BookmarkName::new(symbol.name.as_str()),
                        remote: RemoteName::new(symbol.remote.as_str()),
                        synced,
                        is_tracked: remote_ref.is_tracked(),
                    });
            }
        }

        // Pre-build workspace → commit reverse map (O(W) once, O(1) per commit).
        let mut wc_commit_workspaces: HashMap<
            &BackendCommitId,
            Vec<crate::dag::WorkspaceAnnotation>,
        > = HashMap::new();
        for (ws_name, commit_id) in repo.view().wc_commit_ids() {
            wc_commit_workspaces.entry(commit_id).or_default().push(
                crate::dag::WorkspaceAnnotation {
                    name: WorkspaceName::new(ws_name.as_str()),
                    is_current: *ws_name == self.workspace_name,
                },
            );
        }

        let ctx = CommitContext {
            dirty_bookmarks: &dirty_bookmarks,
            tracking_bookmarks: &tracking_bookmarks,
            remote_bookmark_map: &remote_bookmark_map,
            wc_commit_workspaces: &wc_commit_workspaces,
        };

        // Iterate graph nodes lazily, delivering chunks as they fill up.
        let stream = topo_iter.stream();
        futures::pin_mut!(stream);
        let mut entries = Vec::new();
        let mut chunk_limit = FIRST_CHUNK_SIZE;
        while let Some((commit_id, edges)) = stream
            .try_next()
            .block_on()
            .wrap_err("failed to read graph stream")?
        {
            let commit = repo
                .store()
                .get_commit(&commit_id)
                .wrap_err("failed to load commit")?;

            let immutable = is_immutable
                .as_ref()
                .and_then(|check| {
                    check(&commit_id)
                        .inspect_err(|e| tracing::warn!("immutability check failed: {e}"))
                        .ok()
                })
                .unwrap_or(false);

            let info = self.extract_commit_info(&commit, immutable, &ctx)?;
            let dag_edges: Vec<Edge> = edges
                .into_iter()
                .map(|e| Edge {
                    target: UiCommitId::new(e.target.hex()),
                    kind: match e.edge_type {
                        GraphEdgeType::Direct => EdgeKind::Direct,
                        GraphEdgeType::Indirect => EdgeKind::Indirect,
                        GraphEdgeType::Missing => EdgeKind::Missing,
                    },
                })
                .collect();

            entries.push(DagEntry {
                commit: info,
                edges: dag_edges,
            });

            if entries.len() >= chunk_limit {
                if !on_chunk(std::mem::take(&mut entries), false, &warnings) {
                    return Ok(warnings);
                }
                chunk_limit = CHUNK_SIZE;
            }
        }

        on_chunk(entries, true, &warnings);
        Ok(warnings)
    }

    /// Return the `revsets.log-graph-prioritize` revset from config, falling
    /// back to `"present(@)"`.
    fn prioritize_revset_str(&self) -> String {
        self.settings
            .config()
            .get::<String>("revsets.log-graph-prioritize")
            .unwrap_or_else(|_| "present(@)".to_string())
    }

    /// Evaluate the graph-prioritize revset. Returns `None` if it can't be
    /// evaluated; appends a warning on failure.
    fn evaluate_prioritize(
        &self,
        context: &RevsetParseContext<'_>,
        symbol_resolver: &SymbolResolver,
        warnings: &mut Vec<String>,
    ) -> Option<Box<dyn jj_lib::revset::Revset + '_>> {
        let repo = self.repo.as_ref();
        let mut diagnostics = RevsetDiagnostics::new();
        let revset_str = self.prioritize_revset_str();

        let parsed = match jj_lib::revset::parse(&mut diagnostics, &revset_str, context) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("failed to parse prioritize revset `{revset_str}`: {e}");
                tracing::warn!("{msg}");
                warnings.push(msg);
                return None;
            }
        };
        let resolved = match parsed.resolve_user_expression(repo, symbol_resolver) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("failed to resolve prioritize revset: {e}");
                tracing::warn!("{msg}");
                warnings.push(msg);
                return None;
            }
        };
        match resolved.evaluate(repo) {
            Ok(r) => Some(r),
            Err(e) => {
                let msg = format!("failed to evaluate prioritize revset: {e}");
                tracing::warn!("{msg}");
                warnings.push(msg);
                None
            }
        }
    }

    /// Evaluate the `immutable()` revset. Returns `None` if it can't be
    /// evaluated (e.g. alias not defined); appends a warning on failure.
    fn evaluate_immutable(
        &self,
        context: &RevsetParseContext<'_>,
        symbol_resolver: &SymbolResolver,
        warnings: &mut Vec<String>,
    ) -> Option<Box<dyn jj_lib::revset::Revset + '_>> {
        let repo = self.repo.as_ref();
        let mut diagnostics = RevsetDiagnostics::new();

        let parsed = match jj_lib::revset::parse(&mut diagnostics, "immutable()", context) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("failed to parse immutable() revset: {e}");
                tracing::warn!("{msg}");
                warnings.push(msg);
                return None;
            }
        };
        let resolved = match parsed.resolve_user_expression(repo, symbol_resolver) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("failed to resolve immutable() revset: {e}");
                tracing::warn!("{msg}");
                warnings.push(msg);
                return None;
            }
        };
        match resolved.evaluate(repo) {
            Ok(r) => Some(r),
            Err(e) => {
                let msg = format!("failed to evaluate immutable() revset: {e}");
                tracing::warn!("{msg}");
                warnings.push(msg);
                None
            }
        }
    }
}
