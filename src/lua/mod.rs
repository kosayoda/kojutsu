mod api;
mod config;
pub(crate) mod helpers;
mod init_script;
mod runtime;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use compact_str::CompactString;
use mlua::Lua;

use crate::app::App;
use crate::input::Action;
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{ActionRegistry, AppAction, BindingSpec, CommandFlags};

pub use helpers::generate_type_definitions;
pub use runtime::LuaRuntime;

/// A starter `init.lua`, printed by `--print-default-config`. Every value in
/// it is a built-in default, which
/// [`the_shipped_sample_is_exactly_the_defaults`](config::tests) checks.
pub const DEFAULT_INIT: &str = include_str!("default-init.lua");

struct LuaCommand {
    name: CompactString,
    source: String,
    callback: mlua::RegistryKey,
}

struct LuaHook {
    phase: HookPhase,
    callback: mlua::RegistryKey,
    source: String,
    /// Action names this hook's pattern matched, evaluated once at
    /// registration against the closed set of action names: dispatch is a
    /// set lookup instead of a Lua pattern match per keypress.
    actions: std::collections::HashSet<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookPhase {
    Pre,
    Post,
}

pub enum HookOutcome {
    Proceed,
    Cancel,
    /// A hook yielded; the carried action (e.g. starting a jj command for
    /// the suspended thread) must be performed by the caller.
    Suspended(Action),
}

pub enum ResumeResult {
    Action(Action),
    DispatchAction {
        action: AppAction,
        flags: CommandFlags,
    },
}

/// Outcome of a completed command, passed to post-hooks.
pub struct CommandOutcome<'a> {
    pub success: bool,
    pub cancelled: bool,
    pub code: Option<i32>,
    pub output: &'a [u8],
}

/// The value a suspended Lua thread is resumed with.
pub enum ResumeValue {
    /// Prompt dismissed without input.
    None,
    /// Text input / single-select list selection.
    Text(String),
    /// Multi-select list selection; becomes a Lua array table.
    List(Vec<String>),
    /// A yielded jj command finished (or was cancelled).
    JjResult(Box<crate::jj_command::JJCommandResult>),
}

/// A Lua array of strings, as an argument vector.
fn string_list(table: &mlua::Table) -> mlua::Result<Vec<crate::types::Str>> {
    (1..=table.raw_len())
        .map(|i| table.raw_get::<String>(i).map(Into::into))
        .collect()
}

/// Build the command `kojutsu.exec` asked for. The first element is the program
/// and the rest are handed to it verbatim: there is no shell, so quoting,
/// globbing and redirection are the caller's business, not ours.
fn exec_command(argv: &mlua::Table) -> mlua::Result<JJCommand> {
    let mut argv = string_list(argv)?.into_iter();
    let program = argv
        .next()
        .ok_or_else(|| mlua::Error::external("kojutsu.exec: needs a program to run"))?;
    Ok(JJCommand {
        kind: JJCommandKind::Exec {
            program,
            args: argv.collect(),
        },
        flags: CommandFlags::empty(),
    })
}

/// How a finished command is described to Lua:
/// `{status = "ok"|"failed"|"cancelled", ok, output, code?}`.
fn outcome_table(
    lua: &Lua,
    success: bool,
    cancelled: bool,
    code: Option<i32>,
    output: &[u8],
) -> mlua::Result<mlua::Table> {
    let status = if cancelled {
        "cancelled"
    } else if success {
        "ok"
    } else {
        "failed"
    };
    let table = lua.create_table()?;
    table.set("status", status)?;
    table.set("ok", status == "ok")?;
    table.set("output", String::from_utf8_lossy(output).into_owned())?;
    if let Some(code) = code {
        table.set("code", code)?;
    }
    Ok(table)
}

/// What `kojutsu.jj` hands back: the outcome plus the two pipes on their own.
/// `output` interleaves them for display, which is the wrong thing to parse:
/// jj writes warnings and hints to stderr, so a plugin reading an object name
/// out of `output` can pick up the "a" from "Warning" instead. Only a caller
/// holding the command's own result can offer this; post-hooks read their bytes
/// back from the finished command's output, where the split is long gone.
fn jj_result_table(
    lua: &Lua,
    result: &crate::jj_command::JJCommandResult,
) -> mlua::Result<mlua::Table> {
    use crate::jj_command::Stream;

    let table = outcome_table(
        lua,
        result.success,
        result.cancelled,
        result.code,
        result.output.bytes(),
    )?;
    for (key, stream) in [("stdout", Stream::Stdout), ("stderr", Stream::Stderr)] {
        let bytes = result.output.stream(stream);
        table.set(key, String::from_utf8_lossy(&bytes).into_owned())?;
    }
    Ok(table)
}

/// Convert the value a suspended thread is resumed with into the Lua value the
/// yield site receives. A conversion failure degrades to `nil`, which every
/// yield site already treats as "dismissed".
fn resume_value_to_lua(lua: &Lua, value: ResumeValue) -> mlua::Value {
    let converted = match value {
        ResumeValue::None => Ok(mlua::Value::Nil),
        ResumeValue::Text(s) => lua.create_string(&s).map(mlua::Value::String),
        // An array table even when empty: unlike a dismissed prompt (the
        // thread is dropped, never resumed), "confirmed with nothing ticked"
        // is a real answer.
        ResumeValue::List(items) => (|| {
            let table = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                table.raw_set(i + 1, item.as_str())?;
            }
            Ok(mlua::Value::Table(table))
        })(),
        ResumeValue::JjResult(result) => jj_result_table(lua, &result).map(mlua::Value::Table),
    };
    converted.unwrap_or(mlua::Value::Nil)
}

