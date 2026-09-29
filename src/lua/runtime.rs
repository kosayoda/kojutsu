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

    /// Infallible: a broken `init.lua` yields a runtime carrying whatever it
    /// set and registered before the error, reported via
    /// [`Self::init_error`].
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

    /// Problems with what `init.lua` registered, as `file:line: message`.
    pub fn init_warnings(&self) -> &[String] {
        &self.engine.init_warnings
    }
}

#[cfg(test)]
mod tests {
    use super::LuaRuntime;

    /// Total bindings visible in the DAG view's help.
    fn binding_count(runtime: &LuaRuntime) -> usize {
        crate::keymap::help_entries(
            runtime.keymaps.for_view(crate::types::ActiveView::Dag),
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

    /// What ran before an error is kept, so a typo late in the file doesn't
    /// throw away the config above it.
    #[test]
    fn a_broken_script_keeps_what_ran_before_the_error() {
        let dir = config_dir_with(
            r#"
            kojutsu.config.tab_width = 2
            kojutsu.bind { action = "abandon", key = "ctrl-y" }
            error('boom')
            kojutsu.config.tab_width = 7
            "#,
        );
        let repo = std::path::Path::new(".");
        let runtime = LuaRuntime::load_from(dir.path(), repo);
        assert!(runtime.init_error().is_some_and(|e| e.contains("boom")));
        assert_eq!(runtime.config.tab_width, 2);
        let clean = LuaRuntime::load_from(config_dir_with("").path(), repo);
        assert_eq!(binding_count(&runtime), binding_count(&clean) + 1);
    }

    fn warnings_for(init: &str) -> Vec<String> {
        let dir = config_dir_with(init);
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        assert_eq!(runtime.init_error(), None, "warnings never stop the script");
        runtime.init_warnings().to_vec()
    }

    /// A registration mistake is reported with where it was made, skipped,
    /// and the rest of the file still loads.
    #[test]
    fn a_bad_registration_is_reported_and_the_rest_still_loads() {
        let dir = config_dir_with(
            r#"
            kojutsu.bind { action = "bookmark_view_edit", key = "ctrl-y" }
            kojutsu.bind { action = "abandon", key = "ctrl-g" }
            "#,
        );
        let repo = std::path::Path::new(".");
        let runtime = LuaRuntime::load_from(dir.path(), repo);
        let clean = LuaRuntime::load_from(config_dir_with("").path(), repo);

        assert_eq!(runtime.init_warnings().len(), 1);
        let warning = &runtime.init_warnings()[0];
        assert!(warning.contains("init.lua:2:"), "{warning}");
        assert!(
            warning.contains("unknown action `bookmark_view_edit`"),
            "{warning}"
        );
        assert_eq!(binding_count(&runtime), binding_count(&clean) + 1);
    }

    #[test]
    fn mistyped_and_invalid_options_are_named() {
        let warnings = warnings_for(
            r#"
            kojutsu.bind { action = "abandon", key = { "ctrl-y" } }
            kojutsu.bind { action = "abandon", key = "ctrl-shift-nope" }
            kojutsu.bind { action = "abandon" }
            kojutsu.prefix { key = "ctrl-p", label = "p", scope = "nowhere" }
            kojutsu.command("c", function() end, { selection = "lines", key = "ctrl-l" })
            kojutsu.hook("abandn", "pre", function() end)
            kojutsu.hook("abandon", "during", function() end)
            "#,
        );
        let expected = [
            "`key` must be a string, not a table",
            "invalid key `ctrl-shift-nope`",
            "needs a `key` or `seq`",
            "unknown scope `nowhere`",
            "unknown selection `lines`",
            "`abandn` matches no action",
            "phase must be `pre` or `post`",
        ];
        assert_eq!(warnings.len(), expected.len(), "{warnings:#?}");
        for (warning, expected) in warnings.iter().zip(expected) {
            assert!(warning.contains(expected), "{warning} lacks {expected}");
        }
    }

    /// A help group only decides where a key is listed, so a wrong one costs
    /// the command nothing: it is listed under `commands` and still runs.
    #[test]
    fn an_unknown_group_keeps_the_command() {
        let dir = config_dir_with(
            r#"kojutsu.command("c", function() end, { group = "nope", key = "ctrl-l" })"#,
        );
        let repo = std::path::Path::new(".");
        let runtime = LuaRuntime::load_from(dir.path(), repo);
        let clean = LuaRuntime::load_from(config_dir_with("").path(), repo);
        let warnings = runtime.init_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown group `nope`; listed under `commands`"));
        assert_eq!(binding_count(&runtime), binding_count(&clean) + 1);
    }

    /// Overriding a default with `bind` is allowed but said out loud;
    /// `rebind` is how to say it was meant.
    #[test]
    fn only_bind_warns_about_shadowing_a_default() {
        let bind = warnings_for(r#"kojutsu.bind { action = "abandon", key = "q" }"#);
        assert!(bind.iter().any(|w| w.contains("shadows a default binding")));
        assert!(warnings_for(r#"kojutsu.rebind { action = "abandon", key = "q" }"#).is_empty());
    }

    /// The caller keeps the running runtime when this is set, so it has to be
    /// set rather than the failure being swallowed.
    #[test]
    fn a_broken_script_reports_an_error() {
        let dir = config_dir_with("error('boom')\n");
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        assert!(runtime.init_error().is_some_and(|e| e.contains("boom")));
    }

    fn runtime_with(init: &str) -> LuaRuntime {
        let dir = config_dir_with(init);
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        assert_eq!(runtime.init_error(), None);
        assert!(
            runtime.init_warnings().is_empty(),
            "{:?}",
            runtime.init_warnings()
        );
        runtime
    }

    fn pre_hooks(runtime: &LuaRuntime, app: &mut crate::app::App) -> crate::lua::HookOutcome {
        runtime.engine.run_pre_hooks(
            crate::keymap::AppAction::Abandon,
            crate::keymap::CommandFlags::empty(),
            app,
        )
    }

    #[test]
    fn a_pre_hook_returning_false_cancels_the_action() {
        let runtime =
            runtime_with(r#"kojutsu.hook("abandon", "pre", function() return false end)"#);
        let mut app = crate::app::App::for_test();
        assert!(matches!(
            pre_hooks(&runtime, &mut app),
            crate::lua::HookOutcome::Cancel
        ));
    }

    /// A failing hook is reported and doesn't stop the action it hooks.
    #[test]
    fn a_failing_pre_hook_is_reported_and_the_action_proceeds() {
        let runtime =
            runtime_with(r#"kojutsu.hook("abandon", "pre", function() error("oops") end)"#);
        let mut app = crate::app::App::for_test();
        assert!(matches!(
            pre_hooks(&runtime, &mut app),
            crate::lua::HookOutcome::Proceed
        ));
        runtime.engine.flush_logs(&mut app);
        assert!(
            app.command_log
                .entries
                .iter()
                .any(|e| String::from_utf8_lossy(&e.output).contains("oops"))
        );
    }

    #[test]
    fn a_post_hook_sees_the_commands_result() {
        let runtime = runtime_with(
            r#"kojutsu.hook("abandon", "post", function(ctx, result)
                kojutsu.ui.status(result.status .. " " .. result.output)
            end)"#,
        );
        let mut app = crate::app::App::for_test();
        runtime.engine.run_post_hooks(
            crate::keymap::AppAction::Abandon,
            &mut app,
            crate::lua::CommandOutcome {
                success: true,
                cancelled: false,
                code: Some(0),
                output: b"done",
            },
        );
        runtime.engine.flush_logs(&mut app);
        assert_eq!(
            app.status_message.map(|(msg, _)| msg).as_deref(),
            Some("ok done")
        );
    }

    /// A command's runtime error lands in the command log like a hook's.
    #[test]
    fn a_failing_command_is_recorded_in_the_command_log() {
        let runtime = runtime_with(r#"kojutsu.command("bad", function() error("kaput") end, {})"#);
        let mut app = crate::app::App::for_test();
        runtime
            .engine
            .execute_command(0, &mut app, crate::keymap::CommandFlags::empty());
        assert!(
            app.command_log
                .entries
                .iter()
                .any(|e| String::from_utf8_lossy(&e.output).contains("kaput"))
        );
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
