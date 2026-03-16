use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::Context;
use color_eyre::Result;
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::config::{ConfigSource, StackedConfig};
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::graph::{GraphEdgeType, GraphNode};
use jj_lib::id_prefix::IdPrefixContext;
use jj_lib::object_id::ObjectId;
use jj_lib::ref_name::WorkspaceNameBuf;
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

use futures::StreamExt as _;
use jj_lib::matchers::EverythingMatcher;

use crate::dag::{AuthorInfo, CommitInfo, DagEntry, Edge, EdgeKind, FileChange, FileStatus};

/// Thin adapter around jj-lib. Owns the workspace and repo, converts
/// jj-lib types into our domain types so nothing leaks out.
pub struct JjRepo {
    repo: Arc<ReadonlyRepo>,
    /// Workspace name, needed for `@` resolution.
    workspace_name: WorkspaceNameBuf,
    /// Root path of the workspace (for display / path conversion).
    workspace_root: PathBuf,
}

impl JjRepo {
    /// Open the jj workspace rooted at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let config = Self::load_config(path)?;
        let settings = UserSettings::from_config(config)
            .wrap_err("failed to create jj settings from config")?;
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
            workspace_name,
            workspace_root,
        })
    }

    /// Build a minimal config: jj-lib defaults + user config file if present.
    fn load_config(workspace_path: &Path) -> Result<StackedConfig> {
        let mut config = StackedConfig::with_defaults();

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

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Evaluate a revset string and return DAG entries in topological order
    /// with graph edges for rendering.
    pub fn evaluate_revset(&self, revset_str: &str) -> Result<Vec<DagEntry>> {
        let repo = self.repo.as_ref();

        // Build parse context
        let aliases_map = RevsetAliasesMap::new();
        let extensions = RevsetExtensions::default();
        let fileset_aliases_map = FilesetAliasesMap::new();
        let path_converter = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let workspace_ctx = RevsetWorkspaceContext {
            path_converter: &path_converter,
            workspace_name: &self.workspace_name,
        };
        let context = RevsetParseContext {
            aliases_map: &aliases_map,
            local_variables: Default::default(),
            user_email: "",
            date_pattern_context: DatePatternContext::from(chrono::Local::now()),
            default_ignored_remote: None,
            fileset_aliases_map: &fileset_aliases_map,
            use_glob_by_default: true,
            extensions: &extensions,
            workspace: Some(workspace_ctx),
        };

        // Parse -> Resolve -> Evaluate
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

        // Set up ID prefix index for shortest unique prefixes
        let id_prefix_context = IdPrefixContext::default();
        let id_prefix_index = id_prefix_context
            .populate(repo)
            .wrap_err("failed to populate ID prefix index")?;

        // Iterate graph nodes
        let mut entries = Vec::new();
        for node_result in revset.iter_graph() {
            let (commit_id, edges): GraphNode<CommitId> =
                node_result.wrap_err("error iterating revset graph")?;
            let commit = repo
                .store()
                .get_commit(&commit_id)
                .wrap_err("failed to load commit")?;

            let info = self.extract_commit_info(&commit, &id_prefix_index)?;
            let dag_edges = edges
                .into_iter()
                .map(|e| Edge {
                    target: e.target.hex(),
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

    /// Compute the file-level changes for a commit (diff against parent tree).
    pub fn file_changes(&self, commit_hex_id: &str) -> Result<Vec<FileChange>> {
        let repo = self.repo.as_ref();
        let commit_id = CommitId::try_from_hex(commit_hex_id)
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
        let mut diff_stream = parent_tree.diff_stream(&commit_tree, &EverythingMatcher);

        // Collect the stream synchronously.
        while let Some(entry) = diff_stream.next().block_on() {
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
        }

        Ok(changes)
    }

    fn extract_commit_info(
        &self,
        commit: &Commit,
        id_prefix_index: &jj_lib::id_prefix::IdPrefixIndex<'_>,
    ) -> Result<CommitInfo> {
        let repo = self.repo.as_ref();

        // Shortest unique change ID prefix
        let change_id_len = id_prefix_index
            .shortest_change_prefix_len(repo, commit.change_id())
            .unwrap_or(8);
        let change_id_full = commit.change_id().reverse_hex();
        let change_id = change_id_full
            .get(..change_id_len)
            .unwrap_or(&change_id_full)
            .to_string();

        // Shortest unique commit ID prefix
        let commit_id_len = id_prefix_index
            .shortest_commit_prefix_len(repo, commit.id())
            .unwrap_or(8);
        let commit_id_full = commit.id().hex();
        let commit_id = commit_id_full
            .get(..commit_id_len)
            .unwrap_or(&commit_id_full)
            .to_string();

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

        // Bookmarks
        let bookmarks: Vec<String> = repo
            .view()
            .local_bookmarks_for_commit(commit.id())
            .map(|(name, _)| name.as_str().to_string())
            .collect();

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
            bookmarks,
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