#[cfg(test)]
mod resume_value_tests {
    use super::{Lua, ResumeValue, resume_value_to_lua};

    #[test]
    fn multi_select_resumes_with_an_array_table() {
        let lua = Lua::new();
        let value = resume_value_to_lua(
            &lua,
            ResumeValue::List(vec!["feature-a".into(), "feature-b".into()]),
        );
        let mlua::Value::Table(table) = value else {
            panic!("expected a table");
        };
        assert_eq!(table.raw_len(), 2);
        assert_eq!(table.raw_get::<String>(1).unwrap(), "feature-a");
        assert_eq!(table.raw_get::<String>(2).unwrap(), "feature-b");
    }

    /// Ticking nothing and confirming is distinct from dismissing the prompt,
    /// which drops the thread instead of resuming it.
    #[test]
    fn an_empty_multi_select_is_an_empty_table_not_nil() {
        let lua = Lua::new();
        let value = resume_value_to_lua(&lua, ResumeValue::List(Vec::new()));
        let mlua::Value::Table(table) = value else {
            panic!("expected a table");
        };
        assert_eq!(table.raw_len(), 0);
    }

    #[test]
    fn single_select_resumes_with_a_bare_string() {
        let lua = Lua::new();
        let value = resume_value_to_lua(&lua, ResumeValue::Text("feature-a".into()));
        let mlua::Value::String(s) = value else {
            panic!("expected a string");
        };
        assert_eq!(s.to_str().unwrap(), "feature-a");
    }

    #[test]
    fn a_dismissed_prompt_resumes_with_nil() {
        let lua = Lua::new();
        assert!(resume_value_to_lua(&lua, ResumeValue::None).is_nil());
    }
}

/// How one run of a Lua thread ended.
enum Step {
    /// It yielded a request and is parked until that is answered; the
    /// action is what the caller must do to answer it.
    Parked(Action),
    /// It returned `value`, and `requested` is what its queued requests
    /// asked of the main loop.
    Returned {
        value: mlua::Value,
        requested: Action,
        kind: SuspendedKind,
    },
    /// It raised an error, already reported.
    Failed,
}

/// Record that a pre-hook cancelled `action`.
fn log_cancelled(app: &mut App, action: &str) {
    app.push_command_log(
        crate::app::CommandLogKind::Warning,
        format!("plugin: pre-hook cancelled - {action}"),
        None,
        Vec::new(),
        false,
    );
}

