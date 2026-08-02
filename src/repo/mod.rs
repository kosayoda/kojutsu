mod annotate;
mod commit_info;
mod config;
mod diff;
mod operations;
mod revset;
mod workspace_view;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::Context;
use jj_lib::config::{ConfigGetResultExt as _, StackedConfig};
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::id_prefix::IdPrefixContext;
use jj_lib::object_id::ObjectId;
use jj_lib::ref_name::WorkspaceNameBuf;
use jj_lib::repo::{ReadonlyRepo, Repo as _, StoreFactories};
use jj_lib::repo_path::RepoPathUiConverter;
use jj_lib::revset::{
    RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
    RevsetWorkspaceContext,
};
use jj_lib::settings::UserSettings;
use jj_lib::time_util::DatePatternContext;
use jj_lib::workspace::{Workspace, default_working_copy_factories};

pub use diff::{assemble_resolution, has_conflict_markers, hunk_markers};
pub use operations::millis_to_relative_time;

use crate::types::{BookmarkName, RemoteName};

/// Number of hex characters to show for change/commit IDs.
pub(super) const DISPLAY_ID_LEN: usize = 8;

/// Error from `snapshot` or `update_stale` shell-outs.
pub enum SnapshotError {
    /// The working copy is stale (needs `jj workspace update-stale`).
    Stale(String),
    /// Any other failure (spawn error, non-stale jj error, etc.).
    Other(String),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stale(msg) | Self::Other(msg) => f.write_str(msg),
        }
    }
}

/// Thin adapter around jj-lib. Owns the workspace and repo, converts
/// jj-lib types into our domain types so nothing leaks out.
/// Default max bytes of file content (per side) materialized for a diff.
/// Overridable via `diff.max_file_size_mib` in the config file.
const DEFAULT_DIFF_SIZE_LIMIT: usize = 64 * 1024 * 1024;

pub struct JjRepo {
    pub(super) repo: Arc<ReadonlyRepo>,
    pub(super) settings: UserSettings,
    pub(super) aliases_map: RevsetAliasesMap,
    pub(super) workspace_name: WorkspaceNameBuf,
    pub(super) workspace_root: PathBuf,
    /// The synthetic remote `remote_bookmarks()` ignores unless asked for it
    /// by name — `git` in a git-backed repo, nothing otherwise. Revsets parse
    /// differently with and without it, so `trunk()` depends on getting this
    /// right.
    pub(super) default_ignored_remote: Option<&'static jj_lib::ref_name::RemoteName>,
    /// Max bytes of file content (per side) materialized for a diff —
    /// a memory guard; larger files get a placeholder.
    pub(super) diff_size_limit: usize,
}

impl JjRepo {
    /// Trigger a working copy snapshot so the repo reflects the current
    /// filesystem state. Shells out to `jj status` which snapshots as a
    /// side effect.
    pub fn snapshot(repo_path: &Path) -> std::result::Result<(), SnapshotError> {
        let output = std::process::Command::new("jj")
            .arg("status")
            .arg("-R")
            .arg(repo_path)
            .arg("--quiet")
            .arg("--color=never")
            .output()
            .map_err(|e| SnapshotError::Other(format!("failed to run jj: {e}")))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if stderr.contains("stale") {
                Err(SnapshotError::Stale(stderr))
            } else {
                Err(SnapshotError::Other(stderr))
            }
        }
    }

    /// Run `jj workspace update-stale` to recover a stale working copy.
    pub fn update_stale(repo_path: &Path) -> std::result::Result<(), SnapshotError> {
        let output = std::process::Command::new("jj")
            .arg("workspace")
            .arg("update-stale")
            .arg("-R")
            .arg(repo_path)
            .arg("--color=never")
            .output()
            .map_err(|e| SnapshotError::Other(format!("failed to run jj: {e}")))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(SnapshotError::Other(stderr))
        }
    }

    /// Open the jj workspace rooted at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let config = config::load(path)?;
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

        let default_ignored_remote = default_ignored_remote(repo.store());

        Ok(Self {
            repo,
            settings,
            aliases_map,
            workspace_name,
            workspace_root,
            default_ignored_remote,
            diff_size_limit: DEFAULT_DIFF_SIZE_LIMIT,
        })
    }

    /// Set the max bytes of file content (per side) materialized for a diff.
    pub fn set_diff_size_limit(&mut self, bytes: usize) {
        self.diff_size_limit = bytes;
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
            .filter(|(symbol, _)| Some(symbol.remote) != self.default_ignored_remote)
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
                "present(@) | ancestors(immutable_heads()..@, 2) | trunk() | ancestors(trunk(), 16)"
                    .to_string()
            })
    }

    /// Read the `run.jobs` config for a workspace, if set to a usable value.
    pub fn read_run_jobs(workspace_path: &Path) -> Option<usize> {
        let config = config::load(workspace_path).ok()?;
        let jobs = config.get::<i64>("run.jobs").ok()?;
        usize::try_from(jobs).ok().filter(|&n| n > 0)
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
                    let _ = aliases_map.insert(decl, defn, None);
                }
            }
        }

        Ok(aliases_map)
    }

    /// Build a [`RevsetParseContext`] with loaded aliases and proper user email.
    pub(super) fn revset_parse_context<'a>(
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
            default_ignored_remote: self.default_ignored_remote,
            fileset_aliases_map,
            extensions,
            workspace: Some(workspace_ctx),
        }
    }

    /// Build an `IdPrefixContext` over the same disambiguation set jj uses, so
    /// that a prefix shown here still resolves when handed back to the `jj`
    /// CLI as a revision argument.
    pub(super) fn build_id_prefix_context(
        &self,
        context: &RevsetParseContext<'_>,
    ) -> Result<IdPrefixContext> {
        let ctx = IdPrefixContext::new(Arc::new(RevsetExtensions::default()));
        let Some(revset_str) = short_prefixes_revset(self.settings.config())? else {
            return Ok(ctx);
        };
        let mut diag = RevsetDiagnostics::new();
        let expression = jj_lib::revset::parse(&mut diag, &revset_str, context)
            .wrap_err_with(|| format!("invalid ID prefix disambiguation revset `{revset_str}`"))?;
        Ok(ctx.disambiguate_within(expression))
    }
}

