use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::Context;
use color_eyre::Result;
use futures::StreamExt as _;
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::graph::{GraphEdgeType, GraphNode, TopoGroupedGraphIterator};
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

use jj_lib::conflict_labels::ConflictLabels;
use jj_lib::conflicts::{materialize_tree_value, ConflictMaterializeOptions};
use jj_lib::diff_presentation::unified::{self, git_diff_part, DiffLineType};
use jj_lib::merge::Diff;
use jj_lib::repo_path::RepoPathBuf;

use crate::dag::{
    AuthorInfo, BookmarkInfo, CommitInfo, DagEntry, DiffLine, DiffLineKind, Edge, EdgeKind,
    FileChange, FileStatus, LineStats, RemoteBookmarkInfo, ShortId,
};
use crate::types::ChangeId;

/// Number of hex characters to show for change/commit IDs.
const DISPLAY_ID_LEN: usize = 8;

/// Vendored jj-cli default revset configuration.
/// Contains `[revsets]` (default log revset, etc.) and `[revset-aliases]`
/// (trunk(), immutable_heads(), immutable(), mutable(), etc.).
const DEFAULT_REVSETS_TOML: &str = include_str!("config/revsets.toml");

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

impl JjRepo {
    /// Trigger a working copy snapshot so the repo reflects the current
    /// filesystem state. Shells out to `jj status` which snapshots as a
    /// side effect.
    pub fn snapshot(repo_path: &Path) {
        let _ = std::process::Command::new("jj")
            .arg("status")
            .arg("-R")
            .arg(repo_path)
            .arg("--quiet")
            .arg("--color=never")
            .output();
    }

