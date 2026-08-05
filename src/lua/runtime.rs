use std::path::Path;
use std::rc::Rc;

use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
use crate::theme::Config;

use super::LuaEngine;

/// Everything derived from the user's config directory, built as one unit.
pub struct LuaRuntime {
    pub config: Rc<Config>,
    pub engine: LuaEngine,
    pub keymaps: Keymaps,
}

impl LuaRuntime {
    /// Load from the user's config directory, or built-in defaults when the
    /// platform has none.
    pub fn load(repo_path: &Path) -> Self {
        match crate::theme::kojutsu_config_dir() {
            Some(dir) => Self::load_from(&dir, repo_path),
            None => Self::load_from(Path::new(""), repo_path),
        }
    }

    /// Infallible: a broken `init.lua` yields a runtime carrying whatever
    /// registered before the error, reported via [`Self::init_error`].
    pub fn load_from(config_dir: &Path, repo_path: &Path) -> Self {
        let mut config = crate::theme::Config::default();
        let mut registry = ActionRegistry::new();
        let default_specs = default_bindings();
        let mut engine = LuaEngine::new(
            repo_path,
            &mut registry,
            &default_specs,
            &mut config,
            config_dir,
        );
        let mut specs = default_specs;
        specs.extend(engine.take_extra_bindings());
        let keymaps = Keymaps::build(specs, registry);
        Self {
            config: Rc::new(config),
            engine,
            keymaps,
        }
    }

    /// The error that stopped `init.lua`, if it didn't run to completion.
    pub fn init_error(&self) -> Option<&str> {
        self.engine.init_error.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::LuaRuntime;

    /// Total bindings visible in the DAG view's help.
    fn binding_count(runtime: &LuaRuntime) -> usize {
        crate::keymap::help_entries(
            runtime.keymaps.for_view(crate::app::ActiveView::Dag),
            &runtime.keymaps.registry,
            &runtime.config.revsets.presets,
        )
        .iter()
        .map(|(_, entries)| entries.len())
        .sum()
    }

    fn config_dir_with(init: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("init.lua"), init).expect("write init.lua");
        dir
    }

    /// `kojutsu.command` and `kojutsu.bind` append and allocate action ids, so
    /// a reload that re-ran the script over the live engine would double every
    /// binding. Loading builds a whole new runtime instead; this is what says
    /// so.
    #[test]
    fn loading_repeatedly_does_not_accumulate_bindings() {
        let dir = config_dir_with(
            r#"
            kojutsu.bind { action = "abandon", key = "ctrl-y" }
            kojutsu.command("hi", function() end, { key = "ctrl-g" })
            "#,
        );
        let repo = std::path::Path::new(".");

        let first = LuaRuntime::load_from(dir.path(), repo);
        assert_eq!(first.init_error(), None);
        let counts: Vec<usize> = (0..3)
            .map(|_| binding_count(&LuaRuntime::load_from(dir.path(), repo)))
            .collect();

        assert_eq!(counts, vec![binding_count(&first); 3]);
    }

    /// A reload has to reflect what the file says now, including bindings it
    /// used to declare and no longer does, which re-running over the live
    /// engine could never do, since nothing there knows to remove them.
    #[test]
    fn loading_again_picks_up_an_edit_and_drops_what_it_removed() {
        let dir = config_dir_with(
            r#"
            kojutsu.config.tab_width = 2
            kojutsu.bind { action = "abandon", key = "ctrl-y" }
            "#,
        );
        let repo = std::path::Path::new(".");
        let before = LuaRuntime::load_from(dir.path(), repo);
        assert_eq!(before.config.tab_width, 2);

        std::fs::write(
            dir.path().join("init.lua"),
            "kojutsu.config.tab_width = 7\n",
        )
        .unwrap();
        let after = LuaRuntime::load_from(dir.path(), repo);

        assert_eq!(after.config.tab_width, 7);
        assert_eq!(binding_count(&after), binding_count(&before) - 1);
    }

    /// The caller keeps the running runtime when this is set, so it has to be
    /// set rather than the failure being swallowed.
    #[test]
    fn a_broken_script_reports_an_error() {
        let dir = config_dir_with("error('boom')\n");
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        assert!(runtime.init_error().is_some_and(|e| e.contains("boom")));
    }

    /// Run the first registered command and report what it left the config
    /// table as.
    fn run_command(runtime: &LuaRuntime) -> Option<Result<crate::theme::Config, String>> {
        let mut app = crate::app::App::for_test();
        runtime
            .engine
            .execute_command(0, &mut app, crate::keymap::CommandFlags::empty());
        runtime.engine.take_config_change(&runtime.config)
    }

    #[test]
    fn a_command_writing_to_the_config_table_is_picked_up() {
        let dir = config_dir_with(
            r#"kojutsu.command("dark", function() kojutsu.config.theme.accent = "red" end, {})"#,
        );
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        assert_eq!(runtime.init_error(), None);
        // Loading is not a plugin write; there is nothing to adopt yet.
        assert!(runtime.engine.take_config_change(&runtime.config).is_none());

        let change = run_command(&runtime).expect("a change").expect("valid");
        assert_eq!(change.theme.accent, ratatui::style::Color::Red);
    }

    /// The table is re-read by polling, so the same write must not be
    /// reported twice: the second look has nothing new to say.
    #[test]
    fn the_same_write_is_only_reported_once() {
        let dir = config_dir_with(
            r#"kojutsu.command("dark", function() kojutsu.config.theme.accent = "red" end, {})"#,
        );
        let mut runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        let change = run_command(&runtime).expect("a change").expect("valid");
        runtime.config = std::rc::Rc::new(change);
        assert!(runtime.engine.take_config_change(&runtime.config).is_none());
    }

    #[test]
    fn a_command_that_leaves_the_config_alone_reports_nothing() {
        let dir = config_dir_with(r#"kojutsu.command("noop", function() end, {})"#);
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        assert!(run_command(&runtime).is_none());
    }

    #[test]
    fn a_command_writing_an_unknown_key_reports_the_error() {
        let dir = config_dir_with(
            r#"kojutsu.command("oops", function() kojutsu.config.theme.acccent = "red" end, {})"#,
        );
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        let err = run_command(&runtime)
            .expect("a change")
            .expect_err("invalid");
        assert!(err.contains("unknown field `acccent`"), "{err}");
    }

    /// No init.lua at all is the common case, not an error.
    #[test]
    fn an_empty_config_directory_loads_clean() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        assert_eq!(runtime.init_error(), None);
        assert_eq!(*runtime.config, crate::theme::Config::default());
    }
}
