use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::Context;
use color_eyre::Result;
use futures::{StreamExt as _, TryStreamExt as _};
use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::commit::Commit;
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::graph::{GraphEdgeType, GraphNode, TopoGroupedGraph};
use jj_lib::id_prefix::IdPrefixContext;
use jj_lib::matchers::EverythingMatcher;
use jj_lib::object_id::ObjectId;
use jj_lib::ref_name::{RefName, WorkspaceNameBuf};
use jj_lib::repo::{ReadonlyRepo, Repo, StoreFactories};
use jj_lib::repo_path::RepoPathUiConverter;
use jj_lib::revset::{
    RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
    RevsetWorkspaceContext, SymbolResolver,
};
use jj_lib::settings::UserSettings;
use jj_lib::time_util::DatePatternContext;
use jj_lib::workspace::{default_working_copy_factories, Workspace};
use pollster::FutureExt as _;

use jj_lib::conflict_labels::ConflictLabels;
use jj_lib::conflicts::{
    materialize_tree_value, try_materialize_file_conflict_value, ConflictMaterializeOptions,
};
use jj_lib::diff_presentation::unified::{self, git_diff_part, DiffLineType};
use jj_lib::diff_presentation::DiffTokenType;
use jj_lib::merge::{Diff, Merge};
use jj_lib::merged_tree::MergedTree;
use jj_lib::repo_path::RepoPathBuf;

use crate::dag::{
    AuthorInfo, BookmarkInfo, CommitDetails, CommitInfo, DagEntry, DiffLine, DiffLineKind,
    DiffResult, DivergenceUpdate, Edge, EdgeKind, FileChange, FileStatus, LineStats,
    PrefixLengthUpdate, RemoteBookmarkInfo, RevsetResult, ShortId,
};
use crate::types::{
    BookmarkName, CommitId as UiCommitId, OperationId, RemoteName, RepoPath, Str, TagName,
    WorkspaceName,
};

/// Number of hex characters to show for change/commit IDs.
const DISPLAY_ID_LEN: usize = 8;

/// Vendored jj-cli default revset configuration.
/// Contains `[revsets]` (default log revset, etc.) and `[revset-aliases]`
/// (trunk(), immutable_heads(), immutable(), mutable(), etc.).
const DEFAULT_REVSETS_TOML: &str = include_str!("../vendored/revsets.toml");

/// Thin adapter around jj-lib. Owns the workspace and repo, converts
/// jj-lib types into our domain types so nothing leaks out.
pub struct JjRepo {
    repo: Arc<ReadonlyRepo>,
    /// User settings (for user_email, config access).
    settings: UserSettings,
    /// Revset aliases loaded from vendored defaults + user/repo config.
    aliases_map: RevsetAliasesMap,
    /// Workspace name, needed for `@` resolution.
    workspace_name: WorkspaceNameBuf,
    /// Root path of the workspace (for display / path conversion).
    workspace_root: PathBuf,
}

/// Pre-built lookup tables passed to `extract_commit_info` for each commit.
struct CommitContext<'a> {
    dirty_bookmarks: &'a HashSet<&'a RefName>,
    tracking_bookmarks: &'a HashSet<&'a RefName>,
    remote_bookmark_map: &'a HashMap<BackendCommitId, Vec<RemoteBookmarkInfo>>,
    wc_commit_workspaces: &'a HashMap<&'a BackendCommitId, Vec<crate::dag::WorkspaceAnnotation>>,
}

struct CommitDetailInfo {
    change_id: ShortId,
    short_commit_id: ShortId,
    description: Option<String>,
    change_id_suffix: Option<usize>,
    is_hidden: bool,
}