enum SuspendedKind {
    Command(CommandFlags),
    PreHooks {
        action: AppAction,
        flags: CommandFlags,
        header: String,
    },
    PostHooks {
        header: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogPhase {
    Pre,
    Post,
    Command,
}

struct LogGroup {
    header: String,
    phase: LogPhase,
    messages: Vec<String>,
}

/// Shared mutable state accessed by Lua closures via `lua.app_data_ref()`.
/// Stored as `RefCell<LuaState>` in the Lua instance's app data, eliminating
/// the need for `Rc<RefCell<>>` on each field.
#[derive(Default)]
struct LuaState {
    /// Nav/dispatch requests queued by the running Lua code, applied in call
    /// order when it completes.
    pending_actions: Vec<PendingAction>,
    pending_logs: Vec<String>,
    pending_status: Option<(String, crate::app::StatusLevel)>,
    log_groups: Vec<LogGroup>,
}

impl LuaState {
    /// File the messages logged since the last collection under `header`.
    fn collect_logs(&mut self, header: String, phase: LogPhase) {
        let messages: Vec<String> = self.pending_logs.drain(..).collect();
        if !messages.is_empty() {
            self.log_groups.push(LogGroup {
                header,
                phase,
                messages,
            });
        }
    }
}

macro_rules! lua_state {
    ($lua:expr) => {
        $lua.app_data_ref::<RefCell<LuaState>>().unwrap()
    };
}
use lua_state;

enum PendingAction {
    Refresh,
    SetRevset(String),
    Interactive(Vec<String>),
    SwitchView(crate::types::ActiveView),
    JumpTo(crate::types::RevisionArg),
    Dispatch(AppAction),
}

pub struct LuaEngine {
    lua: Lua,
    commands: Vec<LuaCommand>,
    hooks: Vec<LuaHook>,
    extra_bindings: Vec<BindingSpec>,
    suspended_thread: RefCell<Option<(mlua::RegistryKey, SuspendedKind)>>,
    current_header: RefCell<String>,
    /// Set whenever user Lua runs. Writes to `kojutsu.config` can't be
    /// observed directly: Lua only notifies on absent keys, and every config
    /// key is present, so the table is re-read by polling. This keeps that
    /// off the keystroke path and on the far rarer "a plugin just ran" path.
    lua_ran: Cell<bool>,
    pub init_error: Option<String>,
    /// Problems with what `init.lua` registered, each naming where it was
    /// registered. The registrations concerned were skipped.
    pub init_warnings: Vec<String>,
    repo_path: PathBuf,
}

impl LuaEngine {
    /// Construct via [`LuaRuntime::load`]: the engine, the action registry
    /// and the keymaps are only correct when built together.
    ///
    /// `config` arrives holding the values read from disk and leaves holding
    /// whatever `init.lua` made of them.
    pub(crate) fn new(
        repo_path: &Path,
        registry: &mut ActionRegistry,
        default_specs: &[BindingSpec],
        config: &mut crate::theme::Config,
        config_dir: &Path,
    ) -> Self {
        let mut engine = Self::without_config(repo_path);
        if engine.init_error.is_none() {
            engine.load_init_script(registry, default_specs, config, config_dir);
        }
        engine
    }

    /// An engine with the API registered but no `init.lua` loaded. Tests use
    /// this so they don't depend on whatever config the machine happens to
    /// have.
    fn without_config(repo_path: &Path) -> Self {
        let lua = unsafe {
            Lua::unsafe_new_with(
                mlua::StdLib::ALL_SAFE | mlua::StdLib::DEBUG,
                mlua::LuaOptions::default(),
            )
        };
        lua.set_app_data(RefCell::new(LuaState::default()));
        let mut engine = LuaEngine {
            lua,
            commands: Vec::new(),
            hooks: Vec::new(),
            extra_bindings: Vec::new(),
            suspended_thread: RefCell::new(None),
            current_header: RefCell::new(String::new()),
            lua_ran: Cell::new(false),
            init_error: None,
            init_warnings: Vec::new(),
            repo_path: repo_path.to_path_buf(),
        };

        if let Err(e) = engine.register_persistent_functions() {
            engine.init_error = Some(format!("failed to register Lua API: {e}"));
        }
        engine
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::without_config(Path::new("."))
    }

    pub(crate) fn take_extra_bindings(&mut self) -> Vec<BindingSpec> {
        std::mem::take(&mut self.extra_bindings)
    }

    pub fn has_suspended_thread(&self) -> bool {
        self.suspended_thread.borrow().is_some()
    }

    /// Run the pre-hooks matching `action`. They can cancel it by returning
    /// `false`, or yield (to prompt or run a command), in which case the
    /// action is dispatched when the parked thread finishes.
    pub fn run_pre_hooks(
        &self,
        action: AppAction,
        flags: CommandFlags,
        app: &mut App,
    ) -> HookOutcome {
        let header = action.id_name().to_string();
        let kind = SuspendedKind::PreHooks {
            action,
            flags,
            header,
        };
        match self.run_hooks(action, HookPhase::Pre, kind, app, None) {
            None => HookOutcome::Proceed,
            Some(Step::Parked(yielded)) => HookOutcome::Suspended(yielded),
            Some(Step::Returned {
                value: mlua::Value::Boolean(false),
                ..
            }) => {
                log_cancelled(app, action.id_name());
                HookOutcome::Cancel
            }
            Some(Step::Returned { .. } | Step::Failed) => HookOutcome::Proceed,
        }
    }

    /// Run the post-hooks matching `action`, handing them how its command
    /// went.
    pub fn run_post_hooks(
        &self,
        action: AppAction,
        app: &mut App,
        outcome: CommandOutcome<'_>,
    ) -> HookOutcome {
        let kind = SuspendedKind::PostHooks {
            header: action.id_name().to_string(),
        };
        let result = match outcome_table(
            &self.lua,
            outcome.success,
            outcome.cancelled,
            outcome.code,
            outcome.output,
        ) {
            Ok(t) => t,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return HookOutcome::Proceed;
            }
        };
        match self.run_hooks(action, HookPhase::Post, kind, app, Some(result)) {
            Some(Step::Parked(yielded)) => HookOutcome::Suspended(yielded),
            None | Some(Step::Returned { .. } | Step::Failed) => HookOutcome::Proceed,
        }
    }

    /// Run the hooks of `phase` matching `action` in one thread, or `None`
    /// if there are none.
    fn run_hooks(
        &self,
        action: AppAction,
        phase: HookPhase,
        kind: SuspendedKind,
        app: &mut App,
        result: Option<mlua::Table>,
    ) -> Option<Step> {
        let hooks = self.collect_hook_functions(action.id_name(), phase)?;
        self.lua_ran.set(true);
        let thread = (|| -> mlua::Result<(mlua::Thread, mlua::Table)> {
            let kojutsu: mlua::Table = self.lua.globals().get("kojutsu")?;
            let runner = kojutsu.get::<mlua::Function>("_run_hooks")?;
            Ok((self.lua.create_thread(runner)?, self.build_ctx_table(app)?))
        })();
        let (thread, ctx) = match thread {
            Ok(pair) => pair,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return Some(Step::Failed);
            }
        };
        let phase_name = match phase {
            HookPhase::Pre => "pre",
            HookPhase::Post => "post",
        };
        Some(self.step(thread, (hooks, phase_name, ctx, result), kind, app))
    }

    /// The name a command was registered under, for error messages.
    pub fn command_name(&self, id: u16) -> &str {
        &self.commands[id as usize].name
    }

