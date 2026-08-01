mod api;
pub(crate) mod helpers;
mod init_script;

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use compact_str::CompactString;
use mlua::Lua;

use crate::app::App;
use crate::input::Action;
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{ActionRegistry, AppAction, BindingSpec, CommandFlags};

pub use helpers::generate_type_definitions;

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
    /// registration against the closed set of action names — dispatch is a
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

/// Build the Lua result table for a jj command:
/// `{status = "ok"|"failed"|"cancelled", ok, output, code?}`.
fn jj_result_table(
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
        ResumeValue::JjResult(result) => jj_result_table(
            lua,
            result.success,
            result.cancelled,
            result.code,
            &result.output,
        )
        .map(mlua::Value::Table),
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
    SwitchView(crate::app::ActiveView),
    JumpTo(crate::types::ChangeId),
    Dispatch(AppAction),
}

pub struct LuaEngine {
    lua: Lua,
    commands: Vec<LuaCommand>,
    hooks: Vec<LuaHook>,
    extra_bindings: Vec<BindingSpec>,
    suspended_thread: RefCell<Option<(mlua::RegistryKey, SuspendedKind)>>,
    current_header: RefCell<String>,
    pub init_error: Option<String>,
    repo_path: PathBuf,
}

impl LuaEngine {
    pub fn new(
        repo_path: &Path,
        registry: &mut ActionRegistry,
        default_specs: &[BindingSpec],
    ) -> Self {
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
            init_error: None,
            repo_path: repo_path.to_path_buf(),
        };

        if let Err(e) = engine.register_persistent_functions() {
            engine.init_error = Some(format!("failed to register Lua API: {e}"));
            return engine;
        }