impl JjRepo {
    /// Trigger a working copy snapshot so the repo reflects the current
    /// filesystem state. Shells out to `jj status` which snapshots as a
    /// side effect.
    pub fn snapshot(repo_path: &Path) -> Result<(), String> {
        let output = std::process::Command::new("jj")
            .arg("status")
            .arg("-R")
            .arg(repo_path)
            .arg("--quiet")
            .arg("--color=never")
            .output()
            .map_err(|e| format!("failed to run jj: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(stderr)
        }
    }

    /// Run `jj workspace update-stale` to recover a stale working copy.
    pub fn update_stale(repo_path: &Path) -> Result<(), String> {
        let output = std::process::Command::new("jj")
            .arg("workspace")
            .arg("update-stale")
            .arg("-R")
            .arg(repo_path)
            .arg("--color=never")
            .output()
            .map_err(|e| format!("failed to run jj: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(stderr)
        }
    }

    /// Open the jj workspace rooted at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let config = Self::load_config(path)?;
        let settings = UserSettings::from_config(config)
            .wrap_err("failed to create jj settings from config")?;
        let aliases_map = Self::load_revset_aliases(&settings)?;
        let workspace = Workspace::load(
            &settings,
            path,
            &StoreFactories::default(),
            &default_working_copy_factories(),
        )
        .wrap_err_with(|| format!("failed to load jj workspace at {}", path.display()))?;

        let workspace_name = workspace.workspace_name().to_owned();
        let workspace_root = workspace.workspace_root().to_owned();
        let repo = workspace
            .repo_loader()
            .load_at_head()
            .block_on()
            .wrap_err("failed to load repo at HEAD")?;

        Ok(Self {
            repo,
            settings,
            aliases_map,
            workspace_name,
            workspace_root,
        })
    }

    /// Get a clone of the inner `Arc<ReadonlyRepo>` for background work.
    pub fn inner_repo(&self) -> Arc<ReadonlyRepo> {
        Arc::clone(&self.repo)
    }

    fn remote_bookmarks(
        &'_ self,
    ) -> impl Iterator<
        Item = (
            jj_lib::ref_name::RemoteRefSymbol<'_>,
            &'_ jj_lib::op_store::RemoteRef,
        ),
    > {
        // Exclude the synthetic `git` remote (colocated repos).
        self.repo
            .view()
            .all_remote_bookmarks()
            .filter(|(symbol, _)| symbol.remote.as_str() != "git")
    }

    /// All remote bookmarks (tracked and untracked) with structured data.
    pub fn all_remote_bookmark_refs(&self) -> Vec<crate::dag::RemoteBookmarkRef> {
        use crate::types::CommitId as UiCommitId;
        self.remote_bookmarks()
            .map(|(s, r)| crate::dag::RemoteBookmarkRef {
                name: BookmarkName::new(s.name.as_str()),
                remote: RemoteName::new(s.remote.as_str()),
                commit_id: r.target.as_normal().map(|id| UiCommitId::new(id.hex())),
                is_tracked: r.is_tracked(),
            })
            .collect()
    }

    /// Unique git remote names, sorted alphabetically.
    pub fn git_remotes(&self) -> Vec<RemoteName> {
        use std::collections::BTreeSet;
        self.remote_bookmarks()
            .map(|(s, _)| RemoteName::new(s.remote.as_str()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Build config stack: jj-lib defaults + vendored CLI defaults + user + repo.
    fn load_config(workspace_path: &Path) -> Result<StackedConfig> {
        let mut config = StackedConfig::with_defaults();

        // Add vendored jj-cli defaults (revset aliases, default log revset, etc.)
        // as a Default layer so user/repo config can override them.
        let cli_defaults = ConfigLayer::parse(ConfigSource::Default, DEFAULT_REVSETS_TOML)
            .wrap_err("failed to parse vendored revsets.toml")?;
        config.add_layer(cli_defaults);

        // Try loading user config (~/.config/jj/config.toml or platform equivalent)
        if let Some(config_dir) = dirs::config_dir() {
            let user_config = config_dir.join("jj").join("config.toml");
            if user_config.exists() {
                let _ = config.load_file(ConfigSource::User, &user_config);
            }
        }

        // Try loading repo config (.jj/repo/config.toml)
        let repo_config = workspace_path.join(".jj").join("repo").join("config.toml");
        if repo_config.exists() {
            let _ = config.load_file(ConfigSource::Repo, &repo_config);
        }

        Ok(config)
    }

    /// Build the revset aliases map from all config layers.
    ///
    /// Iterates config layers in precedence order. Higher-precedence layers
    /// (User, Repo) override lower ones (Default), matching jj-cli behavior.
    fn load_revset_aliases(settings: &UserSettings) -> Result<RevsetAliasesMap> {
        let mut aliases_map = RevsetAliasesMap::new();

        for layer in settings.config().layers() {
            let table = match layer.look_up_table("revset-aliases") {
                Ok(Some(table)) => table,
                Ok(None) => continue,
                Err(_) => continue, // not a table, skip
            };
            for (decl, item) in table.iter() {
                if let Some(defn) = item.as_str() {
                    // Silently ignore malformed declarations; they'll error
                    // when the alias is actually used in a revset.
                    let _ = aliases_map.insert(decl, defn);
                }
            }
        }

        Ok(aliases_map)
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Return the default log revset from config (`revsets.log`), falling back
    /// to a sensible built-in default if not configured.
    pub fn default_revset(&self) -> String {
        self.settings
            .config()
            .get::<String>("revsets.log")
            .unwrap_or_else(|_| {
                "present(@) | ancestors(immutable_heads()..@, 2) | ancestors(trunk(), 16)"
                    .to_string()
            })
    }

    /// Build a [`RevsetParseContext`] with loaded aliases and proper user email.
    fn revset_parse_context<'a>(
        &'a self,
        extensions: &'a RevsetExtensions,
        fileset_aliases_map: &'a FilesetAliasesMap,
        path_converter: &'a RepoPathUiConverter,
    ) -> RevsetParseContext<'a> {
        let workspace_ctx = RevsetWorkspaceContext {
            path_converter,
            workspace_name: &self.workspace_name,
        };
        RevsetParseContext {
            aliases_map: &self.aliases_map,
            local_variables: Default::default(),
            user_email: self.settings.user_email(),
            date_pattern_context: DatePatternContext::from(chrono::Local::now()),
            default_ignored_remote: None,
            fileset_aliases_map,
            use_glob_by_default: true,
            extensions,
            workspace: Some(workspace_ctx),
        }
    }

    /// Resolve a revset to a single commit's hex ID.
    pub fn resolve_single_commit(&self, revset_str: &str) -> Result<String> {
        let repo = self.repo.as_ref();
        let extensions = RevsetExtensions::default();
        let fileset_aliases_map = FilesetAliasesMap::new();
        let path_converter = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = self.revset_parse_context(&extensions, &fileset_aliases_map, &path_converter);
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
        let repo = self.repo.as_ref();

        // Shared context pieces
        let extensions = RevsetExtensions::default();
        let fileset_aliases_map = FilesetAliasesMap::new();
        let path_converter = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = self.revset_parse_context(&extensions, &fileset_aliases_map, &path_converter);

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
            // Collect log commit IDs only when needed for filtering.
            let log_commit_ids: std::collections::HashSet<BackendCommitId> = revset
                .stream()
                .try_collect::<Vec<_>>()
                .block_on()?
                .into_iter()
                .collect();
            let prio_ids: Vec<BackendCommitId> = prio.stream().try_collect().block_on()?;
            for commit_id in prio_ids {
                if log_commit_ids.contains(&commit_id) {
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
                    name: crate::types::WorkspaceName::new(ws_name.as_str()),
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

        // Iterate graph nodes
        let mut entries = Vec::new();
        let topo_nodes: Vec<GraphNode<BackendCommitId>> =
            topo_iter.stream().try_collect().block_on()?;
        for (commit_id, edges) in topo_nodes {
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
        }

        Ok(RevsetResult { entries, warnings })
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

    /// Compute the file-level changes and line totals for a commit.
    pub fn commit_details(&self, commit_id: &UiCommitId) -> Result<CommitDetails> {
        let repo = self.repo.as_ref();
        let commit_hex_id = commit_id.as_str();
        let commit_id = BackendCommitId::try_from_hex(commit_hex_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex: {commit_hex_id}"))?;
        let commit = repo
            .store()
            .get_commit(&commit_id)
            .wrap_err("failed to load commit for diff")?;

        let parent_tree = commit
            .parent_tree(repo)
            .block_on()
            .wrap_err("failed to get parent tree")?;
        let commit_tree = commit.tree();

        let mut changes = Vec::new();
        let mut stats = LineStats::default();
        let labels = ConflictLabels::unlabeled();
        let materialize_options = default_materialize_options();

        // Build copy records for rename/copy detection.
        let mut copy_records = jj_lib::copies::CopyRecords::default();
        for parent_id in commit.parent_ids() {
            match repo.store().get_copy_records(None, parent_id, commit.id()) {
                Ok(stream) => {
                    use futures::TryStreamExt as _;
                    let records: Vec<_> = stream
                        .try_collect()
                        .block_on()
                        .inspect_err(|e| tracing::warn!("failed to collect copy records: {e}"))
                        .unwrap_or_default();
                    copy_records.add_records(records);
                }
                Err(e) => {
                    tracing::warn!("failed to get copy records: {e}");
                }
            }
        }

        let mut diff_stream =
            parent_tree.diff_stream_with_copies(&commit_tree, &EverythingMatcher, &copy_records);

        let mut skipped_entries = 0u32;
        while let Some(entry) = diff_stream.next().block_on() {
            let target_path = entry.path.target().as_internal_file_string().to_string();
            let values = match entry.values {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("skipping diff entry for {target_path}: {e}");
                    skipped_entries += 1;
                    continue;
                }
            };

            let before_present = values.before.is_present();
            let after_present = values.after.is_present();

            let (status, old_path) = if let Some(copy_op) = entry.path.copy_operation() {
                match copy_op {
                    jj_lib::copies::CopyOperation::Rename => (
                        FileStatus::Renamed,
                        entry
                            .path
                            .source
                            .as_ref()
                            .map(|(p, _)| RepoPath::new(p.as_internal_file_string())),
                    ),
                    jj_lib::copies::CopyOperation::Copy => (
                        FileStatus::Copied,
                        entry
                            .path
                            .source
                            .as_ref()
                            .map(|(p, _)| RepoPath::new(p.as_internal_file_string())),
                    ),
                }
            } else {
                match (before_present, after_present) {
                    (false, true) => (FileStatus::Added, None),
                    (true, false) => (FileStatus::Deleted, None),
                    (true, true) => (FileStatus::Modified, None),
                    (false, false) => continue,
                }
            };

            let has_conflict = !values.after.is_resolved();
            changes.push(FileChange {
                path: RepoPath::new(&target_path),
                old_path: old_path.clone(),
                status,
                has_conflict,
                stats: LineStats::default(),
            });

            // For stats: materialize before content from old path if renamed/copied.
            let before_repo_path = old_path
                .as_ref()
                .and_then(|p| RepoPathBuf::from_internal_string(p.as_str()).ok())
                .unwrap_or_else(|| {
                    RepoPathBuf::from_internal_string(&target_path).expect("target path is valid")
                });
            let after_repo_path =
                RepoPathBuf::from_internal_string(&target_path).expect("target path is valid");

            let before_mat =
                materialize_tree_value(repo.store(), &before_repo_path, values.before, &labels)
                    .block_on()?;
            let after_mat =
                materialize_tree_value(repo.store(), &after_repo_path, values.after, &labels)
                    .block_on()?;

            let before_part = git_diff_part(&before_repo_path, before_mat, &materialize_options)
                .block_on()
                .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
            let after_part = git_diff_part(&after_repo_path, after_mat, &materialize_options)
                .block_on()
                .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;

            if before_part.content.is_binary || after_part.content.is_binary {
                continue;
            }

            let contents = Diff::new(
                before_part.content.contents.as_ref(),
                after_part.content.contents.as_ref(),
            );
            let hunks = unified::unified_diff_hunks(contents, 0, Default::default());
            let file_stats = count_line_stats(&hunks);
            stats.added = stats.added.saturating_add(file_stats.added);
            stats.removed = stats.removed.saturating_add(file_stats.removed);
            if let Some(fc) = changes.last_mut() {
                fc.stats = file_stats;
            }
        }

        if skipped_entries > 0 {
            tracing::warn!("skipped {skipped_entries} diff entries due to errors");
        }

        // Add any conflicted files not already found by the diff pass.
        if commit.has_conflict() {
            let existing: HashSet<RepoPath> = changes.iter().map(|c| c.path.clone()).collect();
            for (path, _) in commit_tree.conflicts() {
                let repo_path = RepoPath::new(path.as_internal_file_string());
                if !existing.contains(&repo_path) {
                    changes.push(FileChange {
                        path: repo_path,
                        old_path: None,
                        status: FileStatus::Modified,
                        has_conflict: true,
                        stats: LineStats::default(),
                    });
                }
            }
        }

        let is_empty = changes.is_empty();
        Ok(CommitDetails {
            files: changes,
            stats,
            is_empty,
        })
    }

    /// Compute the line-level diff for a single file in a commit.
    /// For renamed/copied files, `old_path` provides the source path to diff against.
    /// Returns (git_diff_lines, color_words_lines).
    pub fn file_diff(
        &self,
        commit_id: &UiCommitId,
        path: &RepoPath,
        old_path: Option<&RepoPath>,
    ) -> Result<DiffResult> {
        let repo = self.repo.as_ref();
        let commit_hex_id = commit_id.as_str();
        let commit_id = BackendCommitId::try_from_hex(commit_hex_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex: {commit_hex_id}"))?;
        let commit = repo
            .store()
            .get_commit(&commit_id)
            .wrap_err("failed to load commit for diff")?;

        let parent_tree = commit.parent_tree(repo).block_on()?;
        let commit_tree = commit.tree();
        let repo_path = RepoPathBuf::from_internal_string(path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;
        let before_repo_path = old_path
            .and_then(|p| RepoPathBuf::from_internal_string(p.as_str()).ok())
            .unwrap_or_else(|| repo_path.clone());
        let labels = ConflictLabels::unlabeled();
        let materialize_options = default_materialize_options();

        let before_value = parent_tree.path_value(&before_repo_path).block_on()?;
        let after_value = commit_tree.path_value(&repo_path).block_on()?;

        let before_mat =
            materialize_tree_value(repo.store(), &repo_path, before_value, &labels).block_on()?;
        let after_mat =
            materialize_tree_value(repo.store(), &repo_path, after_value, &labels).block_on()?;

        let before_part = git_diff_part(&repo_path, before_mat, &materialize_options)
            .block_on()
            .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
        let after_part = git_diff_part(&repo_path, after_mat, &materialize_options)
            .block_on()
            .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;

        if before_part.content.is_binary || after_part.content.is_binary {
            let line = DiffLine {
                kind: DiffLineKind::Header,
                content: "(binary file)".to_string(),
                tokens: vec![],
                old_line: None,
                new_line: None,
            };
            return Ok(DiffResult {
                git: vec![line.clone()],
                color_words: vec![line],
            });
        }

        let contents = Diff::new(
            before_part.content.contents.as_ref(),
            after_part.content.contents.as_ref(),
        );
        let hunks = unified::unified_diff_hunks(
            contents,
            3, // context lines
            Default::default(),
        );

        let mut git_lines = Vec::new();
        hunks_to_diff_lines(&hunks, &mut git_lines);
        let mut cw_lines = Vec::new();
        hunks_to_color_words_lines(&hunks, &mut cw_lines);
        Ok(DiffResult {
            git: git_lines,
            color_words: cw_lines,
        })
    }

    /// Get the conflict hunks for a conflicted file, broken down by hunk.
    /// Returns a list of resolved (context) and conflicted hunks.
    pub fn conflict_hunks(
        &self,
        commit_id: &UiCommitId,
        path: &RepoPath,
    ) -> Result<Vec<crate::dag::ConflictHunkKind>> {
        use crate::dag::ConflictHunkKind;

        let repo = self.repo.as_ref();
        let backend_id = BackendCommitId::try_from_hex(commit_id.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let commit = repo
            .store()
            .get_commit(&backend_id)
            .wrap_err("failed to load commit")?;
        let tree = commit.tree();
        let repo_path = RepoPathBuf::from_internal_string(path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;
        let tree_value = tree.path_value(&repo_path).block_on()?;
        let labels = ConflictLabels::unlabeled();

        let Some(materialized) =
            try_materialize_file_conflict_value(repo.store(), &repo_path, &tree_value, &labels)
                .block_on()
                .wrap_err("failed to materialize conflict")?
        else {
            return Ok(vec![]);
        };

        let merge_options = jj_lib::tree_merge::MergeOptions {
            hunk_level: jj_lib::files::FileMergeHunkLevel::Line,
            same_change: jj_lib::merge::SameChange::Accept,
        };
        let merge_result = jj_lib::files::merge_hunks(&materialized.contents, &merge_options);

        let hunks = match merge_result {
            jj_lib::files::MergeResult::Resolved(content) => {
                let lines = String::from_utf8_lossy(&content)
                    .lines()
                    .map(String::from)
                    .collect();
                vec![ConflictHunkKind::Resolved { lines }]
            }
            jj_lib::files::MergeResult::Conflict(merge_hunks) => {
                merge_hunks
                    .into_iter()
                    .map(|hunk| {
                        if let Some(resolved) = hunk.as_resolved() {
                            let lines = String::from_utf8_lossy(resolved.as_ref())
                                .lines()
                                .map(String::from)
                                .collect();
                            ConflictHunkKind::Resolved { lines }
                        } else {
                            // Collect each side's content as lines.
                            let sides: Vec<Vec<String>> = hunk
                                .iter()
                                .map(|side| {
                                    String::from_utf8_lossy(side.as_ref())
                                        .lines()
                                        .map(String::from)
                                        .collect()
                                })
                                .collect();
                            ConflictHunkKind::Conflict {
                                sides,
                                selected: None,
                            }
                        }
                    })
                    .collect()
            }
        };

        Ok(hunks)
    }

    fn extract_commit_info(
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

    /// All local tag names (including those outside the current revset).
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
        use crate::dag::{BookmarkDetails, ConflictTargetKind};

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
                    if let Some(ct) =
                        self.make_conflict_target(removed_id, ConflictTargetKind::Removed)
                    {
                        details.conflict_targets.push(ct);
                    }
                }
                for added_id in target.added_ids() {
                    if let Some(ct) = self.make_conflict_target(added_id, ConflictTargetKind::Added)
                    {
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

    /// Build an `IdPrefixContext` using the `revsets.short-prefixes` config.
    fn build_id_prefix_context(&self, context: &RevsetParseContext<'_>) -> IdPrefixContext {
        let short_prefixes_str = self
            .settings
            .config()
            .get::<String>("revsets.short-prefixes")
            .unwrap_or_else(|_| self.default_revset());
        let mut diag = RevsetDiagnostics::new();
        let ctx = IdPrefixContext::new(Arc::new(RevsetExtensions::default()));
        if let Ok(expression) = jj_lib::revset::parse(&mut diag, &short_prefixes_str, context) {
            ctx.disambiguate_within(expression)
        } else {
            ctx
        }
    }

    /// Build a `ShortId` for a commit's change ID using the prefix index.
    fn short_change_id(
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
    fn short_commit_id(
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
    fn commit_detail_info(&self, commit_id: &BackendCommitId) -> Option<CommitDetailInfo> {
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

    /// Build a `BookmarkConflictTarget` from a commit ID.
    fn make_conflict_target(
        &self,
        commit_id: &BackendCommitId,
        kind: crate::dag::ConflictTargetKind,
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

    /// Walk the operation log and return entries in reverse chronological order.
    /// Returns at most `limit` entries and a flag indicating whether more exist.
    pub fn operation_log(&self, limit: usize) -> Result<(Vec<crate::app::OpLogEntry>, bool)> {
        use crate::dag::{Edge, EdgeKind};
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
        use crate::dag::{Edge, EdgeKind};

        let repo = self.repo.as_ref();
        let commit_id = BackendCommitId::try_from_hex(commit_id_hex)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit id hex"))?;

        // Build ID prefix context for disambiguation.
        let extensions = RevsetExtensions::default();
        let fileset_aliases_map = FilesetAliasesMap::new();
        let path_converter = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = self.revset_parse_context(&extensions, &fileset_aliases_map, &path_converter);
        let id_prefix_context = self.build_id_prefix_context(&context);
        let prefix_index = id_prefix_context
            .populate(repo)
            .wrap_err("failed to populate ID prefix index for evolog")?;

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

    /// Compute file-level changes between two commits (for evolog level-1 unfold).
    pub fn inter_commit_details(&self, from_id: &str, to_id: &str) -> Result<Vec<FileChange>> {
        let repo = self.repo.as_ref();
        let from_commit_id = BackendCommitId::try_from_hex(from_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let to_commit_id = BackendCommitId::try_from_hex(to_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let from_commit = repo.store().get_commit(&from_commit_id)?;
        let to_commit = repo.store().get_commit(&to_commit_id)?;

        let from_tree = from_commit.tree();
        let to_tree = to_commit.tree();
        let copy_records = jj_lib::copies::CopyRecords::default();
        let labels = ConflictLabels::unlabeled();
        let materialize_options = default_materialize_options();

        let mut changes = Vec::new();
        let mut diff_stream =
            from_tree.diff_stream_with_copies(&to_tree, &EverythingMatcher, &copy_records);

        while let Some(entry) = diff_stream.next().block_on() {
            let path = entry.path.target();
            let target_path = path.as_internal_file_string().to_string();
            let values = match entry.values {
                Ok(v) => v,
                Err(_) => continue,
            };

            let before_present = values.before.is_present();
            let after_present = values.after.is_present();
            let status = match (before_present, after_present) {
                (false, true) => FileStatus::Added,
                (true, false) => FileStatus::Deleted,
                (true, true) => FileStatus::Modified,
                (false, false) => continue,
            };
            let has_conflict = !values.after.is_resolved();

            // Compute line stats by materializing + diffing.
            let mut file_stats = LineStats::default();
            let before_mat =
                materialize_tree_value(repo.store(), path, values.before, &labels).block_on()?;
            let after_mat =
                materialize_tree_value(repo.store(), path, values.after, &labels).block_on()?;
            let before_part = git_diff_part(path, before_mat, &materialize_options)
                .block_on()
                .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
            let after_part = git_diff_part(path, after_mat, &materialize_options)
                .block_on()
                .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
            if !before_part.content.is_binary && !after_part.content.is_binary {
                let contents = Diff::new(
                    before_part.content.contents.as_ref(),
                    after_part.content.contents.as_ref(),
                );
                let hunks = unified::unified_diff_hunks(contents, 0, Default::default());
                file_stats = count_line_stats(&hunks);
            }

            changes.push(FileChange {
                path: RepoPath::new(&target_path),
                old_path: None,
                status,
                has_conflict,
                stats: file_stats,
            });
        }

        Ok(changes)
    }

    /// Compute the diff for a single file between two commits (for evolog level-2 unfold).
    pub fn inter_commit_file_diff(
        &self,
        from_id: &str,
        to_id: &str,
        path: &RepoPath,
    ) -> Result<DiffResult> {
        let repo = self.repo.as_ref();
        let from_commit_id = BackendCommitId::try_from_hex(from_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let to_commit_id = BackendCommitId::try_from_hex(to_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let from_commit = repo.store().get_commit(&from_commit_id)?;
        let to_commit = repo.store().get_commit(&to_commit_id)?;

        let from_tree = from_commit.tree();
        let to_tree = to_commit.tree();
        self.trees_file_diff(&from_tree, &to_tree, path)
    }

    /// Compute the diff for a single file between two trees.
    fn trees_file_diff(
        &self,
        before_tree: &MergedTree,
        after_tree: &MergedTree,
        path: &RepoPath,
    ) -> Result<DiffResult> {
        let repo = self.repo.as_ref();
        let repo_path = RepoPathBuf::from_internal_string(path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;
        let labels = ConflictLabels::unlabeled();
        let materialize_options = default_materialize_options();

        let before_value = before_tree.path_value(&repo_path).block_on()?;
        let after_value = after_tree.path_value(&repo_path).block_on()?;

        let before_mat =
            materialize_tree_value(repo.store(), &repo_path, before_value, &labels).block_on()?;
        let after_mat =
            materialize_tree_value(repo.store(), &repo_path, after_value, &labels).block_on()?;

        let before_part = git_diff_part(&repo_path, before_mat, &materialize_options)
            .block_on()
            .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
        let after_part = git_diff_part(&repo_path, after_mat, &materialize_options)
            .block_on()
            .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;

        if before_part.content.is_binary || after_part.content.is_binary {
            let line = DiffLine {
                kind: DiffLineKind::Header,
                content: "(binary file)".to_string(),
                tokens: Vec::new(),
                old_line: None,
                new_line: None,
            };
            return Ok(DiffResult {
                git: vec![line.clone()],
                color_words: vec![line],
            });
        }

        let contents = Diff::new(
            before_part.content.contents.as_ref(),
            after_part.content.contents.as_ref(),
        );
        let hunks = unified::unified_diff_hunks(contents, 3, Default::default());

        let mut git_lines = Vec::new();
        hunks_to_diff_lines(&hunks, &mut git_lines);
        let mut cw_lines = Vec::new();
        hunks_to_color_words_lines(&hunks, &mut cw_lines);
        Ok(DiffResult {
            git: git_lines,
            color_words: cw_lines,
        })
    }

    /// Compute the rebased tree pair for an interdiff.
    /// Returns (rebased_from_tree, to_tree) where rebased_from_tree has from's
    /// changes applied onto to's parent base via 3-way merge.
    fn compute_interdiff_trees(
        &self,
        from_id: &str,
        to_id: &str,
    ) -> Result<(MergedTree, MergedTree)> {
        let repo = self.repo.as_ref();
        let from_commit_id = BackendCommitId::try_from_hex(from_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let to_commit_id = BackendCommitId::try_from_hex(to_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let from_commit = repo.store().get_commit(&from_commit_id)?;
        let to_commit = repo.store().get_commit(&to_commit_id)?;

        let from_parent_tree = from_commit.parent_tree(repo).block_on()?;
        let from_tree = from_commit.tree();
        let to_parent_tree = to_commit.parent_tree(repo).block_on()?;
        let to_tree = to_commit.tree();

        let merge_input = Merge::from_removes_adds(
            [(from_parent_tree, String::new())],
            [(to_parent_tree, String::new()), (from_tree, String::new())],
        );
        let rebased_tree = MergedTree::merge(merge_input)
            .block_on()
            .map_err(|e| color_eyre::eyre::eyre!("tree merge failed: {e}"))?;

        Ok((rebased_tree, to_tree))
    }

    /// Compute file-level changes for an interdiff between two commits.
    /// Rebases `from` onto `to`'s parents, then diffs the result against `to`.
    pub fn interdiff_details(&self, from_id: &str, to_id: &str) -> Result<Vec<FileChange>> {
        let repo = self.repo.as_ref();
        let (rebased_tree, to_tree) = self.compute_interdiff_trees(from_id, to_id)?;

        let copy_records = jj_lib::copies::CopyRecords::default();
        let labels = ConflictLabels::unlabeled();
        let materialize_options = default_materialize_options();

        let mut changes = Vec::new();
        let mut diff_stream =
            rebased_tree.diff_stream_with_copies(&to_tree, &EverythingMatcher, &copy_records);

        while let Some(entry) = diff_stream.next().block_on() {
            let path = entry.path.target();
            let target_path = path.as_internal_file_string().to_string();
            let values = match entry.values {
                Ok(v) => v,
                Err(_) => continue,
            };

            let before_present = values.before.is_present();
            let after_present = values.after.is_present();
            let status = match (before_present, after_present) {
                (false, true) => FileStatus::Added,
                (true, false) => FileStatus::Deleted,
                (true, true) => FileStatus::Modified,
                (false, false) => continue,
            };
            let has_conflict = !values.after.is_resolved();

            let mut file_stats = LineStats::default();
            let before_mat =
                materialize_tree_value(repo.store(), path, values.before, &labels).block_on()?;
            let after_mat =
                materialize_tree_value(repo.store(), path, values.after, &labels).block_on()?;
            let before_part = git_diff_part(path, before_mat, &materialize_options)
                .block_on()
                .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
            let after_part = git_diff_part(path, after_mat, &materialize_options)
                .block_on()
                .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
            if !before_part.content.is_binary && !after_part.content.is_binary {
                let contents = Diff::new(
                    before_part.content.contents.as_ref(),
                    after_part.content.contents.as_ref(),
                );
                let hunks = unified::unified_diff_hunks(contents, 0, Default::default());
                file_stats = count_line_stats(&hunks);
            }

            changes.push(FileChange {
                path: RepoPath::new(&target_path),
                old_path: None,
                status,
                has_conflict,
                stats: file_stats,
            });
        }

        Ok(changes)
    }

    /// Compute per-file diff for an interdiff between two commits.
    pub fn interdiff_file_diff(
        &self,
        from_id: &str,
        to_id: &str,
        path: &RepoPath,
    ) -> Result<DiffResult> {
        let (rebased_tree, to_tree) = self.compute_interdiff_trees(from_id, to_id)?;
        self.trees_file_diff(&rebased_tree, &to_tree, path)
    }

    /// Compute line-by-line annotation (blame) for a file at a specific commit.
    pub fn file_annotate(
        &self,
        commit_id: &UiCommitId,
        file_path: &RepoPath,
    ) -> Result<crate::dag::AnnotateResult> {
        use jj_lib::annotate::FileAnnotator;
        use jj_lib::revset::ResolvedRevsetExpression;

        let repo = self.repo.as_ref();

        let backend_id = BackendCommitId::try_from_hex(commit_id.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit id hex"))?;
        let commit = repo.store().get_commit(&backend_id)?;

        let repo_path = RepoPathBuf::from_internal_string(file_path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;

        let mut annotator = FileAnnotator::from_commit(&commit, &repo_path)
            .block_on()
            .wrap_err("failed to initialize annotator")?;

        let domain = ResolvedRevsetExpression::all();
        annotator
            .compute(repo, &domain)
            .block_on()
            .map_err(|e| color_eyre::eyre::eyre!("annotation failed: {e}"))?;

        let annotation = annotator.to_annotation();

        // Build ID prefix context for short change IDs.
        let extensions = RevsetExtensions::default();
        let fileset_aliases_map = FilesetAliasesMap::new();
        let path_converter = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = self.revset_parse_context(&extensions, &fileset_aliases_map, &path_converter);
        let id_prefix_context = self.build_id_prefix_context(&context);
        let prefix_index = id_prefix_context
            .populate(repo)
            .wrap_err("failed to populate ID prefix index for annotate")?;

        // Cache commit metadata per unique backend CommitId.
        struct CachedMeta {
            ui_commit_id: UiCommitId,
            change_id: ShortId,
            author_display: String,
            relative_time: Str,
        }
        let mut commit_cache: HashMap<BackendCommitId, CachedMeta> = HashMap::new();
        let mut commit_info: HashMap<UiCommitId, crate::dag::AnnotateCommitInfo> = HashMap::new();

        // Pre-seed with the annotated-at commit so header info is always available.
        {
            let change_id = Self::short_change_id(&prefix_index, repo, &commit);
            let short_cid = Self::short_commit_id(&prefix_index, repo, &backend_id);
            commit_info.insert(
                commit_id.clone(),
                build_annotate_commit_info(&commit, short_cid, change_id),
            );
        }

        let mut lines = Vec::new();
        for (line_number, (origin_result, content)) in annotation.line_origins().enumerate() {
            let (origin, outside_domain) = match origin_result {
                Ok(o) => (o, false),
                Err(o) => (o, true),
            };

            if !commit_cache.contains_key(&origin.commit_id) {
                let c = repo.store().get_commit(&origin.commit_id)?;
                let change_id = Self::short_change_id(&prefix_index, repo, &c);
                let short_commit_id = Self::short_commit_id(&prefix_index, repo, &origin.commit_id);
                let commit_id_hex = origin.commit_id.hex();

                let author_sig = c.author();
                let author_display = if author_sig.name.is_empty() {
                    author_sig.email.clone()
                } else {
                    author_sig.name.clone()
                };
                let relative_time = millis_to_relative_time(author_sig.timestamp.timestamp.0);

                let ui_cid = UiCommitId::new(&commit_id_hex);

                commit_info.insert(
                    ui_cid.clone(),
                    build_annotate_commit_info(&c, short_commit_id, change_id.clone()),
                );

                commit_cache.insert(
                    origin.commit_id.clone(),
                    CachedMeta {
                        ui_commit_id: ui_cid,
                        change_id,
                        author_display,
                        relative_time,
                    },
                );
            }
            let meta = &commit_cache[&origin.commit_id];

            let content_str = String::from_utf8_lossy(content)
                .trim_end_matches('\n')
                .to_string();

            lines.push(crate::dag::AnnotateLineData {
                commit_id: meta.ui_commit_id.clone(),
                change_id: meta.change_id.clone(),
                author: meta.author_display.clone(),
                relative_time: meta.relative_time.clone(),
                line_number: line_number + 1,
                content: content_str,
                syntax_tokens: Vec::new(),
                outside_domain,
            });
        }

        syntax_highlight_lines(&mut lines, file_path.as_str());

        Ok(crate::dag::AnnotateResult { lines, commit_info })
    }

    /// Compute the diff between an operation and its parent.
    pub fn op_diff(&self, op_id_hex: &str) -> Result<Vec<crate::app::OpDetailLine>> {
        use crate::app::{
            OpDetailLine, OpDiffBookmark, OpDiffCommit, OpDiffKind, OpDiffWorkingCopy,
        };

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
        let extensions = RevsetExtensions::default();
        let fileset_aliases_map = FilesetAliasesMap::new();
        let path_converter = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = self.revset_parse_context(&extensions, &fileset_aliases_map, &path_converter);
        let id_prefix_context = self.build_id_prefix_context(&context);
        let prefix_index = id_prefix_context
            .populate(self.repo.as_ref())
            .wrap_err("failed to populate ID prefix index")?;

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
                    kind: OpDiffKind::Added,
                }));
            }
            for commit_id in &removed_commits {
                let (change_id, commit_id, desc) =
                    self.short_commit_info(store, commit_id, &prefix_index);
                lines.push(OpDetailLine::Commit(OpDiffCommit {
                    change_id,
                    commit_id,
                    description: desc,
                    kind: OpDiffKind::Removed,
                }));
            }
        }

        // Helper: build a ShortId for a commit ID using the prefix index.
        let short_commit_id = |id: &BackendCommitId| -> ShortId {
            let prefix_len = prefix_index
                .shortest_commit_prefix_len(self.repo.as_ref(), id)
                .unwrap_or(DISPLAY_ID_LEN);
            let hex = id.hex();
            let display_len = prefix_len.max(DISPLAY_ID_LEN);
            ShortId {
                display: hex.get(..display_len).unwrap_or(&hex).to_string(),
                prefix_len,
            }
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

        let change_prefix_len = commit
            .as_ref()
            .and_then(|c| {
                prefix_index
                    .shortest_change_prefix_len(repo, c.change_id())
                    .inspect_err(|e| tracing::warn!("change prefix computation failed: {e}"))
                    .ok()
            })
            .unwrap_or(DISPLAY_ID_LEN);
        let change_id = commit
            .as_ref()
            .map(|c| {
                let h = c.change_id().reverse_hex();
                let display_len = change_prefix_len.max(DISPLAY_ID_LEN);
                ShortId {
                    display: h.get(..display_len).unwrap_or(&h).to_string(),
                    prefix_len: change_prefix_len,
                }
            })
            .unwrap_or_else(|| ShortId {
                display: String::new(),
                prefix_len: 0,
            });

        let commit_prefix_len = prefix_index
            .shortest_commit_prefix_len(repo, commit_id)
            .inspect_err(|e| tracing::warn!("commit prefix computation failed: {e}"))
            .unwrap_or(DISPLAY_ID_LEN);
        let commit_hex = commit_id.hex();
        let commit_display_len = commit_prefix_len.max(DISPLAY_ID_LEN);
        let short_commit = ShortId {
            display: commit_hex
                .get(..commit_display_len)
                .unwrap_or(&commit_hex)
                .to_string(),
            prefix_len: commit_prefix_len,
        };

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
fn format_absolute_time(ts: &jj_lib::backend::Timestamp) -> String {
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

fn build_annotate_commit_info(
    commit: &Commit,
    short_commit_id: ShortId,
    change_id: ShortId,
) -> crate::dag::AnnotateCommitInfo {
    let author_sig = commit.author();
    let committer_sig = commit.committer();
    crate::dag::AnnotateCommitInfo {
        commit_id: short_commit_id,
        change_id,
        author_name: author_sig.name.clone(),
        author_email: author_sig.email.clone(),
        author_date: format_absolute_time(&author_sig.timestamp),
        committer_name: committer_sig.name.clone(),
        committer_email: committer_sig.email.clone(),
        committer_date: format_absolute_time(&committer_sig.timestamp),
        description_lines: {
            let trimmed = commit.description().trim();
            if trimmed.is_empty() {
                Vec::new()
            } else {
                trimmed.lines().map(String::from).collect()
            }
        },
    }
}

/// Cached syntax definitions and ANSI theme for syntax highlighting.
fn syntax_assets() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::Theme) {
    use std::sync::LazyLock;
    use syntect::highlighting::Color;
    static ASSETS: LazyLock<(syntect::parsing::SyntaxSet, syntect::highlighting::Theme)> =
        LazyLock::new(|| {
            let ss = syntect::parsing::SyntaxSet::load_defaults_newlines();
            let ansi = |idx: u8| Color {
                r: idx,
                g: 0,
                b: 1,
                a: 0xFF,
            };
            let theme = build_ansi_theme(ansi);
            (ss, theme)
        });
    &ASSETS
}

/// Apply syntax highlighting to annotate lines based on file extension.
/// Uses ANSI terminal colors so highlighting respects the user's color scheme.
fn syntax_highlight_lines(lines: &mut [crate::dag::AnnotateLineData], file_path: &str) {
    use syntect::easy::HighlightLines;

    let (ss, theme) = syntax_assets();

    let syntax = file_path
        .rsplit('.')
        .next()
        .and_then(|ext| ss.find_syntax_by_extension(ext))
        .or_else(|| {
            let name = file_path.rsplit('/').next().unwrap_or(file_path);
            ss.find_syntax_by_extension(name)
        })
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    if syntax.name == "Plain Text" {
        return;
    }

    let mut h = HighlightLines::new(syntax, theme);

    for line in lines.iter_mut() {
        let input = format!("{}\n", line.content);
        let Ok(regions) = h.highlight_line(&input, ss) else {
            continue;
        };
        let mut tokens = Vec::new();
        for (style, text) in regions {
            let trimmed = text.trim_end_matches('\n');
            if trimmed.is_empty() {
                continue;
            }
            let fg = style.foreground;
            let color_idx = if fg.g == 0 && fg.b == 1 {
                fg.r // decode our sentinel
            } else {
                7 // default: ANSI white
            };
            tokens.push(crate::dag::SyntaxToken {
                text: trimmed.to_string(),
                color_idx,
            });
        }
        line.syntax_tokens = tokens;
    }
}

/// Build a syntect Theme that maps syntax scopes to ANSI terminal color indices.
/// Color mapping follows the koda colorscheme conventions.
fn build_ansi_theme(
    ansi: impl Fn(u8) -> syntect::highlighting::Color,
) -> syntect::highlighting::Theme {
    use std::str::FromStr;
    use syntect::highlighting::{ScopeSelectors, Theme, ThemeItem, ThemeSettings};

    let item = |scope: &str, color_idx: u8| ThemeItem {
        scope: ScopeSelectors::from_str(scope).unwrap_or_default(),
        style: syntect::highlighting::StyleModifier {
            foreground: Some(ansi(color_idx)),
            background: None,
            font_style: None,
        },
    };

    // ANSI indices: 1=red, 2=green, 3=yellow, 4=blue, 5=magenta, 6=cyan,
    //              7=white(fg), 8=bright black(dim)
    Theme {
        name: Some("ansi".to_string()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(ansi(7)),
            background: Some(ansi(0)),
            ..Default::default()
        },
        scopes: vec![
            // Comments (including delimiters like ///) → dim (fg_alt)
            item("comment", 8),
            item("punctuation.definition.comment", 8),
            // Strings, characters → green (including quote delimiters)
            item("string", 2),
            item("constant.character", 2),
            item("punctuation.definition.string", 2),
            // Numbers, booleans, floats, special chars → yellow (orange equivalent)
            item("constant.numeric", 3),
            item("constant.character.escape", 3),
            item("constant.other.placeholder", 3),
            // Language constants (true/false/nil) → cyan
            item("constant.language", 6),
            item("constant", 6),
            item("entity.name.constant", 6),
            // Keywords, control flow, storage → red
            item("keyword", 1),
            item("storage", 1),
            item("keyword.control.import", 1),
            // Repeat keywords (for/while/loop) → magenta
            item("keyword.control.repeat", 5),
            // Types, structures, traits, interfaces → blue
            item("entity.name.type", 4),
            item("entity.name.class", 4),
            item("entity.name.struct", 4),
            item("entity.name.enum", 4),
            item("entity.name.union", 4),
            item("entity.name.trait", 4),
            item("entity.name.impl", 4),
            item("entity.name.interface", 4),
            item("entity.other.inherited-class", 4),
            item("support.type", 4),
            item("support.class", 4),
            // Functions → yellow
            item("entity.name.function", 3),
            item("support.function", 3),
            item("variable.function", 3),
            // Macros → magenta (override function parent)
            item("entity.name.function.macro", 5),
            item("support.macro", 5),
            item("entity.name.macro", 5),
            item("meta.attribute", 5),
            // Variables, identifiers → default fg
            item("variable", 7),
            // Operators, delimiters, punctuation → default fg
            item("keyword.operator", 7),
            item("punctuation", 7),
            // Properties, members, labels, attributes → magenta (purple equivalent)
            item("variable.other.member", 5),
            item("variable.other.property", 5),
            item("entity.name.label", 5),
            item("entity.other.attribute-name", 5),
            item("variable.annotation", 5),
            // Tags (HTML/XML) → red
            item("entity.name.tag", 1),
            // Modules, namespaces → default fg
            item("entity.name.module, entity.name.namespace", 7),
            // Language builtins (self, super, etc.) → cyan
            item("variable.language", 6),
            item("support.constant", 6),
            // Invalid → red
            item("invalid", 1),
            // Markup (markdown, etc.)
            item("markup.heading, entity.name.section", 4),
            item("markup.list", 1),
            item("markup.raw", 2),
            item("markup.underline.link", 6),
            item("markup.link", 6),
            item("markup.quote", 8),
            item("markup.inserted", 2),
            item("markup.deleted", 1),
            item("markup.changed", 3),
        ],
        ..Default::default()
    }
}

/// Parse a commit description, returning `None` for empty/placeholder descriptions.
fn parse_first_line_description(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "(no description set)" {
        None
    } else {
        trimmed.lines().next().map(String::from)
    }
}

/// Convert unified diff hunks into `DiffLine` structs with token spans and line numbers.
/// Count added/removed lines from unified diff hunks.
fn count_line_stats(hunks: &[unified::UnifiedDiffHunk<'_>]) -> LineStats {
    let mut stats = LineStats::default();
    for hunk in hunks {
        for (line_type, _) in &hunk.lines {
            match line_type {
                DiffLineType::Added => stats.added = stats.added.saturating_add(1),
                DiffLineType::Removed => stats.removed = stats.removed.saturating_add(1),
                DiffLineType::Context => {}
            }
        }
    }
    stats
}

fn hunks_to_diff_lines(hunks: &[unified::UnifiedDiffHunk<'_>], out: &mut Vec<DiffLine>) {
    for hunk in hunks {
        out.push(DiffLine {
            kind: DiffLineKind::Header,
            content: format!(
                "@@ -{},{} +{},{} @@",
                hunk.left_line_range.start + 1,
                hunk.left_line_range.len(),
                hunk.right_line_range.start + 1,
                hunk.right_line_range.len(),
            ),
            tokens: vec![],
            old_line: None,
            new_line: None,
        });

        let mut old_line = hunk.left_line_range.start as u32 + 1;
        let mut new_line = hunk.right_line_range.start as u32 + 1;

        for (line_type, tokens) in &hunk.lines {
            let mut diff_tokens = Vec::new();
            let mut full_text = String::new();
            for (tag, bytes) in tokens {
                let text = String::from_utf8_lossy(bytes).to_string();
                full_text.push_str(&text);
                let kind = match tag {
                    DiffTokenType::Matching => crate::dag::DiffTokenKind::Unchanged,
                    DiffTokenType::Different => match line_type {
                        DiffLineType::Removed => crate::dag::DiffTokenKind::Removed,
                        _ => crate::dag::DiffTokenKind::Added,
                    },
                };
                diff_tokens.push(crate::dag::DiffToken { text, kind });
            }
            if let Some(last) = diff_tokens.last_mut() {
                last.text = last.text.trim_end_matches('\n').to_string();
            }
            let text = full_text.trim_end_matches('\n').to_string();

            let (kind, ol, nl) = match line_type {
                DiffLineType::Context => {
                    let r = (DiffLineKind::Context, Some(old_line), Some(new_line));
                    old_line += 1;
                    new_line += 1;
                    r
                }
                DiffLineType::Removed => {
                    let r = (DiffLineKind::Removed, Some(old_line), None);
                    old_line += 1;
                    r
                }
                DiffLineType::Added => {
                    let r = (DiffLineKind::Added, None, Some(new_line));
                    new_line += 1;
                    r
                }
            };
            out.push(DiffLine {
                kind,
                content: text,
                tokens: diff_tokens,
                old_line: ol,
                new_line: nl,
            });
        }
    }
}

/// Count how many times a word diff alternates between removed and added content.
/// Used to decide whether to inline color-words or fall back to separate lines.
fn count_alternations(diff: &jj_lib::diff::ContentDiff<'_>) -> usize {
    let mut count: usize = 0;
    let mut last_side: Option<u8> = None;
    for h in diff.hunks() {
        if h.kind == jj_lib::diff::DiffHunkKind::Matching {
            continue;
        }
        if !h.contents[0].is_empty() && last_side != Some(0) {
            count += 1;
            last_side = Some(0);
        }
        if !h.contents[1].is_empty() && last_side != Some(1) {
            count += 1;
            last_side = Some(1);
        }
    }
    count
}

/// Accumulator for building color-words diff lines from consecutive
/// removed/added blocks.
struct ColorWordBuilder {
    removed_lines: Vec<String>,
    added_lines: Vec<String>,
    removed_count: u32,
    added_count: u32,
    old_line: u32,
    new_line: u32,
}

impl ColorWordBuilder {
    fn new(old_start: u32, new_start: u32) -> Self {
        Self {
            removed_lines: Vec::new(),
            added_lines: Vec::new(),
            removed_count: 0,
            added_count: 0,
            old_line: old_start,
            new_line: new_start,
        }
    }

    fn push_removed(&mut self, text: String) {
        self.removed_lines.push(text);
        self.removed_count += 1;
    }

    fn push_added(&mut self, text: String) {
        self.added_lines.push(text);
        self.added_count += 1;
    }

    fn has_pending_added_only(&self) -> bool {
        !self.added_lines.is_empty() && self.removed_lines.is_empty()
    }

    /// Flush a collected removed+added block into output DiffLines.
    fn flush(&mut self, out: &mut Vec<DiffLine>) {
        use crate::dag::{DiffToken, DiffTokenKind};
        const MAX_ALTERNATION: usize = 3;

        if self.removed_lines.is_empty() && self.added_lines.is_empty() {
            return;
        }

        let removed_block = self.removed_lines.join("\n");
        let added_block = self.added_lines.join("\n");

        // Word-diff once, use for both alternation check and rendering.
        let can_inline = !self.removed_lines.is_empty() && !self.added_lines.is_empty();
        let diff = if can_inline {
            Some(jj_lib::diff::ContentDiff::by_word([
                removed_block.as_bytes(),
                added_block.as_bytes(),
            ]))
        } else {
            None
        };

        let should_inline = diff
            .as_ref()
            .is_some_and(|d| count_alternations(d) <= MAX_ALTERNATION);

        if should_inline {
            let diff = diff.unwrap();
            let mut tokens: Vec<DiffToken> = Vec::new();
            let mut content = String::new();
            let base_old = self.old_line;
            let base_new = self.new_line;

            for h in diff.hunks() {
                match h.kind {
                    jj_lib::diff::DiffHunkKind::Matching => {
                        let text = String::from_utf8_lossy(h.contents[0]);
                        for (i, part) in text.split('\n').enumerate() {
                            if i > 0 {
                                out.push(DiffLine {
                                    kind: DiffLineKind::Context,
                                    content: std::mem::take(&mut content),
                                    tokens: std::mem::take(&mut tokens),
                                    old_line: Some(self.old_line),
                                    new_line: Some(self.new_line),
                                });
                                self.old_line += 1;
                                self.new_line += 1;
                            }
                            if !part.is_empty() {
                                content.push_str(part);
                                tokens.push(DiffToken {
                                    text: part.to_string(),
                                    kind: DiffTokenKind::Unchanged,
                                });
                            }
                        }
                    }
                    jj_lib::diff::DiffHunkKind::Different => {
                        let removed = String::from_utf8_lossy(h.contents[0]);
                        let added = String::from_utf8_lossy(h.contents[1]);

                        for (i, part) in removed.split('\n').enumerate() {
                            if i > 0 {
                                out.push(DiffLine {
                                    kind: DiffLineKind::Context,
                                    content: std::mem::take(&mut content),
                                    tokens: std::mem::take(&mut tokens),
                                    old_line: Some(self.old_line),
                                    new_line: None,
                                });
                                self.old_line += 1;
                            }
                            if !part.is_empty() {
                                content.push_str(part);
                                tokens.push(DiffToken {
                                    text: part.to_string(),
                                    kind: DiffTokenKind::Removed,
                                });
                            }
                        }

                        for (i, part) in added.split('\n').enumerate() {
                            if i > 0 {
                                out.push(DiffLine {
                                    kind: DiffLineKind::Context,
                                    content: std::mem::take(&mut content),
                                    tokens: std::mem::take(&mut tokens),
                                    old_line: None,
                                    new_line: Some(self.new_line),
                                });
                                self.new_line += 1;
                            }
                            if !part.is_empty() {
                                content.push_str(part);
                                tokens.push(DiffToken {
                                    text: part.to_string(),
                                    kind: DiffTokenKind::Added,
                                });
                            }
                        }
                    }
                }
            }

            if !content.is_empty() || !tokens.is_empty() {
                let has_removed = tokens.iter().any(|t| t.kind == DiffTokenKind::Removed);
                let has_added = tokens.iter().any(|t| t.kind == DiffTokenKind::Added);
                out.push(DiffLine {
                    kind: DiffLineKind::Context,
                    content,
                    tokens,
                    old_line: if self.old_line > base_old || has_removed || !has_added {
                        Some(self.old_line)
                    } else {
                        None
                    },
                    new_line: if self.new_line > base_new || has_added || !has_removed {
                        Some(self.new_line)
                    } else {
                        None
                    },
                });
            }

            self.old_line = base_old + self.removed_count;
            self.new_line = base_new + self.added_count;
        } else {
            for line in self.removed_lines.drain(..) {
                out.push(DiffLine {
                    kind: DiffLineKind::Removed,
                    content: line.clone(),
                    tokens: vec![DiffToken {
                        text: line,
                        kind: DiffTokenKind::Removed,
                    }],
                    old_line: Some(self.old_line),
                    new_line: None,
                });
                self.old_line += 1;
            }
            for line in self.added_lines.drain(..) {
                out.push(DiffLine {
                    kind: DiffLineKind::Added,
                    content: line.clone(),
                    tokens: vec![DiffToken {
                        text: line,
                        kind: DiffTokenKind::Added,
                    }],
                    old_line: None,
                    new_line: Some(self.new_line),
                });
                self.new_line += 1;
            }
        }

        self.removed_lines.clear();
        self.added_lines.clear();
        self.removed_count = 0;
        self.added_count = 0;
    }
}

/// Convert unified diff hunks into color-words `DiffLine` structs.
fn hunks_to_color_words_lines(hunks: &[unified::UnifiedDiffHunk<'_>], out: &mut Vec<DiffLine>) {
    use crate::dag::{DiffToken, DiffTokenKind};

    for hunk in hunks {
        out.push(DiffLine {
            kind: DiffLineKind::Header,
            content: format!(
                "@@ -{},{} +{},{} @@",
                hunk.left_line_range.start + 1,
                hunk.left_line_range.len(),
                hunk.right_line_range.start + 1,
                hunk.right_line_range.len(),
            ),
            tokens: vec![],
            old_line: None,
            new_line: None,
        });

        let mut builder = ColorWordBuilder::new(
            hunk.left_line_range.start as u32 + 1,
            hunk.right_line_range.start as u32 + 1,
        );

        for (line_type, tokens) in &hunk.lines {
            let mut text = String::new();
            for (_, bytes) in tokens {
                text.push_str(&String::from_utf8_lossy(bytes));
            }
            let text = text.trim_end_matches('\n').to_string();

            match line_type {
                DiffLineType::Removed => {
                    if builder.has_pending_added_only() {
                        builder.flush(out);
                    }
                    builder.push_removed(text);
                }
                DiffLineType::Added => {
                    builder.push_added(text);
                }
                DiffLineType::Context => {
                    builder.flush(out);
                    out.push(DiffLine {
                        kind: DiffLineKind::Context,
                        content: text.clone(),
                        tokens: vec![DiffToken {
                            text,
                            kind: DiffTokenKind::Unchanged,
                        }],
                        old_line: Some(builder.old_line),
                        new_line: Some(builder.new_line),
                    });
                    builder.old_line += 1;
                    builder.new_line += 1;
                }
            }
        }
        builder.flush(out);
    }
}

fn format_relative_time(dt: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(dt);

    if duration.num_seconds() < 0 {
        return "just now".to_string();
    }

    let secs = duration.num_seconds();
    if secs < 60 {
        return if secs == 1 {
            "1 second ago".to_string()
        } else {
            format!("{secs} seconds ago")
        };
    }
    let mins = duration.num_minutes();
    if mins < 60 {
        return if mins == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{mins} minutes ago")
        };
    }
    let hours = duration.num_hours();
    if hours < 24 {
        return if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        };
    }
    let days = duration.num_days();
    if days < 30 {
        return if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{days} days ago")
        };
    }
    let months = days / 30;
    if months < 12 {
        return if months == 1 {
            "1 month ago".to_string()
        } else {
            format!("{months} months ago")
        };
    }
    let years = days / 365;
    if years == 1 {
        "1 year ago".to_string()
    } else {
        format!("{years} years ago")
    }
}

fn default_materialize_options() -> ConflictMaterializeOptions {
    ConflictMaterializeOptions {
        marker_style: jj_lib::conflicts::ConflictMarkerStyle::Git,
        marker_len: None,
        merge: jj_lib::tree_merge::MergeOptions {
            hunk_level: jj_lib::files::FileMergeHunkLevel::Line,
            same_change: jj_lib::merge::SameChange::Accept,
        },
    }
}
