use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::app::{App, CommandLogKind};
use crate::config::Config;
use crate::jj_version::InstalledJj;
use crate::keymap::{ActionRegistry, Keymaps, default_bindings};

use super::LuaEngine;

/// Everything derived from the user's config directory, built as one unit.
pub struct LuaRuntime {
    pub config: Rc<Config>,
    pub engine: LuaEngine,
    pub keymaps: Keymaps,
    config_dir: PathBuf,
}

impl LuaRuntime {
    /// Load from the user's config directory, or built-in defaults when the
    /// platform has none.
    pub fn load(repo_path: &Path) -> Self {
        match crate::config::kojutsu_config_dir() {
            Some(dir) => Self::load_from(&dir, repo_path),
            None => Self::load_from(Path::new(""), repo_path),
        }
    }

    /// Infallible: a broken `init.lua` yields a runtime carrying whatever it
    /// set and registered before the error, reported via
    /// [`Self::init_error`].
    pub fn load_from(config_dir: &Path, repo_path: &Path) -> Self {
        let mut config = crate::config::Config::default();
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
            config_dir: config_dir.to_path_buf(),
        }
    }

    /// Rebuild from the same `init.lua` and swap the result in, then adopt
    /// what `probe_jj` finds, since jj may have been upgraded meanwhile.
    ///
    /// All-or-nothing: the new runtime is built first and only installed if
    /// its script ran clean, so a broken edit leaves the running one
    /// untouched. The rebuild is wholesale rather than a re-run over the
    /// live engine, because registering a command or binding appends: a
    /// second pass would double every one of them, and a fresh Lua also
    /// drops `package.loaded`, so `require`d modules are re-read instead of
    /// served from cache.
    pub fn reload(&mut self, app: &mut App, probe_jj: impl FnOnce() -> InstalledJj) {
        // A parked thread belongs to the engine that created it, and the
        // reload drops that engine along with the prompt it is waiting on.
        if self.engine.has_suspended_thread() {
            app.set_error("reload: a plugin prompt is still open");
            return;
        }

        let reloaded = Self::load_from(&self.config_dir, &self.engine.repo_path);
        if let Some(err) = reloaded.init_error() {
            app.push_command_log(
                CommandLogKind::Warning,
                "reload failed - keeping the running config",
                None,
                err.as_bytes().to_vec(),
                false,
            );
            app.set_error(format!("reload: {err}"));
            return;
        }

        // What Lua logged and has yet to be shown would go with the engine.
        self.engine.flush_logs(app);
        *self = reloaded;
        app.apply_reloaded_config(self.config.clone());
        app.push_command_log(
            CommandLogKind::Background,
            "reloaded init.lua",
            None,
            Vec::new(),
            true,
        );
        app.set_status("reloaded init.lua");
        self.report_init_warnings(app);
        app.set_installed_jj(probe_jj());
    }

    /// The error that stopped `init.lua`, if it didn't run to completion.
    pub fn init_error(&self) -> Option<&str> {
        self.engine.init_error.as_deref()
    }

    /// Problems with what `init.lua` registered, as `file:line: message`.
    pub fn init_warnings(&self) -> &[String] {
        &self.engine.init_warnings
    }

    /// Show what `init.lua` registered wrongly: one command-log entry listing
    /// every problem, and the count in the status line pointing at it.
    pub fn report_init_warnings(&self, app: &mut App) {
        let warnings = self.init_warnings();
        if warnings.is_empty() {
            return;
        }
        let count = warnings.len();
        let summary = format!(
            "init.lua: {count} {}",
            crate::pluralize!(count, "problem", "problems")
        );
        app.push_command_log(
            CommandLogKind::Warning,
            summary.clone(),
            None,
            warnings.join("\n").into_bytes(),
            false,
        );
        app.set_error(format!("{summary} (see the command log)"));
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
                .entries()
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
                .entries()
                .any(|e| String::from_utf8_lossy(&e.output).contains("kaput"))
        );
    }

    /// A command reads the commit under the cursor from whichever view it
    /// runs in, not only the DAG.
    #[test]
    fn a_command_sees_the_commit_under_the_cursor_in_any_view() {
        use crate::app::{App, BookmarkKind, BookmarkViewEntry};
        use crate::types::{ActiveView, BookmarkName, CommitId, RevisionArg};

        let runtime = runtime_with(
            r#"kojutsu.command("show", function(ctx)
                kojutsu.ui.status(ctx.view .. " " .. tostring(ctx.change_id) .. " "
                    .. tostring(ctx.commit_id) .. " " .. tostring(ctx.is_empty))
            end, {})"#,
        );
        let status = |app: &mut App| {
            runtime
                .engine
                .execute_command(0, app, crate::keymap::CommandFlags::empty());
            runtime.engine.flush_logs(app);
            app.status_message.take().map(|(msg, _)| msg)
        };

        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        assert_eq!(
            status(&mut app).as_deref(),
            Some("dag qpvuntsm 7bbaa2cb false")
        );

        app.views.bookmark_entries.push(BookmarkViewEntry {
            name: BookmarkName::new("main"),
            commit_id: Some(CommitId::new("7bbaa2cb")),
            change_id: None,
            short_commit_id: None,
            revision: Some(RevisionArg::new("qpv")),
            description: None,
            kind: BookmarkKind::Local {
                is_dirty: false,
                is_conflicted: false,
            },
        });
        app.switch_view(ActiveView::Bookmarks);
        assert_eq!(
            status(&mut app).as_deref(),
            Some("bookmarks qpv 7bbaa2cb false")
        );
    }

    /// Run the first registered command and report what it left the config
    /// table as.
    fn run_command(runtime: &LuaRuntime) -> Option<Result<crate::config::Config, String>> {
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
        assert!(run_command(&runtime).expect("a change").is_err());
    }

    fn rewrite(dir: &tempfile::TempDir, init: &str) {
        std::fs::write(dir.path().join("init.lua"), init).expect("write init.lua");
    }

    /// The runtime `dir` holds, and an app pointed at its config.
    fn running(dir: &tempfile::TempDir) -> (LuaRuntime, crate::app::App) {
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        assert_eq!(runtime.init_error(), None);
        let app = crate::app::App::new(String::new(), String::new(), runtime.config.clone());
        (runtime, app)
    }

    fn reload(runtime: &mut LuaRuntime, app: &mut crate::app::App) {
        runtime.reload(app, crate::jj_version::InstalledJj::default);
    }

    fn command_names(runtime: &LuaRuntime) -> Vec<&str> {
        runtime
            .engine
            .commands
            .iter()
            .map(|c| c.name.as_str())
            .collect()
    }

    fn logged(app: &crate::app::App, kind: crate::app::CommandLogKind, text: &str) -> bool {
        app.command_log
            .entries()
            .any(|e| e.kind == kind && String::from_utf8_lossy(&e.output).contains(text))
    }

    fn error_status(app: &crate::app::App) -> Option<&str> {
        match &app.status_message {
            Some((msg, crate::app::StatusLevel::Error)) => Some(msg),
            _ => None,
        }
    }

    /// Registering appends, so the same script loaded over the running
    /// engine would bind everything twice.
    #[test]
    fn reloading_an_unchanged_script_registers_nothing_twice() {
        let dir = config_dir_with(
            r#"
            kojutsu.bind { action = "abandon", key = "ctrl-y" }
            kojutsu.command("c", function() end, { key = "ctrl-l" })
            "#,
        );
        let (mut runtime, mut app) = running(&dir);
        let bindings = binding_count(&runtime);

        reload(&mut runtime, &mut app);

        assert_eq!(binding_count(&runtime), bindings);
        assert_eq!(command_names(&runtime), ["c"]);
    }

    #[test]
    fn a_reload_picks_up_edits_and_drops_what_the_script_no_longer_registers() {
        let dir = config_dir_with(
            r#"
            kojutsu.config.tab_width = 2
            kojutsu.bind { action = "abandon", key = "ctrl-y" }
            kojutsu.command("c", function() end, { key = "ctrl-l" })
            "#,
        );
        let (mut runtime, mut app) = running(&dir);
        rewrite(
            &dir,
            r#"
            kojutsu.config.tab_width = 3
            kojutsu.command("d", function() end, { key = "ctrl-l" })
            "#,
        );

        reload(&mut runtime, &mut app);

        assert_eq!(runtime.init_error(), None);
        assert_eq!(runtime.config.tab_width, 3);
        assert!(std::rc::Rc::ptr_eq(&app.config, &runtime.config));
        assert_eq!(command_names(&runtime), ["d"]);
        let clean = LuaRuntime::load_from(config_dir_with("").path(), std::path::Path::new("."));
        assert_eq!(binding_count(&runtime), binding_count(&clean) + 1);
    }

    /// `require` looks under the config directory only. Lua's own default
    /// path ends in the current directory, usually a repository, where a
    /// module of the right name would run as the user's config.
    #[test]
    fn modules_come_only_from_the_config_directory() {
        let dir = config_dir_with("kojutsu.config.date_format = package.path");
        let (runtime, _) = running(&dir);
        let lua = dir.path().join("lua").display().to_string();
        assert_eq!(
            runtime.config.date_format,
            format!("{lua}/?.lua;{lua}/?/init.lua")
        );
    }

    /// A script gets no `debug` library, and native modules cannot be
    /// loaded, so a config is Lua and nothing more.
    #[test]
    fn a_script_has_no_debug_library_and_loads_no_native_code() {
        let dir = config_dir_with(
            r#"kojutsu.config.date_format = tostring(debug) .. "|" .. tostring(pcall(package.loadlib, "/nonexistent.so", "luaopen_x"))"#,
        );
        let (runtime, _) = running(&dir);
        assert_eq!(runtime.config.date_format, "nil|false");
    }

    /// A fresh Lua has an empty `package.loaded`, so an edited module is
    /// read again rather than served from the cache.
    #[test]
    fn a_reload_reads_required_modules_again() {
        let dir = config_dir_with(r#"kojutsu.config.tab_width = require("width")"#);
        let module = dir.path().join("lua").join("width.lua");
        std::fs::create_dir(module.parent().unwrap()).unwrap();
        std::fs::write(&module, "return 2").unwrap();
        let (mut runtime, mut app) = running(&dir);
        std::fs::write(&module, "return 5").unwrap();

        reload(&mut runtime, &mut app);

        assert_eq!(runtime.config.tab_width, 5);
    }

    /// The reload starts from `init.lua` alone: what a plugin wrote to
    /// `kojutsu.config` since is not carried over.
    #[test]
    fn a_reload_drops_what_a_plugin_wrote_to_the_config() {
        let dir = config_dir_with(
            r#"kojutsu.command("wide", function() kojutsu.config.tab_width = 8 end, {})"#,
        );
        let (mut runtime, mut app) = running(&dir);
        let change = run_command(&runtime).expect("a change").expect("valid");
        runtime.config = std::rc::Rc::new(change);

        reload(&mut runtime, &mut app);

        assert_eq!(*runtime.config, crate::config::Config::default());
    }

    #[test]
    fn a_broken_script_leaves_the_running_config_in_place_and_says_why() {
        let dir = config_dir_with(
            r#"
            kojutsu.config.tab_width = 2
            kojutsu.command("c", function() end, { key = "ctrl-l" })
            "#,
        );
        let (mut runtime, mut app) = running(&dir);
        let config = runtime.config.clone();
        let bindings = binding_count(&runtime);
        rewrite(
            &dir,
            r#"
            kojutsu.config.tab_width = 9
            kojutsu.bind { action = "abandon", key = "ctrl-y" }
            error("boom")
            "#,
        );

        reload(&mut runtime, &mut app);

        assert_eq!(runtime.init_error(), None);
        assert!(std::rc::Rc::ptr_eq(&runtime.config, &config));
        assert!(std::rc::Rc::ptr_eq(&app.config, &config));
        assert_eq!(binding_count(&runtime), bindings);
        assert_eq!(command_names(&runtime), ["c"]);
        assert!(error_status(&app).is_some_and(|msg| msg.contains("boom")));
        assert!(logged(&app, crate::app::CommandLogKind::Warning, "boom"));
    }

    /// The prompt's thread lives in the running engine, so replacing it
    /// would strand the prompt; once answered, the reload goes ahead.
    #[test]
    fn a_reload_is_refused_while_a_plugin_prompt_is_open() {
        let dir = config_dir_with(
            r#"kojutsu.command("ask", function() kojutsu.ui.input("name: ") end, {})"#,
        );
        let (mut runtime, mut app) = running(&dir);
        runtime
            .engine
            .execute_command(0, &mut app, crate::keymap::CommandFlags::empty());
        assert!(runtime.engine.has_suspended_thread());
        rewrite(&dir, "kojutsu.config.tab_width = 3");

        reload(&mut runtime, &mut app);

        assert!(runtime.engine.has_suspended_thread());
        assert_eq!(command_names(&runtime), ["ask"]);
        assert!(error_status(&app).is_some_and(|msg| msg.contains("plugin prompt")));

        runtime
            .engine
            .resume_suspended(&mut app, crate::lua::ResumeValue::None);
        reload(&mut runtime, &mut app);

        assert_eq!(runtime.config.tab_width, 3);
    }

    #[test]
    fn a_reload_adopts_the_jj_found_by_probing_again_and_warns_about_it_once() {
        use crate::jj_version::{FLOOR, InstalledJj, JjVersion};

        let dir = config_dir_with("");
        let (mut runtime, mut app) = running(&dir);
        app.set_installed_jj(InstalledJj::known(FLOOR));
        let downgraded = InstalledJj::known(JjVersion::new(FLOOR.major, FLOOR.minor - 1, 0));
        let concerns = downgraded.concerns();
        assert!(!concerns.is_empty());
        let warnings = |app: &crate::app::App| {
            app.command_log
                .entries()
                .filter(|e| concerns.contains(&e.summary))
                .count()
        };

        runtime.reload(&mut app, || downgraded);
        assert_eq!(app.jj, downgraded);
        assert_eq!(warnings(&app), concerns.len());

        runtime.reload(&mut app, || downgraded);
        assert_eq!(warnings(&app), concerns.len());
    }

    /// The repo service holds its own copy of the diff limit.
    #[test]
    fn a_reload_hands_the_repo_service_the_new_diff_limit() {
        let dir = config_dir_with("");
        let (mut runtime, mut app) = running(&dir);
        rewrite(&dir, "kojutsu.config.diff.max_file_size_mib = 3");
        app.take_repo_requests();

        reload(&mut runtime, &mut app);

        let bytes = runtime.config.diff.max_file_size_bytes();
        assert_ne!(
            bytes,
            crate::config::Config::default().diff.max_file_size_bytes()
        );
        assert!(app.take_repo_requests().iter().any(|request| matches!(
            request,
            crate::repo_service::RepoRequest::SetDiffSizeLimit { bytes: sent } if *sent == bytes
        )));
    }

    #[test]
    fn a_reload_reports_what_the_new_script_registered_wrongly() {
        let dir = config_dir_with("");
        let (mut runtime, mut app) = running(&dir);
        rewrite(
            &dir,
            r#"kojutsu.bind { action = "bookmark_view_edit", key = "ctrl-y" }"#,
        );

        reload(&mut runtime, &mut app);

        let warning = &runtime.init_warnings()[0];
        assert!(logged(&app, crate::app::CommandLogKind::Warning, warning));
    }

    /// What Lua logged and has yet to be shown is held by the engine the
    /// reload drops, so it must be shown first.
    #[test]
    fn what_a_hook_logged_before_a_reload_is_still_shown() {
        let dir = config_dir_with(
            r#"kojutsu.hook("reload_config", "pre", function() kojutsu.log("saving state") end)"#,
        );
        let mut runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        let mut app = crate::app::App::for_test();
        assert!(matches!(
            runtime.engine.run_pre_hooks(
                crate::keymap::AppAction::ReloadConfig,
                crate::keymap::CommandFlags::empty(),
                &mut app,
            ),
            crate::lua::HookOutcome::Proceed
        ));

        runtime.reload(&mut app, crate::jj_version::InstalledJj::default);
        runtime.engine.flush_logs(&mut app);

        assert!(
            app.command_log
                .entries()
                .any(|e| String::from_utf8_lossy(&e.output).contains("saving state"))
        );
    }

    /// No init.lua at all is the common case, not an error.
    #[test]
    fn an_empty_config_directory_loads_clean() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = LuaRuntime::load_from(dir.path(), std::path::Path::new("."));
        assert_eq!(runtime.init_error(), None);
        assert_eq!(*runtime.config, crate::config::Config::default());
    }
}