    /// Open the jj workspace rooted at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        pollster::block_on(Self::open_async(path))
    }

    /// Open the jj workspace rooted at `path`.
    pub async fn open_async(path: &Path) -> Result<Self> {
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
            .await
            .wrap_err("failed to load repo at HEAD")?;

        Ok(Self {
            repo,
            settings,
            aliases_map,
            workspace_name,
            workspace_root,
        })
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
        if let Some(config_dir) = dirs_next_config_dir() {
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
                "present(@) | ancestors(immutable_heads().., 2) | trunk()".to_string()
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

    /// Evaluate a revset string and return DAG entries in topological order
    /// with graph edges for rendering.
    pub fn evaluate_revset(&self, revset_str: &str) -> Result<Vec<DagEntry>> {
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
        let immutable_revset = self.evaluate_immutable(&context, &symbol_resolver);
        let is_immutable = immutable_revset.as_ref().map(|r| r.containing_fn());

        // Collect the set of commit IDs in the log revset so we can safely
        // filter the prioritize revset (prioritize_branch panics if given an
        // ID that doesn't exist in the input iterator).
        let log_commit_ids: std::collections::HashSet<CommitId> = revset.iter().flatten().collect();

        // Wrap the graph iterator with TopoGroupedGraphIterator for proper
        // branch grouping, then prioritize branches matching the config
        // (default: present(@)) so they appear on the leftmost column.
        let graph_iter = revset.iter_graph();
        let mut topo_iter: TopoGroupedGraphIterator<CommitId, CommitId, _, _> =
            TopoGroupedGraphIterator::new(graph_iter, |id| id);

        // Evaluate the log-graph-prioritize revset and call prioritize_branch()
        // only for commits that are actually in the log revset.
        let prioritize_revset = self.evaluate_prioritize(&context, &symbol_resolver);
        if let Some(ref prio) = prioritize_revset {
            for commit_id in prio.iter().flatten() {
                if log_commit_ids.contains(&commit_id) {
                    topo_iter.prioritize_branch(commit_id);
                }
            }
        }

        // Set up ID prefix index for shortest unique prefixes.
        // Scope disambiguation to the log revset so prefixes only need to be
        // unique among visible commits (matching jj-cli behavior).
        let id_prefix_context = {
            let mut diag = RevsetDiagnostics::new();
            if let Ok(expression) = jj_lib::revset::parse(&mut diag, revset_str, &context) {
                IdPrefixContext::default().disambiguate_within(expression)
            } else {
                IdPrefixContext::default()
            }
        };
        let id_prefix_index = id_prefix_context
            .populate(repo)
            .wrap_err("failed to populate ID prefix index")?;

        // Pre-build the set of local bookmarks that differ from their tracked
        // remote counterpart (O(M) once, then O(1) per bookmark lookup).
        let dirty_bookmarks: HashSet<&RefName> = repo
            .view()
            .all_remote_bookmarks()
            .filter(|(symbol, remote_ref)| {
                remote_ref.is_tracked()
                    && *repo.view().get_local_bookmark(symbol.name) != remote_ref.target
            })
            .map(|(symbol, _)| symbol.name)
            .collect();

        // Pre-build a map from commit ID to remote bookmarks pointing at it.
        // O(M) once per refresh, then O(1) per commit lookup.
        let mut remote_bookmark_map: HashMap<CommitId, Vec<(String, String)>> = HashMap::new();
        for (symbol, remote_ref) in repo.view().all_remote_bookmarks() {
            if let Some(commit_id) = remote_ref.target.as_normal() {
                remote_bookmark_map
                    .entry(commit_id.clone())
                    .or_default()
                    .push((
                        symbol.name.as_str().to_string(),
                        symbol.remote.as_str().to_string(),
                    ));
            }
        }

        // Iterate graph nodes
        let mut entries = Vec::new();
        for node_result in topo_iter {
            let (commit_id, edges): GraphNode<CommitId> =
                node_result.wrap_err("error iterating revset graph")?;
            let commit = repo
                .store()
                .get_commit(&commit_id)
                .wrap_err("failed to load commit")?;

            let immutable = is_immutable
                .as_ref()
                .and_then(|check| check(&commit_id).ok())
                .unwrap_or(false);

            let info = self.extract_commit_info(
                &commit,
                &id_prefix_index,
                immutable,
                &dirty_bookmarks,
                &remote_bookmark_map,
            )?;
            let dag_edges = edges
                .into_iter()
                .map(|e| Edge {
                    target: ChangeId::new(e.target.hex()),
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

        Ok(entries)
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
    /// evaluated.
    fn evaluate_prioritize(
        &self,
        context: &RevsetParseContext<'_>,
        symbol_resolver: &SymbolResolver,
    ) -> Option<Box<dyn jj_lib::revset::Revset + '_>> {
        let repo = self.repo.as_ref();
        let mut diagnostics = RevsetDiagnostics::new();
        let revset_str = self.prioritize_revset_str();

        let parsed = jj_lib::revset::parse(&mut diagnostics, &revset_str, context).ok()?;
        let resolved = parsed.resolve_user_expression(repo, symbol_resolver).ok()?;
        resolved.evaluate(repo).ok()
    }

    /// Evaluate the `immutable()` revset. Returns `None` if it can't be
    /// evaluated (e.g. alias not defined). The caller keeps the returned
    /// `Box<dyn Revset>` alive and calls `.containing_fn()` on it.
    fn evaluate_immutable(
        &self,
        context: &RevsetParseContext<'_>,
        symbol_resolver: &SymbolResolver,
    ) -> Option<Box<dyn jj_lib::revset::Revset + '_>> {
        let repo = self.repo.as_ref();
        let mut diagnostics = RevsetDiagnostics::new();

        let parsed = jj_lib::revset::parse(&mut diagnostics, "immutable()", context).ok()?;
        let resolved = parsed.resolve_user_expression(repo, symbol_resolver).ok()?;
        resolved.evaluate(repo).ok()
    }

    /// Compute the file-level changes and line totals for a commit.
    pub async fn commit_details(
        &self,
        commit_hex_id: &str,
    ) -> Result<(Vec<FileChange>, LineStats)> {
        let repo = self.repo.as_ref();
        let commit_id = CommitId::try_from_hex(commit_hex_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex: {commit_hex_id}"))?;
        let commit = repo
            .store()
            .get_commit(&commit_id)
            .wrap_err("failed to load commit for diff")?;

        let parent_tree = commit
            .parent_tree(repo)
            .await
            .wrap_err("failed to get parent tree")?;
        let commit_tree = commit.tree();

        let mut changes = Vec::new();
        let mut stats = LineStats::default();
        let labels = ConflictLabels::unlabeled();
        let materialize_options = ConflictMaterializeOptions {
            marker_style: jj_lib::conflicts::ConflictMarkerStyle::Git,
            marker_len: None,
            merge: jj_lib::tree_merge::MergeOptions {
                hunk_level: jj_lib::files::FileMergeHunkLevel::Line,
                same_change: jj_lib::merge::SameChange::Accept,
            },
        };
        let mut diff_stream = parent_tree.diff_stream(&commit_tree, &EverythingMatcher);

        while let Some(entry) = diff_stream.next().await {
            let path = entry.path.as_internal_file_string().to_string();
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
                (false, false) => continue, // shouldn't happen
            };

            changes.push(FileChange { path, status });

            let before_mat =
                materialize_tree_value(repo.store(), &entry.path, values.before, &labels).await?;
            let after_mat =
                materialize_tree_value(repo.store(), &entry.path, values.after, &labels).await?;

            let before_part = git_diff_part(&entry.path, before_mat, &materialize_options)
                .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
            let after_part = git_diff_part(&entry.path, after_mat, &materialize_options)
                .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;

            if before_part.content.is_binary || after_part.content.is_binary {
                continue;
            }

            let contents = Diff::new(
                before_part.content.contents.as_ref(),
                after_part.content.contents.as_ref(),
            );
            let hunks = unified::unified_diff_hunks(contents, 3, Default::default());
            for hunk in &hunks {
                for (line_type, _) in &hunk.lines {
                    match line_type {
                        DiffLineType::Added => stats.added = stats.added.saturating_add(1),
                        DiffLineType::Removed => stats.removed = stats.removed.saturating_add(1),
                        DiffLineType::Context => {}
                    }
                }
            }
        }

        Ok((changes, stats))
    }

    /// Compute the line-level diff for a single file in a commit.
    pub async fn file_diff(&self, commit_hex_id: &str, path: &str) -> Result<Vec<DiffLine>> {
        let repo = self.repo.as_ref();
        let commit_id = CommitId::try_from_hex(commit_hex_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex: {commit_hex_id}"))?;
        let commit = repo
            .store()
            .get_commit(&commit_id)
            .wrap_err("failed to load commit for diff")?;

        let parent_tree = commit.parent_tree(repo).await?;
        let commit_tree = commit.tree();
        let repo_path = RepoPathBuf::from_internal_string(path)
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;
        let labels = ConflictLabels::unlabeled();
        let materialize_options = ConflictMaterializeOptions {
            marker_style: jj_lib::conflicts::ConflictMarkerStyle::Git,
            marker_len: None,
            merge: jj_lib::tree_merge::MergeOptions {
                hunk_level: jj_lib::files::FileMergeHunkLevel::Line,
                same_change: jj_lib::merge::SameChange::Accept,
            },
        };

        let before_value = parent_tree.path_value(&repo_path)?;
        let after_value = commit_tree.path_value(&repo_path)?;

        let before_mat =
            materialize_tree_value(repo.store(), &repo_path, before_value, &labels).await?;
        let after_mat =
            materialize_tree_value(repo.store(), &repo_path, after_value, &labels).await?;

        let before_part = git_diff_part(&repo_path, before_mat, &materialize_options)
            .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
        let after_part = git_diff_part(&repo_path, after_mat, &materialize_options)
            .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;

        if before_part.content.is_binary || after_part.content.is_binary {
            return Ok(vec![DiffLine {
                kind: DiffLineKind::Header,
                content: "(binary file)".to_string(),
                old_line: None,
                new_line: None,
            }]);
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

        let mut lines = Vec::new();
        for hunk in &hunks {
            // Hunk header
            lines.push(DiffLine {
                kind: DiffLineKind::Header,
                content: format!(
                    "@@ -{},{} +{},{} @@",
                    hunk.left_line_range.start + 1,
                    hunk.left_line_range.len(),
                    hunk.right_line_range.start + 1,
                    hunk.right_line_range.len(),
                ),
                old_line: None,
                new_line: None,
            });

            // Track line numbers through the hunk (1-indexed).
            let mut old_line = hunk.left_line_range.start as u32 + 1;
            let mut new_line = hunk.right_line_range.start as u32 + 1;

            for (line_type, tokens) in &hunk.lines {
                // Concatenate all tokens into a single string.
                let text: String = tokens
                    .iter()
                    .map(|(_, bytes)| String::from_utf8_lossy(bytes))
                    .collect::<String>()
                    .trim_end_matches('\n')
                    .to_string();

                let (kind, ol, nl) = match line_type {
                    DiffLineType::Context => {
                        let result = (DiffLineKind::Context, Some(old_line), Some(new_line));
                        old_line += 1;
                        new_line += 1;
                        result
                    }
                    DiffLineType::Removed => {
                        let result = (DiffLineKind::Removed, Some(old_line), None);
                        old_line += 1;
                        result
                    }
                    DiffLineType::Added => {
                        let result = (DiffLineKind::Added, None, Some(new_line));
                        new_line += 1;
                        result
                    }
                };
                lines.push(DiffLine {
                    kind,
                    content: text,
                    old_line: ol,
                    new_line: nl,
                });
            }
        }

        Ok(lines)
    }

    fn extract_commit_info(
        &self,
        commit: &Commit,
        id_prefix_index: &jj_lib::id_prefix::IdPrefixIndex<'_>,
        is_immutable: bool,
        dirty_bookmarks: &HashSet<&RefName>,
        remote_bookmark_map: &HashMap<CommitId, Vec<(String, String)>>,
    ) -> Result<CommitInfo> {
        let repo = self.repo.as_ref();

        // Change ID: 8-char display with unique prefix highlighted
        let change_prefix_len = id_prefix_index
            .shortest_change_prefix_len(repo, commit.change_id())
            .unwrap_or(DISPLAY_ID_LEN);
        let change_id_full = commit.change_id().reverse_hex();
        let change_id = ShortId {
            display: change_id_full
                .get(..DISPLAY_ID_LEN)
                .unwrap_or(&change_id_full)
                .to_string(),
            prefix_len: change_prefix_len.min(DISPLAY_ID_LEN),
        };

        // Commit ID: 8-char display with unique prefix highlighted
        let commit_prefix_len = id_prefix_index
            .shortest_commit_prefix_len(repo, commit.id())
            .unwrap_or(DISPLAY_ID_LEN);
        let commit_id_full = commit.id().hex();
        let commit_id = ShortId {
            display: commit_id_full
                .get(..DISPLAY_ID_LEN)
                .unwrap_or(&commit_id_full)
                .to_string(),
            prefix_len: commit_prefix_len.min(DISPLAY_ID_LEN),
        };

        // Description
        let raw_desc = commit.description().trim();
        let description = if raw_desc.is_empty() || raw_desc == "(no description set)" {
            None
        } else {
            raw_desc.lines().next().map(String::from)
        };

        // Author
        let sig = commit.author();
        let millis = sig.timestamp.timestamp.0;
        let tz_offset_seconds = sig.timestamp.tz_offset as i64 * 60;
        let timestamp =
            jiff::Timestamp::from_millisecond(millis).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
        // We store the raw UTC timestamp; display code can apply tz offset later
        let _ = tz_offset_seconds; // TODO: use for display formatting

        let author = AuthorInfo {
            name: sig.name.clone(),
            email: sig.email.clone(),
            timestamp,
        };

        // Working copy
        let is_working_copy = repo.view().is_wc_commit_id(commit.id());

        // Empty
        let is_empty = commit.is_empty(repo).unwrap_or(false);

        // Conflicts
        let has_conflict = commit.has_conflict();

        // Bookmarks (with dirty/tracking status)
        let bookmarks: Vec<BookmarkInfo> = repo
            .view()
            .local_bookmarks_for_commit(commit.id())
            .map(|(name, _)| BookmarkInfo {
                name: name.as_str().to_string(),
                is_dirty: dirty_bookmarks.contains(name),
            })
            .collect();

        // Remote bookmarks pointing at this commit, excluding those already
        // represented by a local bookmark with the same name on this commit.
        let local_names: HashSet<&str> = bookmarks.iter().map(|b| b.name.as_str()).collect();
        let remote_bookmarks: Vec<RemoteBookmarkInfo> = remote_bookmark_map
            .get(commit.id())
            .map(|rbs: &Vec<(String, String)>| {
                rbs.iter()
                    .filter(|(name, _): &&(String, String)| !local_names.contains(name.as_str()))
                    .map(|(name, remote)| RemoteBookmarkInfo {
                        name: name.clone(),
                        remote: remote.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Hidden, divergent, and change ID disambiguation.
        let resolved_targets = repo.resolve_change_id(commit.change_id()).ok().flatten();

        let is_divergent = resolved_targets
            .as_ref()
            .is_some_and(|targets| targets.is_divergent());

        let is_hidden = commit.is_hidden(repo).unwrap_or(false);

        // Compute disambiguation suffix (e.g., /5 in ztmnmkvk/5) only for
        // hidden or divergent commits where disambiguation is needed.
        let change_id_suffix = if is_hidden || is_divergent {
            resolved_targets
                .as_ref()
                .and_then(|targets| targets.find_offset(commit.id()))
        } else {
            None
        };

        // Full commit ID hex for graph rendering (stable key).
        let graph_id = commit.id().hex();

        Ok(CommitInfo {
            graph_id,
            change_id,
            commit_id,
            description,
            author,
            is_working_copy,
            is_empty,
            has_conflict,
            is_immutable,
            is_divergent,
            is_hidden,
            change_id_suffix,
            bookmarks,
            remote_bookmarks,
        })
    }
}

/// Platform-appropriate user config directory.
fn dirs_next_config_dir() -> Option<PathBuf> {
    // XDG_CONFIG_HOME or ~/.config on Linux
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}
