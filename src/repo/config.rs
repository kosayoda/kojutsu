//! Discovery of the config files jj itself would read for a workspace.
//!
//! Kojutsu shows IDs and evaluates revsets that the user then hands straight
//! back to the `jj` CLI, so the two must resolve `revsets.log`, `trunk()` and
//! friends identically. That only holds if we read the same files, in the same
//! order, that jj does: this module mirrors jj-cli's `ConfigEnv`.

use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::Context as _;
use etcetera::BaseStrategy as _;
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::workspace::{DefaultWorkspaceLoaderFactory, WorkspaceLoaderFactory as _};

/// Vendored jj-cli default revset configuration.
pub(super) const DEFAULT_REVSETS_TOML: &str = include_str!("../../vendored/revsets.toml");

/// Length of the hex config ID naming a secure config directory
/// (jj's `CONFIG_ID_BYTES * 2`).
const CONFIG_ID_LEN: usize = 20;

/// Build the config stack jj would see for the workspace rooted at
/// `workspace_path`: jj-lib defaults, vendored jj-cli defaults, user files,
/// the repo and workspace files, then environment overrides.
///
/// Individual layers are best-effort: a repo with an unreadable user config
/// should still open, just with jj's defaults.
pub(super) fn load(workspace_path: &Path) -> Result<StackedConfig> {
    let mut config = StackedConfig::with_defaults();

    // Vendored jj-cli defaults (revset aliases, the default log revset) go in
    // as a Default layer so user and repo config still override them.
    let cli_defaults = ConfigLayer::parse(ConfigSource::Default, DEFAULT_REVSETS_TOML)
        .wrap_err("failed to parse vendored revsets.toml")?;
    config.add_layer(cli_defaults);

    for path in user_config_paths() {
        load_path(&mut config, ConfigSource::User, &path);
    }

    // The repo path is read from `.jj/repo`, which is a pointer file rather
    // than a directory in secondary workspaces: let jj-lib resolve it.
    match DefaultWorkspaceLoaderFactory.create(workspace_path) {
        Ok(loader) => {
            if let Some(path) = repo_config_path(loader.repo_path()) {
                load_path(&mut config, ConfigSource::Repo, &path);
            }
        }
        Err(e) => tracing::warn!("failed to locate repo dir for config: {e}"),
    }
    if let Some(path) = workspace_config_path(&workspace_path.join(".jj")) {
        load_path(&mut config, ConfigSource::Workspace, &path);
    }

    config.add_layer(env_overrides_layer());

    Ok(config)
}

/// Load one discovered path, which may be a file or a `conf.d`-style
/// directory. Missing paths are skipped: jj lists its platform config file
/// whether or not it exists so that `jj config edit` can create it.
fn load_path(config: &mut StackedConfig, source: ConfigSource, path: &Path) {
    let result = if path.is_dir() {
        config.load_dir(source, path)
    } else if path.is_file() {
        config.load_file(source, path)
    } else {
        return;
    };
    if let Err(e) = result {
        tracing::warn!("failed to load config {}: {e}", path.display());
    }
}

/// The user-level config paths jj reads, in ascending precedence. Mirrors
/// jj-cli's `UnresolvedConfigEnv::resolve`.
fn user_config_paths() -> Vec<PathBuf> {
    // A set `JJ_CONFIG` replaces every default location rather than adding to
    // them, and may name several paths.
    if let Some(paths) = std::env::var_os("JJ_CONFIG") {
        return std::env::split_paths(&paths)
            .filter(|path| !path.as_os_str().is_empty())
            .collect();
    }

    let strategy = etcetera::choose_base_strategy()
        .inspect_err(|e| tracing::warn!("failed to resolve config directory: {e}"))
        .ok();
    let home_config = strategy
        .as_ref()
        .map(|s| s.home_dir().join(".jjconfig.toml"));
    let config_dir = strategy.map(|s| s.config_dir().join("jj"));
    let platform_config = config_dir.as_ref().map(|dir| dir.join("config.toml"));

    let mut paths = Vec::new();
    // The legacy home-directory config is only consulted when it exists, so
    // that a fresh install writes to the platform path instead.
    if let Some(path) = home_config
        && (path.exists() || platform_config.is_none())
    {
        paths.push(path);
    }
    paths.extend(platform_config);
    if let Some(dir) = config_dir.map(|dir| dir.join("conf.d"))
        && dir.exists()
    {
        paths.push(dir);
    }
    paths
}