    pub fn execute_command(&self, id: u16, app: &mut App, flags: CommandFlags) -> Action {
        let cmd = &self.commands[id as usize];
        app.push_command_log(
            crate::app::CommandLogKind::Command,
            format!("plugin: {}", cmd.name),
            None,
            Vec::new(),
            true,
        );

        self.prepare_execution();
        *self.current_header.borrow_mut() = cmd.source.clone();
        let thread = self
            .lua
            .registry_value::<mlua::Function>(&cmd.callback)
            .and_then(|func| self.lua.create_thread(func));
        let thread = match thread {
            Ok(t) => t,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return Action::None;
            }
        };
        let ctx = self
            .build_ctx_table(app)
            .map(mlua::Value::Table)
            .unwrap_or(mlua::Value::Nil);
        match self.step(thread, ctx, SuspendedKind::Command(flags), app) {
            Step::Parked(yielded) => yielded,
            Step::Returned { requested, .. } => requested,
            Step::Failed => Action::None,
        }
    }

    pub fn resume_suspended(&self, app: &mut App, value: ResumeValue) -> ResumeResult {
        let Some((thread_key, kind)) = self.suspended_thread.borrow_mut().take() else {
            return ResumeResult::Action(Action::None);
        };
        self.lua_ran.set(true);
        let thread: mlua::Result<mlua::Thread> = self.lua.registry_value(&thread_key);
        let _ = self.lua.remove_registry_value(thread_key);
        let thread = match thread {
            Ok(t) => t,
            Err(e) => {
                app.set_error(format!("plugin resume: {e}"));
                return ResumeResult::Action(Action::None);
            }
        };
        {
            let cell = lua_state!(self.lua);
            let mut state = cell.borrow_mut();
            state.pending_actions.clear();
            state.pending_logs.clear();
        }

        let arg = resume_value_to_lua(&self.lua, value);
        match self.step(thread, arg, kind, app) {
            Step::Parked(yielded) => ResumeResult::Action(yielded),
            Step::Returned {
                value,
                kind: SuspendedKind::PreHooks { action, flags, .. },
                ..
            } => {
                if matches!(value, mlua::Value::Boolean(false)) {
                    log_cancelled(app, action.id_name());
                    ResumeResult::Action(Action::None)
                } else {
                    ResumeResult::DispatchAction { action, flags }
                }
            }
            Step::Returned { requested, .. } => ResumeResult::Action(requested),
            Step::Failed => ResumeResult::Action(Action::None),
        }
    }

    pub fn cancel_suspended_thread(&self) {
        if let Some((key, _)) = self.suspended_thread.borrow_mut().take() {
            let _ = self.lua.remove_registry_value(key);
        }
    }

    /// Resume `thread` with `args` and settle how it ended. A yield parks it
    /// until its request is answered; a return collects its logs and applies
    /// the requests it queued; an error is reported. `kind` says whose
    /// thread it is, which decides all three.
    fn step(
        &self,
        thread: mlua::Thread,
        args: impl mlua::IntoLuaMulti,
        kind: SuspendedKind,
        app: &mut App,
    ) -> Step {
        let resumed = thread.resume::<mlua::Value>(args);
        let (header, phase) = match &kind {
            SuspendedKind::Command(_) => (self.current_header.borrow().clone(), LogPhase::Command),
            SuspendedKind::PreHooks { header, .. } => (header.clone(), LogPhase::Pre),
            SuspendedKind::PostHooks { header } => (header.clone(), LogPhase::Post),
        };
        self.collect_logs(header, phase);
        match resumed {
            Ok(value) if thread.status() == mlua::ThreadStatus::Resumable => {
                Step::Parked(self.handle_yield(thread, value, app, kind))
            }
            Ok(value) => {
                // Only a command may ask the main loop for more (a dispatch,
                // an interactive jj): a hook doing so would recurse.
                let requested = match kind {
                    SuspendedKind::Command(flags) => self.drain_pending_actions(app, flags, true),
                    SuspendedKind::PreHooks { flags, .. } => {
                        self.drain_pending_actions(app, flags, false)
                    }
                    SuspendedKind::PostHooks { .. } => {
                        self.drain_pending_actions(app, CommandFlags::empty(), false)
                    }
                };
                Step::Returned {
                    value,
                    requested,
                    kind,
                }
            }
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                app.push_command_log(
                    crate::app::CommandLogKind::Warning,
                    "plugin: runtime error",
                    None,
                    e.to_string().into_bytes(),
                    false,
                );
                Step::Failed
            }
        }
    }

    fn collect_hook_functions(&self, action_name: &str, phase: HookPhase) -> Option<mlua::Table> {
        let matching: Vec<&LuaHook> = self
            .hooks
            .iter()
            .filter(|hook| hook.phase == phase && hook.actions.contains(action_name))
            .collect();
        if matching.is_empty() {
            return None;
        }
        let table = self.lua.create_table().ok()?;
        for (i, hook) in matching.iter().enumerate() {
            let func: mlua::Function = self.lua.registry_value(&hook.callback).ok()?;
            let entry = self.lua.create_table().ok()?;
            entry.set("fn", func).ok()?;
            entry.set("source", hook.source.as_str()).ok()?;
            table.raw_set(i + 1, entry).ok()?;
        }
        Some(table)
    }

    /// Park a yielded thread and set up whatever its request needs. Returns
    /// the action the caller must perform (e.g. starting a jj command);
    /// `Action::None` for requests handled entirely via `app` state.
    fn handle_yield(
        &self,
        thread: mlua::Thread,
        value: mlua::Value,
        app: &mut App,
        kind: SuspendedKind,
    ) -> Action {
        let table = match value {
            mlua::Value::Table(t) => t,
            _ => {
                app.set_error("plugin: invalid yield (expected table)");
                return Action::None;
            }
        };
        let request_type: String = table.get("type").unwrap_or_default();

        // The mode to enter for prompt-style requests, or the action the
        // caller must perform for command-style requests.
        let (mode, action) = match request_type.as_str() {
            "input" => {
                let prompt: String = table.get("prompt").unwrap_or_default();
                let default: String = table.get("default").unwrap_or_default();
                (
                    Some(crate::app::AppMode::text_input(
                        &prompt,
                        &default,
                        crate::types::PromptStep::LuaResume,
                    )),
                    Action::None,
                )
            }
            "choose" => {
                let items: Vec<String> = table
                    .get::<mlua::Table>("items")
                    .map(|t| helpers::table_to_string_vec(&t))
                    .unwrap_or_default();
                if items.is_empty() {
                    app.set_error("plugin: choose requires non-empty items");
                    return Action::None;
                }
                let title: String = table.get("title").unwrap_or_default();
                let multi: bool = table.get("multi").unwrap_or(false);
                (
                    Some(crate::app::AppMode::select_from_list(
                        title,
                        items,
                        multi,
                        crate::types::PendingSelection::LuaResume { multi },
                        false,
                    )),
                    Action::None,
                )
            }
            // Both run through the normal pipeline (running overlay, live
            // output, Esc/^C cancellation); completion resumes this thread with
            // the result table.
            "jj" | "exec" => {
                let Ok(args) = table.get::<mlua::Table>("args") else {
                    app.set_error("plugin: a command request carries no arguments");
                    return Action::None;
                };
                let cmd = if request_type == "exec" {
                    match exec_command(&args) {
                        Ok(cmd) => cmd,
                        Err(e) => {
                            app.set_error(format!("plugin: {e}"));
                            return Action::None;
                        }
                    }
                } else {
                    let args = match string_list(&args) {
                        Ok(args) => args,
                        Err(e) => {
                            app.set_error(format!("plugin: jj arguments must be strings: {e}"));
                            return Action::None;
                        }
                    };
                    if args.is_empty() {
                        app.set_error("plugin: jj requires at least one argument");
                        return Action::None;
                    }
                    // Only a jj command line can carry jj's global flags.
                    let flags = match &kind {
                        SuspendedKind::Command(flags) => *flags,
                        SuspendedKind::PreHooks { flags, .. } => *flags,
                        SuspendedKind::PostHooks { .. } => CommandFlags::empty(),
                    };
                    JJCommand {
                        kind: JJCommandKind::Raw { args },
                        flags,
                    }
                };
                (
                    None,
                    Action::RunJj {
                        cmd,
                        completion: crate::input::Completion::ResumeLua,
                    },
                )
            }
            other => {
                app.set_error(format!("plugin: unknown yield type `{other}`"));
                return Action::None;
            }
        };

        let key = match self.lua.create_registry_value(thread) {
            Ok(k) => k,
            Err(e) => {
                app.set_error(format!("plugin: failed to store thread: {e}"));
                return Action::None;
            }
        };
        *self.suspended_thread.borrow_mut() = Some((key, kind));
        if let Some(mode) = mode {
            app.mode = mode;
        }
        action
    }

    /// Apply queued nav/dispatch requests in call order. Nav requests apply
    /// to the app directly; at most one action that must round-trip through
    /// the main loop (`jj_interactive`, `dispatch`) is returned: extras are
    /// dropped with a warning. From hooks (`allow_breaking = false`) those
    /// requests are unsupported and warn.
    fn drain_pending_actions(
        &self,
        app: &mut App,
        flags: CommandFlags,
        allow_breaking: bool,
    ) -> Action {
        let pending: Vec<PendingAction> =
            std::mem::take(&mut lua_state!(self.lua).borrow_mut().pending_actions);
        let mut breaking = Action::None;
        for action in pending {
            match action {
                PendingAction::Refresh => {
                    // The reload path only; a plugin wanting a working-copy
                    // re-scan can dispatch the builtin `refresh` action.
                    app.refresh(crate::repo_service::RevsetLoadKind::NoSnapshot);
                }
                PendingAction::SetRevset(revset) => {
                    app.request_revset_load(
                        Some(revset),
                        crate::repo_service::RevsetLoadKind::NoSnapshot,
                    );
                }
                PendingAction::SwitchView(view) => {
                    app.switch_view(view);
                }
                PendingAction::JumpTo(change_id) => {
                    if let Some(commit_id) = app.commit_id_for_change(&change_id)
                        && let Some(idx) = app.entry_by_commit_id(&commit_id)
                        && let Some(row) = app.row_of_commit(idx)
                    {
                        app.set_cursor(row);
                    }
                }
                PendingAction::Interactive(args) => {
                    let cmd = JJCommand {
                        kind: JJCommandKind::Raw {
                            args: args.into_iter().map(Into::into).collect(),
                        },
                        flags,
                    };
                    self.set_breaking(app, &mut breaking, Action::run(cmd), allow_breaking);
                }
                // The dispatched action skips its own pre-hooks (same as a
                // pre-hook resumption) so hooks can't recurse into themselves.
                PendingAction::Dispatch(action) => {
                    self.set_breaking(
                        app,
                        &mut breaking,
                        Action::DeferredDispatch { action, flags },
                        allow_breaking,
                    );
                }
            }
        }
        breaking
    }

    fn set_breaking(&self, app: &mut App, slot: &mut Action, action: Action, allow: bool) {
        if !allow {
            app.set_error("plugin: dispatch/jj_interactive are not supported from hooks");
            return;
        }
        if matches!(slot, Action::None) {
            *slot = action;
        } else {
            app.set_error("plugin: only one dispatch/jj_interactive per command; extra ignored");
        }
    }

    fn prepare_execution(&self) {
        self.lua_ran.set(true);
        let cell = lua_state!(self.lua);
        let mut state = cell.borrow_mut();
        state.pending_actions.clear();
        state.pending_logs.clear();
        state.log_groups.clear();
    }

    fn collect_logs(&self, header: String, phase: LogPhase) {
        lua_state!(self.lua)
            .borrow_mut()
            .collect_logs(header, phase);
    }

    /// What `kojutsu.config` holds now, if user Lua has run since the last
    /// call and left it different from `current`. `Some(Err)` means the table
    /// it left cannot be read at all.
    pub fn take_config_change(
        &self,
        current: &crate::theme::Config,
    ) -> Option<Result<crate::theme::Config, String>> {
        if !self.lua_ran.replace(false) {
            return None;
        }
        match config::read(&self.lua) {
            Ok(next) if next != *current => Some(Ok(next)),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        }
    }

    pub fn flush_logs(&self, app: &mut App) {
        let (status_msg, mut groups, stray) = {
            let cell = lua_state!(self.lua);
            let mut state = cell.borrow_mut();
            let status_msg = state.pending_status.take();
            let groups: Vec<LogGroup> = state.log_groups.drain(..).collect();
            let stray: Vec<String> = state.pending_logs.drain(..).collect();
            (status_msg, groups, stray)
        };
        if let Some((msg, level)) = status_msg {
            match level {
                crate::app::StatusLevel::Info => app.set_status(msg),
                crate::app::StatusLevel::Error => app.set_error(msg),
            }
        }
        if !stray.is_empty() {
            groups.push(LogGroup {
                header: String::new(),
                phase: LogPhase::Command,
                messages: stray,
            });
        }
        if groups.is_empty() {
            return;
        }

        fn render_messages(group: &LogGroup, out: &mut Vec<u8>) {
            for msg in &group.messages {
                out.extend_from_slice(msg.as_bytes());
                out.push(b'\n');
            }
        }

        fn render_group(group: &LogGroup, out: &mut Vec<u8>) {
            if !group.header.is_empty() {
                out.extend_from_slice(format!("── {} ──\n", group.header).as_bytes());
            }
            render_messages(group, out);
        }

        match &mut app.mode {
            crate::app::AppMode::CommandOutput(state) => {
                let pre: Vec<&LogGroup> =
                    groups.iter().filter(|g| g.phase == LogPhase::Pre).collect();
                if !pre.is_empty() {
                    let mut pre_output = Vec::new();
                    for g in &pre {
                        render_group(g, &mut pre_output);
                    }
                    pre_output.push(b'\n');
                    pre_output.extend_from_slice(&state.output);
                    state.output = pre_output;
                }
                let post: Vec<&LogGroup> =
                    groups.iter().filter(|g| g.phase != LogPhase::Pre).collect();
                if !post.is_empty() {
                    state.output.push(b'\n');
                    for g in &post {
                        render_group(g, &mut state.output);
                    }
                }
                state.reparse();
            }
            _ => {
                let mut output = Vec::new();
                for g in &groups {
                    render_group(g, &mut output);
                }
                app.mode =
                    crate::app::AppMode::command_output(String::new(), None, output, true, vec![]);
            }
        }

        // One entry per group, not per message: a plugin printing a thirty-line
        // report should leave one row behind with the report folded under it,
        // the way a jj command leaves one row carrying its output. The lines
        // belong in `output` rather than the summary for the same reason: a
        // summary is drawn verbatim, so a plugin's colour would arrive as
        // literal escape codes, while output is ANSI-parsed.
        for group in groups {
            let mut output = Vec::new();
            render_messages(&group, &mut output);
            let summary = if group.header.is_empty() {
                "plugin output".to_string()
            } else {
                group.header
            };
            app.push_command_log(
                crate::app::CommandLogKind::Background,
                summary,
                None,
                output,
                true,
            );
        }
    }

    /// What a plugin is told about the cursor: the commit under it in
    /// whichever view, described from the DAG when the DAG shows it.
    fn build_ctx_table(&self, app: &App) -> mlua::Result<mlua::Table> {
        let ctx = self.lua.create_table()?;
        let string_list = |items: &mut dyn Iterator<Item = &str>| -> mlua::Result<mlua::Table> {
            let table = self.lua.create_table()?;
            for (i, item) in items.enumerate() {
                table.raw_set(i + 1, item)?;
            }
            Ok(table)
        };
        let entry_idx = app.target_entry_idx();
        let node = entry_idx.map(|idx| &app.dag.nodes[idx]);
        let commit = node.map(|n| &n.commit);

        if let Some(change_id) = app.cursor_revision() {
            ctx.set("change_id", change_id.as_str())?;
        }
        let commit_id = commit
            .map(|c| c.graph_id.clone())
            .or_else(|| app.target_commit().map(|(id, _)| id));
        if let Some(commit_id) = &commit_id {
            ctx.set("commit_id", commit_id.as_str())?;
        }
        let change_ids = app.target_revisions();
        ctx.set(
            "change_ids",
            string_list(&mut change_ids.iter().map(|id| id.as_str()))?,
        )?;
        if let Some(path) = app.selected_file_path() {
            ctx.set("file_path", path.as_str())?;
        }
        if let Some(commit) = commit {
            if let Some(desc) = &commit.description {
                ctx.set("description", desc.as_str())?;
            }
            ctx.set(
                "bookmarks",
                string_list(&mut commit.bookmarks.iter().map(|b| b.name.as_str()))?,
            )?;
            ctx.set(
                "tags",
                string_list(&mut commit.tags.iter().map(|t| t.as_str()))?,
            )?;
            ctx.set("author_name", commit.author.name.as_str())?;
            ctx.set("author_email", commit.author.email.as_str())?;
            ctx.set("is_immutable", commit.is_immutable)?;
            ctx.set("is_merge", commit.is_merge)?;
        }
        if let Some(node) = node {
            let parents: Vec<crate::types::ChangeId> = node
                .parents
                .iter()
                .map(|&idx| app.dag.nodes[idx].commit.unique_change_id())
                .collect();
            ctx.set(
                "parent_change_ids",
                string_list(&mut parents.iter().map(|id| id.as_str()))?,
            )?;
        }
        ctx.set(
            "is_working_copy",
            commit.is_some_and(|c| c.is_working_copy()),
        )?;
        ctx.set("is_empty", commit.is_some_and(|c| c.is_empty))?;
        ctx.set("has_conflict", commit.is_some_and(|c| c.has_conflict))?;
        ctx.set("view", app.active_view.to_string())?;
        ctx.set("revset", app.revset.current.as_str())?;
        ctx.set("repo_root", app.repo_root.as_str())?;
        Ok(ctx)
    }
}