/// The revset jj disambiguates change/commit ID prefixes within:
/// `revsets.short-prefixes`, falling back to `revsets.log` when it is unset.
/// An empty string means "no disambiguation" — prefixes are then made unique
/// against the whole index, hidden commits included.
///
/// Mirrors jj-cli's `load_short_prefixes_expression`. Getting this wrong in
/// either direction is user-visible: a wider set than jj's shows needlessly
/// long IDs, a narrower one shows IDs that `jj` rejects as ambiguous.
fn short_prefixes_revset(config: &StackedConfig) -> Result<Option<String>> {
    let revset = match config
        .get::<String>("revsets.short-prefixes")
        .optional()
        .wrap_err("invalid `revsets.short-prefixes`")?
    {
        Some(revset) => revset,
        // Absent only if the vendored jj defaults failed to load; degrading to
        // whole-index prefixes is correct, just verbose.
        None => config
            .get::<String>("revsets.log")
            .optional()
            .wrap_err("invalid `revsets.log`")?
            .unwrap_or_default(),
    };
    Ok((!revset.is_empty()).then_some(revset))
}

/// The remote that `remote_bookmarks()` skips by default. Git-backed repos
/// expose a synthetic `git` remote mirroring the colocated git repo's refs,
/// which jj hides unless it is named explicitly. Mirrors jj-cli's
/// `default_ignored_remote_name`.
fn default_ignored_remote(
    store: &jj_lib::store::Store,
) -> Option<&'static jj_lib::ref_name::RemoteName> {
    jj_lib::git::get_git_backend(store)
        .is_ok()
        .then_some(jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO)
}

/// Parse a commit description, returning `None` for empty/placeholder descriptions.
pub(super) fn parse_first_line_description(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "(no description set)" {
        None
    } else {
        trimmed.lines().next().map(String::from)
    }
}

use pollster::FutureExt as _;

#[cfg(test)]
mod tests {
    use jj_lib::config::{ConfigLayer, ConfigSource};

    use super::*;

    fn config(toml: &str) -> StackedConfig {
        let mut config = StackedConfig::empty();
        config.add_layer(ConfigLayer::parse(ConfigSource::User, toml).unwrap());
        config
    }

    #[test]
    fn short_prefixes_falls_back_to_the_log_revset() {
        // jj ships no default for `revsets.short-prefixes`, so this fallback is
        // the one that actually fires. Skipping it makes every ID a character
        // or two longer than what `jj log` prints for the same revset.
        let config = config("[revsets]\nlog = 'trunk()'\n");
        assert_eq!(
            short_prefixes_revset(&config).unwrap().as_deref(),
            Some("trunk()")
        );
    }

    #[test]
    fn an_explicit_short_prefixes_revset_wins() {
        let config = config("[revsets]\nlog = 'trunk()'\nshort-prefixes = 'mine()'\n");
        assert_eq!(
            short_prefixes_revset(&config).unwrap().as_deref(),
            Some("mine()")
        );
    }

    #[test]
    fn an_empty_revset_opts_out_of_disambiguation() {
        let empty_short_prefixes = config("[revsets]\nlog = 'trunk()'\nshort-prefixes = ''\n");
        assert_eq!(short_prefixes_revset(&empty_short_prefixes).unwrap(), None);

        let empty_log = config("[revsets]\nlog = ''\n");
        assert_eq!(short_prefixes_revset(&empty_log).unwrap(), None);
    }

    #[test]
    fn a_mistyped_revset_is_an_error_rather_than_a_silent_fallback() {
        let config = config("[revsets]\nshort-prefixes = 42\n");
        assert!(short_prefixes_revset(&config).is_err());
    }

    #[test]
    fn the_vendored_jj_defaults_supply_the_fallback() {
        // The fallback above is only load-bearing while the vendored jj-cli
        // config keeps defining `revsets.log`.
        let config = config(config::DEFAULT_REVSETS_TOML);
        assert!(short_prefixes_revset(&config).unwrap().is_some());
    }
}