        engine.load_init_script(registry, default_specs);
        engine
    }

    pub fn take_extra_bindings(&mut self) -> Vec<BindingSpec> {
        std::mem::take(&mut self.extra_bindings)
    }

    pub fn has_suspended_thread(&self) -> bool {
        self.suspended_thread.borrow().is_some()
    }

    pub fn run_pre_hooks(
        &self,
        action_name: &str,
        action: AppAction,
        flags: CommandFlags,
        app: &mut App,
    ) -> HookOutcome {
        let hooks_table = match self.collect_hook_functions(action_name, HookPhase::Pre) {
            Some(t) => t,
            None => return HookOutcome::Proceed,
        };
        let ctx = match self.build_ctx_table(app) {
            Ok(t) => t,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return HookOutcome::Proceed;
            }
        };
        let wrapper: mlua::Function = match (|| {
            let kojutsu: mlua::Table = self.lua.globals().get("kojutsu")?;
            kojutsu.get::<mlua::Function>("_run_pre_hooks")
        })() {
            Ok(f) => f,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return HookOutcome::Proceed;
            }
        };
        let thread = match self.lua.create_thread(wrapper) {
            Ok(t) => t,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return HookOutcome::Proceed;
            }
        };
        match thread.resume::<mlua::Value>((hooks_table, ctx)) {
            Ok(value) if thread.status() == mlua::ThreadStatus::Resumable => {
                self.collect_logs(action_name.to_string(), LogPhase::Pre);
                let yield_action = self.handle_yield(
                    thread,
                    value,
                    app,
                    SuspendedKind::PreHooks {
                        action,
                        flags,
                        header: action_name.to_string(),
                    },
                );
                HookOutcome::Suspended(yield_action)
            }
            Ok(mlua::Value::Boolean(false)) => {
                self.collect_logs(action_name.to_string(), LogPhase::Pre);
                self.drain_pending_actions(app, flags, false);
                app.push_command_log(
                    crate::app::CommandLogKind::Warning,
                    format!("plugin: pre-hook cancelled - {action_name}"),
                    None,
                    Vec::new(),
                    false,
                );
                HookOutcome::Cancel
            }
            Ok(_) => {
                self.collect_logs(action_name.to_string(), LogPhase::Pre);
                self.drain_pending_actions(app, flags, false);
                HookOutcome::Proceed
            }
            Err(e) => {
                self.collect_logs(action_name.to_string(), LogPhase::Pre);
                app.set_error(format!("plugin: pre-hook error: {e}"));
                HookOutcome::Proceed
            }
        }
    }

    pub fn run_post_hooks(
        &self,
        action_name: &str,
        app: &mut App,
        outcome: CommandOutcome<'_>,
    ) -> HookOutcome {
        let hooks_table = match self.collect_hook_functions(action_name, HookPhase::Post) {
            Some(t) => t,
            None => return HookOutcome::Proceed,
        };
        let ctx = match self.build_ctx_table(app) {
            Ok(t) => t,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return HookOutcome::Proceed;
            }
        };
        let result_table = match jj_result_table(
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
        let wrapper: mlua::Function = match (|| {
            let kojutsu: mlua::Table = self.lua.globals().get("kojutsu")?;
            kojutsu.get::<mlua::Function>("_run_post_hooks")
        })() {
            Ok(f) => f,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return HookOutcome::Proceed;
            }
        };
        let thread = match self.lua.create_thread(wrapper) {
            Ok(t) => t,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return HookOutcome::Proceed;
            }
        };
        match thread.resume::<mlua::Value>((hooks_table, ctx, result_table)) {
            Ok(value) if thread.status() == mlua::ThreadStatus::Resumable => {
                self.collect_logs(action_name.to_string(), LogPhase::Post);
                let yield_action = self.handle_yield(
                    thread,
                    value,
                    app,
                    SuspendedKind::PostHooks {
                        header: action_name.to_string(),
                    },
                );
                HookOutcome::Suspended(yield_action)
            }
            Ok(_) => {
                self.collect_logs(action_name.to_string(), LogPhase::Post);
                self.drain_pending_actions(app, CommandFlags::empty(), false);
                HookOutcome::Proceed
            }
            Err(e) => {
                self.collect_logs(action_name.to_string(), LogPhase::Post);
                app.set_error(format!("plugin: post-hook error: {e}"));
                HookOutcome::Proceed
            }
        }
    }

    /// The name a command was registered under, for error messages.
    pub fn command_name(&self, id: u16) -> &str {
        &self.commands[id as usize].name
    }

    pub fn execute_command(&self, id: u16, app: &mut App, flags: CommandFlags) -> Action {
        let cmd = &self.commands[id as usize];
        let cmd_name = cmd.name.clone();

        app.push_command_log(
            crate::app::CommandLogKind::Command,
            format!("plugin: {cmd_name}"),
            None,
            Vec::new(),
            true,
        );

        self.prepare_execution();

        let func: mlua::Function = match self.lua.registry_value(&cmd.callback) {
            Ok(f) => f,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return Action::None;
            }
        };
        *self.current_header.borrow_mut() = cmd.source.clone();
        let thread = match self.lua.create_thread(func) {
            Ok(t) => t,
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                return Action::None;
            }
        };

        let ctx_arg = self
            .build_ctx_table(app)
            .map(mlua::Value::Table)
            .unwrap_or(mlua::Value::Nil);
        self.resume_and_handle(thread, ctx_arg, app, SuspendedKind::Command(flags))
    }

    pub fn resume_suspended(&self, app: &mut App, value: ResumeValue) -> ResumeResult {
        let (thread_key, kind) = match self.suspended_thread.borrow_mut().take() {
            Some(pair) => pair,
            None => return ResumeResult::Action(Action::None),
        };
        let thread: mlua::Thread = match self.lua.registry_value(&thread_key) {
            Ok(t) => t,
            Err(e) => {
                app.set_error(format!("plugin resume: {e}"));
                return ResumeResult::Action(Action::None);
            }
        };
        let _ = self.lua.remove_registry_value(thread_key);

        {
            let cell = lua_state!(self.lua);
            let mut state = cell.borrow_mut();
            state.pending_actions.clear();
            state.pending_logs.clear();
        }

        match thread.resume::<mlua::Value>(resume_value_to_lua(&self.lua, value)) {
            Ok(value) if thread.status() == mlua::ThreadStatus::Resumable => {
                ResumeResult::Action(self.handle_yield(thread, value, app, kind))
            }
            Ok(value) => match kind {
                SuspendedKind::Command(flags) => {
                    self.collect_logs(self.current_header.borrow().clone(), LogPhase::Command);
                    ResumeResult::Action(self.drain_pending_actions(app, flags, true))
                }
                SuspendedKind::PreHooks {
                    action,
                    flags,
                    header,
                } => {
                    self.collect_logs(header, LogPhase::Pre);
                    self.drain_pending_actions(app, flags, false);
                    if matches!(value, mlua::Value::Boolean(false)) {
                        ResumeResult::Action(Action::None)
                    } else {
                        ResumeResult::DispatchAction { action, flags }
                    }
                }
                SuspendedKind::PostHooks { header } => {
                    self.collect_logs(header, LogPhase::Post);
                    self.drain_pending_actions(app, CommandFlags::empty(), false);
                    ResumeResult::Action(Action::None)
                }
            },
            Err(e) => {
                app.set_error(format!("plugin: {e}"));
                ResumeResult::Action(Action::None)
            }
        }
    }

    pub fn cancel_suspended_thread(&self) {
        if let Some((key, _)) = self.suspended_thread.borrow_mut().take() {
            let _ = self.lua.remove_registry_value(key);
        }
    }

    fn resume_and_handle(
        &self,
        thread: mlua::Thread,
        arg: mlua::Value,
        app: &mut App,
        kind: SuspendedKind,
    ) -> Action {
        match thread.resume::<mlua::Value>(arg) {
            Ok(value) if thread.status() == mlua::ThreadStatus::Resumable => {
                self.handle_yield(thread, value, app, kind)
            }
            Ok(_) => {
                if let SuspendedKind::Command(flags) = kind {
                    self.collect_logs(self.current_header.borrow().clone(), LogPhase::Command);
                    self.drain_pending_actions(app, flags, true)
                } else {
                    Action::None
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
                Action::None
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
                        crate::types::PendingCommand::LuaResume,
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
            "jj" => {
                let args: Vec<String> = table
                    .get::<mlua::Table>("args")
                    .map(|t| helpers::table_to_string_vec(&t))
                    .unwrap_or_default();
                if args.is_empty() {
                    app.set_error("plugin: jj requires at least one argument");
                    return Action::None;
                }
                let flags = match &kind {
                    SuspendedKind::Command(flags) => *flags,
                    SuspendedKind::PreHooks { flags, .. } => *flags,
                    SuspendedKind::PostHooks { .. } => CommandFlags::empty(),
                };
                let cmd = JJCommand {
                    kind: JJCommandKind::Raw {
                        args: args.into_iter().map(Into::into).collect(),
                    },
                    flags,
                };
                // The command runs through the normal pipeline (running
                // overlay, live output, Esc/^C cancellation); its completion
                // resumes this thread with the result table.
                (None, Action::RunJjForLua(cmd))
            }
            other => {
                app.set_error(format!("plugin: unknown yield type '{other}'"));
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
    /// the main loop (`jj_interactive`, `dispatch`) is returned — extras are
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
                    app.request_revset_load_no_snapshot(Some(revset));
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
                    self.set_breaking(
                        app,
                        &mut breaking,
                        Action::SuspendAndRunJj(cmd),
                        allow_breaking,
                    );
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
        let cell = lua_state!(self.lua);
        let mut state = cell.borrow_mut();
        state.pending_actions.clear();
        state.pending_logs.clear();
        state.log_groups.clear();
    }

    fn collect_logs(&self, header: String, phase: LogPhase) {
        let cell = lua_state!(self.lua);
        let mut state = cell.borrow_mut();
        let messages: Vec<String> = state.pending_logs.drain(..).collect();
        if messages.is_empty() {
            return;
        }
        state.log_groups.push(LogGroup {
            header,
            phase,
            messages,
        });
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

        fn render_group(group: &LogGroup, out: &mut Vec<u8>) {
            if !group.header.is_empty() {
                out.extend_from_slice(format!("── {} ──\n", group.header).as_bytes());
            }
            for msg in &group.messages {
                out.extend_from_slice(msg.as_bytes());
                out.push(b'\n');
            }
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

        for group in groups {
            for msg in group.messages {
                app.push_command_log(
                    crate::app::CommandLogKind::Background,
                    msg,
                    None,
                    Vec::new(),
                    true,
                );
            }
        }
    }

    fn build_ctx_table(&self, app: &App) -> mlua::Result<mlua::Table> {
        let ctx = self.lua.create_table()?;
        let change_id = app.selected_change_id();
        let commit_id = change_id.as_ref().and_then(|cid| {
            app.commit_id_for_change(cid)
                .map(|id| CompactString::from(id.as_str()))
        });
        match &change_id {
            Some(id) => ctx.set("change_id", id.as_str())?,
            None => ctx.set("change_id", mlua::Value::Nil)?,
        }
        match &commit_id {
            Some(id) => ctx.set("commit_id", id.as_str())?,
            None => ctx.set("commit_id", mlua::Value::Nil)?,
        }
        let change_ids = app.selected_change_ids();
        let ids_table = self.lua.create_table()?;
        for (i, id) in change_ids.iter().enumerate() {
            ids_table.raw_set(i + 1, id.as_str())?;
        }
        ctx.set("change_ids", ids_table)?;
        if let Some(desc) = app.selected_description() {
            ctx.set("description", desc.to_string())?;
        }
        if let Some(bookmarks) = app.selected_bookmarks() {
            let bm_table = self.lua.create_table()?;
            for (i, b) in bookmarks.iter().enumerate() {
                bm_table.raw_set(i + 1, b.name.as_str())?;
            }
            ctx.set("bookmarks", bm_table)?;
        }
        if let Some(tags) = app.selected_tags() {
            let tag_table = self.lua.create_table()?;
            for (i, t) in tags.iter().enumerate() {
                tag_table.raw_set(i + 1, t.as_str())?;
            }
            ctx.set("tags", tag_table)?;
        }
        if let Some(path) = app.selected_file_path() {
            ctx.set("file_path", path.as_str())?;
        }
        ctx.set("is_working_copy", app.selected_is_working_copy())?;
        ctx.set("is_empty", app.selected_is_empty())?;
        ctx.set("has_conflict", app.selected_has_conflict())?;
        if let Some(entry_idx) = app.selected_entry_idx() {
            let commit = &app.nodes[entry_idx].commit;
            ctx.set("author_name", commit.author.name.as_str())?;
            ctx.set("author_email", commit.author.email.as_str())?;
            ctx.set("is_immutable", commit.is_immutable)?;
            ctx.set("is_merge", commit.is_merge)?;
            let parents = self.lua.create_table()?;
            for (i, parent_idx) in app.nodes[entry_idx].parents.iter().enumerate() {
                let parent_change = app.nodes[*parent_idx].commit.unique_change_id();
                parents.raw_set(i + 1, parent_change.as_str())?;
            }
            ctx.set("parent_change_ids", parents)?;
        }
        ctx.set("view", app.active_view.to_string())?;
        ctx.set("revset", app.revset.current.as_str())?;
        ctx.set("repo_root", app.repo_root.as_str())?;
        Ok(ctx)
    }
}