/// What a plugin's `kojutsu.log` calls leave behind in the command log.
#[cfg(test)]
mod command_log_tests {
    use super::{App, LuaEngine};

    /// Log `messages` from a plugin under `header`, flush, and hand back the
    /// command log rows that produced.
    fn logged(header: &str, messages: &[&str]) -> Vec<(String, String)> {
        let engine = LuaEngine::for_test();
        let calls: String = messages
            .iter()
            .map(|m| format!("kojutsu.log({m})\n"))
            .collect();
        engine
            .lua
            .load(format!(
                "{calls}kojutsu._collect_logs('{header}', 'command')"
            ))
            .exec()
            .expect("log and collect");

        let mut app = App::new(
            String::new(),
            String::new(),
            std::rc::Rc::new(crate::theme::Config::default()),
        );
        engine.flush_logs(&mut app);
        app.command_log
            .entries
            .iter()
            .map(|e| {
                (
                    e.summary.clone(),
                    String::from_utf8_lossy(&e.output).into_owned(),
                )
            })
            .collect()
    }

    /// A plugin that prints a report wants one row in the command log carrying
    /// it, the way a jj command leaves one row carrying its output, not one
    /// row per line of the report. And the report has to land in the entry's
    /// output, which is ANSI-parsed; a summary is drawn verbatim, so colour put
    /// there would reach the user as escape codes.
    #[test]
    fn a_plugins_log_lines_become_one_command_log_entry() {
        let rows = logged("lua/plugin.lua:12", &["'\\27[32mfirst\\27[0m'", "'second'"]);
        assert_eq!(
            rows,
            vec![(
                "lua/plugin.lua:12".to_string(),
                "\u{1b}[32mfirst\u{1b}[0m\nsecond\n".to_string()
            )]
        );
    }