/// The root under which jj stores repo- and workspace-scoped config.
/// Deliberately independent of `JJ_CONFIG`, matching jj.
fn secure_config_root() -> Option<PathBuf> {
    let strategy = etcetera::choose_base_strategy().ok()?;
    Some(strategy.config_dir().join("jj"))
}

fn repo_config_path(repo_dir: &Path) -> Option<PathBuf> {
    secure_config_path(repo_dir, "config-id", "repos", "config.toml")
}

fn workspace_config_path(workspace_dot_jj: &Path) -> Option<PathBuf> {
    secure_config_path(
        workspace_dot_jj,
        "workspace-config-id",
        "workspaces",
        "workspace-config.toml",
    )
}

/// Resolve a repo- or workspace-scoped config file.
///
/// Since jj v0.37 these live under the user's config directory rather than in
/// the repo, keyed by an ID file inside it, so that cloning a repo cannot drop
/// config onto your machine. Repos written by older jj still have the config
/// inline, so fall back to `legacy_name` when no ID file is present.
///
/// Read-only by design. jj's own `SecureConfig` generates directories and
/// migrates legacy files as a side effect of reading them; kojutsu re-opens
/// the workspace on every refresh and has no business mutating the user's
/// config. Migration will already have happened via the `jj` we shell out to.
fn secure_config_path(dir: &Path, id_name: &str, kind: &str, legacy_name: &str) -> Option<PathBuf> {
    let legacy = || Some(dir.join(legacy_name)).filter(|path| path.is_file());

    let Ok(config_id) = std::fs::read_to_string(dir.join(id_name)) else {
        return legacy();
    };
    let config_id = config_id.trim();
    if config_id.len() != CONFIG_ID_LEN || !config_id.chars().all(|c| c.is_ascii_hexdigit()) {
        tracing::warn!("ignoring malformed {id_name} in {}", dir.display());
        return legacy();
    }
    Some(
        secure_config_root()?
            .join(kind)
            .join(config_id)
            .join("config.toml"),
    )
}

/// Environment variables that override config values.
///
/// Only the identity pair is mirrored: `user.email` feeds the revset parse
/// context (and so `mine()`), while the rest of jj's env layer covers operation
/// metadata we never write and CLI presentation kojutsu handles itself.
fn env_overrides_layer() -> ConfigLayer {
    let mut layer = ConfigLayer::empty(ConfigSource::EnvOverrides);
    for (var, key) in [("JJ_USER", "user.name"), ("JJ_EMAIL", "user.email")] {
        if let Some(value) = std::env::var_os(var).and_then(|v| v.into_string().ok()) {
            layer
                .set_value(key, value)
                .expect("`user.name`/`user.email` are valid config keys");
        }
    }
    layer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repo_config_id_points_into_the_user_config_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config-id"), "0123456789abcdef0123").unwrap();

        let path = repo_config_path(dir.path()).unwrap();
        assert!(
            path.ends_with("jj/repos/0123456789abcdef0123/config.toml"),
            "unexpected path {}",
            path.display()
        );
    }

    #[test]
    fn a_trailing_newline_in_the_id_file_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config-id"), "0123456789abcdef0123\n").unwrap();

        let path = repo_config_path(dir.path()).unwrap();
        assert!(path.ends_with("0123456789abcdef0123/config.toml"));
    }

    #[test]
    fn a_repo_without_an_id_file_falls_back_to_the_legacy_path() {
        // Repos last touched by jj < 0.37 still keep their config inline.
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("config.toml");
        std::fs::write(&legacy, "[revsets]\n").unwrap();

        assert_eq!(repo_config_path(dir.path()), Some(legacy));
    }

    #[test]
    fn a_repo_with_neither_has_no_config() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(repo_config_path(dir.path()), None);
    }

    #[test]
    fn a_malformed_id_file_does_not_resolve_to_a_stray_directory() {
        let dir = tempfile::tempdir().unwrap();
        // Short, and `zz` is not hex: either would name the wrong directory.
        for id in ["0123", "0123456789abcdef01zz"] {
            std::fs::write(dir.path().join("config-id"), id).unwrap();
            assert_eq!(repo_config_path(dir.path()), None, "accepted id {id:?}");
        }
    }

    #[test]
    fn workspace_config_uses_its_own_id_and_directory() {
        // Repo and workspace config must not collide: a workspace keyed by the
        // same hex would otherwise read the repo's file.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("workspace-config-id"),
            "0123456789abcdef0123",
        )
        .unwrap();

        let path = workspace_config_path(dir.path()).unwrap();
        assert!(
            path.ends_with("jj/workspaces/0123456789abcdef0123/config.toml"),
            "unexpected path {}",
            path.display()
        );
    }
}