    /// Messages logged outside any collected phase still need a summary.
    #[test]
    fn uncollected_log_lines_get_a_summary() {
        let engine = LuaEngine::for_test();
        engine.lua.load("kojutsu.log('stray')").exec().expect("log");
        let mut app = App::new(
            String::new(),
            String::new(),
            std::rc::Rc::new(crate::theme::Config::default()),
        );
        engine.flush_logs(&mut app);
        let entry = app.command_log.entries.first().expect("one entry");
        assert_eq!(entry.summary, "plugin output");
        assert_eq!(String::from_utf8_lossy(&entry.output), "stray\n");
    }
}

#[cfg(test)]
mod api_surface_tests {
    use super::LuaEngine;

    /// Function names on a table, ignoring `_`-prefixed internals.
    fn functions(engine: &LuaEngine, path: &[&str]) -> Vec<String> {
        let mut table: mlua::Table = engine.lua.globals().get("kojutsu").expect("kojutsu global");
        for step in path {
            table = table
                .get(*step)
                .unwrap_or_else(|_| panic!("kojutsu.{step}"));
        }
        let mut names: Vec<String> = table
            .pairs::<String, mlua::Value>()
            .filter_map(|pair| {
                let (name, value) = pair.ok()?;
                (value.is_function() && !name.starts_with('_')).then_some(name)
            })
            .collect();
        names.sort();
        names
    }

    /// The `---@field` names declared for one generated class.
    fn declared(class: &str) -> Vec<String> {
        let defs = crate::lua::generate_type_definitions();
        let start = defs
            .find(&format!("---@class {class}\n"))
            .unwrap_or_else(|| panic!("no class {class}"));
        let block = &defs[start..];
        let end = block.find("\n\n").unwrap_or(block.len());
        let mut names: Vec<String> = block[..end]
            .lines()
            .filter_map(|l| l.strip_prefix("---@field "))
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect();
        names.sort();
        names
    }

    /// The type definitions are what plugin authors code against, and they're
    /// written by hand next to the registrations. A function present in one
    /// and not the other is either invisible or a lie.
    #[test]
    fn ui_and_nav_are_declared_exactly_as_registered() {
        let engine = LuaEngine::for_test();
        for (path, class) in [(["ui"], "KojutsuUi"), (["nav"], "KojutsuNav")] {
            assert_eq!(
                functions(&engine, &path),
                declared(class),
                "kojutsu.{} does not match {class}",
                path[0]
            );
        }
    }

    /// Every object reachable in a serialized `Config`, as a JSON pointer.
    fn object_paths(value: &serde_json::Value, path: String, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                out.push(path.clone());
                for (key, child) in map {
                    object_paths(child, format!("{path}/{key}"), out);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    object_paths(child, format!("{path}/{i}"), out);
                }
            }
            _ => {}
        }
    }

    /// The config classes are written by hand, but their fields belong to
    /// `Config`. A field added there and not here is invisible to a plugin
    /// author's LSP; one removed there and left here is a lie.
    #[test]
    fn the_config_classes_match_the_config_struct() {
        let mut config = crate::theme::Config::default();
        // Presets default to empty, so seed one to reach KojutsuPreset.
        config.revsets.presets.push(crate::theme::Preset {
            name: String::new(),
            revset: String::new(),
        });
        let value = serde_json::to_value(&config).expect("serialize");

        let classes = [
            ("", "KojutsuConfig"),
            ("/theme", "KojutsuTheme"),
            ("/glyphs", "KojutsuGlyphs"),
            ("/default_search_scopes", "KojutsuSearchScopes"),
            ("/revsets", "KojutsuRevsets"),
            ("/revsets/presets/0", "KojutsuPreset"),
            ("/run", "KojutsuRun"),
            ("/diff", "KojutsuDiff"),
        ];

        for (pointer, class) in classes {
            let object = value
                .pointer(pointer)
                .and_then(|v| v.as_object())
                .unwrap_or_else(|| panic!("no table at {pointer:?}"));
            let mut fields: Vec<String> = object.keys().cloned().collect();
            fields.sort();
            assert_eq!(
                fields,
                declared(class),
                "{class} does not match {pointer:?}"
            );
        }

        // A new nested section has to get a class rather than be skipped.
        let mut found = Vec::new();
        object_paths(&value, String::new(), &mut found);
        found.sort();
        let mut mapped: Vec<String> = classes.iter().map(|(p, _)| p.to_string()).collect();
        mapped.sort();
        assert_eq!(found, mapped, "a table in Config has no declared class");
    }

    /// The table a plugin actually receives and the class it is documented as
    /// have to carry the same keys, or the LSP a plugin author leans on is
    /// telling them something untrue.
    #[test]
    fn the_jj_result_table_matches_its_declared_class() {
        use crate::jj_command::{Captured, JJCommandResult, Stream};

        let engine = LuaEngine::for_test();
        let mut output = Captured::default();
        output.push(Stream::Stdout, b"a4f2c1");
        output.push_note("\n");
        output.push(Stream::Stderr, b"Warning: no name\n");
        let result = JJCommandResult {
            display: String::new(),
            display_parts: Vec::new(),
            output,
            success: true,
            cancelled: false,
            // Present, so `code` is among the keys to compare.
            code: Some(0),
        };
        let table = super::jj_result_table(&engine.lua, &result).expect("result table");

        let mut keys: Vec<String> = table
            .pairs::<String, mlua::Value>()
            .filter_map(|pair| pair.ok().map(|(key, _)| key))
            .collect();
        keys.sort();
        assert_eq!(keys, declared("JJResult"));

        assert_eq!(table.get::<String>("stdout").unwrap(), "a4f2c1");
        assert_eq!(table.get::<String>("stderr").unwrap(), "Warning: no name\n");
    }

    /// Run a command call inside a coroutine, as a command or hook does.
    /// Reports whether it parked waiting for the main loop, and what it
    /// returned if it did not.
    fn command_call(call: &str) -> (bool, Option<String>) {
        let engine = LuaEngine::for_test();
        // Stand in for the real inline calls, which would spawn a process.
        engine
            .lua
            .load(
                "kojutsu._jj_sync = function() return 'inline' end\
                 \nkojutsu._exec_sync = function() return 'inline' end",
            )
            .exec()
            .expect("stub the inline calls");
        let thread: mlua::Thread = engine
            .lua
            .load(format!(
                "return coroutine.create(function() return {call} end)"
            ))
            .eval()
            .expect("coroutine");
        let value: mlua::Value = thread.resume(()).expect("resume");
        let parked = matches!(thread.status(), mlua::ThreadStatus::Resumable);
        let returned = value
            .as_string()
            .and_then(|s| s.to_str().ok().map(|s| s.to_string()));
        (parked, returned)
    }

    /// A read-only probe wants its answer without an overlay, a command-log
    /// entry, or a trip through the main loop; everything else wants all
    /// three. `quiet` is the switch, and it has to work inside the coroutine a
    /// command or hook runs in, where yielding is possible and would
    /// otherwise win.
    #[test]
    fn quiet_runs_inline_where_a_plain_call_would_yield() {
        assert_eq!(command_call("kojutsu.jj({'log'})"), (true, None));
        assert_eq!(
            command_call("kojutsu.jj({'log'}, {quiet = true})"),
            (false, Some("inline".into()))
        );
        // An explicit `quiet = false` is still the loud path, not truthiness.
        assert_eq!(
            command_call("kojutsu.jj({'log'}, {quiet = false})"),
            (true, None)
        );
    }

    /// `exec` offers the same two paths, so a plugin does not have to know
    /// which of the two it is calling to reason about what the UI will do.
    #[test]
    fn exec_takes_the_same_two_paths_as_jj() {
        assert_eq!(
            command_call("kojutsu.exec({'git', 'status'})"),
            (true, None)
        );
        assert_eq!(
            command_call("kojutsu.exec({'git', 'status'}, {quiet = true})"),
            (false, Some("inline".into()))
        );
    }

    /// Only one direction at the top level: `command`, `hook`, `bind`,
    /// `rebind`, `unbind` and `prefix` are installed while loading init.lua,
    /// which a test engine skips, so they're declared but not present here.
    #[test]
    fn every_registered_top_level_function_is_declared() {
        let engine = LuaEngine::for_test();
        let declared = declared("Kojutsu");
        for name in functions(&engine, &[]) {
            assert!(
                declared.contains(&name),
                "kojutsu.{name} is registered but missing from the type definitions"
            );
        }
    }
}
