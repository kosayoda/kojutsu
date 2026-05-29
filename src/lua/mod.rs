use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use compact_str::CompactString;
use mlua::Lua;

use crate::app::App;
use crate::input::Action;
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{
    ActionRegistry, AppAction, BindTarget, BindingSpec, CommandFlags, HelpGroup, Scope,
    SelectionKindSet,
};

struct LuaCommand {
    name: CompactString,
    source: String,
    callback: mlua::RegistryKey,
}

struct LuaHook {
    phase: HookPhase,
    callback: mlua::RegistryKey,
    source: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookPhase {
    Pre,
    Post,
}

pub enum HookOutcome {
    Proceed,
    Cancel,
    Suspended,
}

pub enum ResumeResult {
    Action(Action),
    DispatchAction {
        action: AppAction,
        flags: CommandFlags,
    },
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

pub struct LuaEngine {
    lua: Lua,
    commands: Vec<LuaCommand>,
    hooks: Vec<(CompactString, LuaHook)>,
    extra_bindings: Vec<BindingSpec>,
    suspended_thread: RefCell<Option<(mlua::RegistryKey, SuspendedKind)>>,
    current_header: RefCell<String>,
    pending_action: Rc<RefCell<PendingAction>>,
    pending_logs: Rc<RefCell<Vec<String>>>,
    log_groups: Rc<RefCell<Vec<LogGroup>>>,
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
        let pending_action = Rc::new(RefCell::new(PendingAction::None));
        let pending_logs = Rc::new(RefCell::new(Vec::new()));
        let mut engine = LuaEngine {
            lua,
            commands: Vec::new(),
            hooks: Vec::new(),
            extra_bindings: Vec::new(),
            suspended_thread: RefCell::new(None),
            pending_action,
            pending_logs,
            log_groups: Rc::new(RefCell::new(Vec::new())),
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
                self.handle_yield(
                    thread,
                    value,
                    app,
                    SuspendedKind::PreHooks {
                        action,
                        flags,
                        header: action_name.to_string(),
                    },
                );
                HookOutcome::Suspended
            }
            Ok(mlua::Value::Boolean(false)) => {
                self.collect_logs(action_name.to_string(), LogPhase::Pre);
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
        success: bool,
        output: &[u8],
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
        let result_table = match (|| {
            let t = self.lua.create_table()?;
            t.set("ok", success)?;
            t.set("output", String::from_utf8_lossy(output).into_owned())?;
            Ok::<_, mlua::Error>(t)
        })() {
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
                self.handle_yield(
                    thread,
                    value,
                    app,
                    SuspendedKind::PostHooks {
                        header: action_name.to_string(),
                    },
                );
                HookOutcome::Suspended
            }
            Ok(_) => {
                self.collect_logs(action_name.to_string(), LogPhase::Post);
                HookOutcome::Proceed
            }
            Err(e) => {
                self.collect_logs(action_name.to_string(), LogPhase::Post);
                app.set_error(format!("plugin: post-hook error: {e}"));
                HookOutcome::Proceed
            }
        }
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

        self.prepare_execution(app);

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

        self.resume_and_handle(thread, mlua::Value::Nil, app, SuspendedKind::Command(flags))
    }

    pub fn resume_suspended(&self, app: &mut App, value: Option<String>) -> ResumeResult {
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

        self.update_ctx(app);
        self.pending_action.replace(PendingAction::None);
        self.pending_logs.borrow_mut().clear();

        let lua_value = match value {
            Some(s) => match self.lua.create_string(&s) {
                Ok(ls) => mlua::Value::String(ls),
                Err(_) => mlua::Value::Nil,
            },
            None => mlua::Value::Nil,
        };

        match thread.resume::<mlua::Value>(lua_value) {
            Ok(value) if thread.status() == mlua::ThreadStatus::Resumable => {
                self.handle_yield(thread, value, app, kind);
                ResumeResult::Action(Action::None)
            }
            Ok(value) => match kind {
                SuspendedKind::Command(flags) => {
                    self.collect_logs(self.current_header.borrow().clone(), LogPhase::Command);
                    ResumeResult::Action(self.take_pending_action(flags))
                }
                SuspendedKind::PreHooks {
                    action,
                    flags,
                    header,
                } => {
                    self.collect_logs(header, LogPhase::Pre);
                    if matches!(value, mlua::Value::Boolean(false)) {
                        ResumeResult::Action(Action::None)
                    } else {
                        ResumeResult::DispatchAction { action, flags }
                    }
                }
                SuspendedKind::PostHooks { header } => {
                    self.collect_logs(header, LogPhase::Post);
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
                self.handle_yield(thread, value, app, kind);
                Action::None
            }
            Ok(_) => {
                if let SuspendedKind::Command(flags) = kind {
                    self.collect_logs(self.current_header.borrow().clone(), LogPhase::Command);
                    self.take_pending_action(flags)
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
            .filter(|(name, hook)| {
                hook.phase == phase && hook_matches(&self.lua, name, action_name).unwrap_or(false)
            })
            .map(|(_, hook)| hook)
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

    fn handle_yield(
        &self,
        thread: mlua::Thread,
        value: mlua::Value,
        app: &mut App,
        kind: SuspendedKind,
    ) {
        let table = match value {
            mlua::Value::Table(t) => t,
            _ => {
                app.set_error("plugin: invalid yield (expected table)");
                return;
            }
        };
        let request_type: String = table.get("type").unwrap_or_default();

        let mode = match request_type.as_str() {
            "input" => {
                let prompt: String = table.get("prompt").unwrap_or_default();
                let default: String = table.get("default").unwrap_or_default();
                crate::app::AppMode::text_input(
                    &prompt,
                    &default,
                    crate::types::PendingCommand::LuaResume,
                )
            }
            "choose" => {
                let items: Vec<String> = table
                    .get::<mlua::Table>("items")
                    .map(|t| table_to_string_vec(&t))
                    .unwrap_or_default();
                if items.is_empty() {
                    app.set_error("plugin: choose requires non-empty items");
                    return;
                }
                let title: String = table.get("title").unwrap_or_default();
                let multi: bool = table.get("multi").unwrap_or(false);
                crate::app::AppMode::select_from_list(
                    title,
                    items,
                    multi,
                    crate::types::PendingSelection::LuaResume,
                    false,
                )
            }
            other => {
                app.set_error(format!("plugin: unknown yield type '{other}'"));
                return;
            }
        };

        let key = match self.lua.create_registry_value(thread) {
            Ok(k) => k,
            Err(e) => {
                app.set_error(format!("plugin: failed to store thread: {e}"));
                return;
            }
        };
        *self.suspended_thread.borrow_mut() = Some((key, kind));
        app.mode = mode;
    }

    fn take_pending_action(&self, flags: CommandFlags) -> Action {
        match self.pending_action.replace(PendingAction::None) {
            PendingAction::None => Action::None,
            PendingAction::Refresh => Action::Refresh,
            PendingAction::SetRevset(revset) => Action::UpdateRevset(revset),
            PendingAction::Interactive(args) => {
                let cmd = JJCommand {
                    kind: JJCommandKind::Raw {
                        args: args.into_iter().map(Into::into).collect(),
                    },
                    flags,
                };
                Action::SuspendAndRunJj(cmd)
            }
        }
    }

    fn prepare_execution(&self, app: &App) {
        self.update_ctx(app);
        self.pending_action.replace(PendingAction::None);
        self.pending_logs.borrow_mut().clear();
        self.log_groups.borrow_mut().clear();
    }

    fn collect_logs(&self, header: String, phase: LogPhase) {
        let messages: Vec<String> = self.pending_logs.borrow_mut().drain(..).collect();
        if messages.is_empty() {
            return;
        }
        self.log_groups.borrow_mut().push(LogGroup {
            header,
            phase,
            messages,
        });
    }

    pub fn flush_logs(&self, app: &mut App) {
        let mut groups: Vec<LogGroup> = self.log_groups.borrow_mut().drain(..).collect();
        let stray: Vec<String> = self.pending_logs.borrow_mut().drain(..).collect();
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
            crate::app::AppMode::CommandOutput { output, .. } => {
                let pre: Vec<&LogGroup> =
                    groups.iter().filter(|g| g.phase == LogPhase::Pre).collect();
                if !pre.is_empty() {
                    let mut pre_output = Vec::new();
                    for g in &pre {
                        render_group(g, &mut pre_output);
                    }
                    pre_output.push(b'\n');
                    pre_output.extend_from_slice(output);
                    *output = pre_output;
                }
                let post: Vec<&LogGroup> =
                    groups.iter().filter(|g| g.phase != LogPhase::Pre).collect();
                if !post.is_empty() {
                    output.push(b'\n');
                    for g in &post {
                        render_group(g, output);
                    }
                }
            }
            _ => {
                let mut output = Vec::new();
                for g in &groups {
                    render_group(g, &mut output);
                }
                app.mode = crate::app::AppMode::CommandOutput {
                    command: String::new(),
                    command_parts: None,
                    output,
                    success: true,
                    retry: Vec::new(),
                };
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
        ctx.set("view", view_name(app.active_view))?;
        ctx.set("revset", app.revset.current.as_str())?;
        ctx.set("repo_root", app.repo_root.as_str())?;
        Ok(ctx)
    }

    fn update_ctx(&self, app: &App) {
        let Ok(kojutsu): Result<mlua::Table, _> = self.lua.globals().get("kojutsu") else {
            return;
        };
        if let Ok(ctx) = self.build_ctx_table(app) {
            let _ = kojutsu.set("ctx", ctx);
        }
    }

    fn register_persistent_functions(&self) -> mlua::Result<()> {
        register_globals(&self.lua)?;

        let repo_path = self.repo_path.clone();
        let jj_fn = self.lua.create_function(move |lua, args: mlua::Table| {
            let cmd_args: Vec<String> = (1..=args.raw_len())
                .map(|i| args.raw_get(i))
                .collect::<mlua::Result<_>>()?;
            let result = JJCommand {
                kind: JJCommandKind::Raw {
                    args: cmd_args.into_iter().map(Into::into).collect(),
                },
                flags: CommandFlags::empty(),
            }
            .run(&repo_path);
            let tbl = lua.create_table()?;
            tbl.set("ok", result.success)?;
            tbl.set(
                "output",
                String::from_utf8_lossy(&result.output).into_owned(),
            )?;
            Ok(tbl)
        })?;

        let pending_interactive = self.pending_action.clone();
        let jj_interactive_fn = self.lua.create_function(move |_lua, args: mlua::Table| {
            let cmd_args: Vec<String> = (1..=args.raw_len())
                .map(|i| args.raw_get(i))
                .collect::<mlua::Result<_>>()?;
            *pending_interactive.borrow_mut() = PendingAction::Interactive(cmd_args);
            Ok(())
        })?;

        let pending_refresh = self.pending_action.clone();
        let refresh_fn = self.lua.create_function(move |_lua, ()| {
            let mut p = pending_refresh.borrow_mut();
            if matches!(*p, PendingAction::None) {
                *p = PendingAction::Refresh;
            }
            Ok(())
        })?;

        let pending_revset = self.pending_action.clone();
        let set_revset_fn = self.lua.create_function(move |_lua, revset: String| {
            *pending_revset.borrow_mut() = PendingAction::SetRevset(revset);
            Ok(())
        })?;

        let flash_fn = self.lua.create_function(|_lua, _msg: String| Ok(()))?;

        let copy_fn = self.lua.create_function(|_lua, text: String| {
            use base64::Engine;
            use std::io::Write;

            let osc52_ok = (|| -> std::io::Result<()> {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&text);
                write!(std::io::stdout(), "\x1b]52;c;{encoded}\x07")?;
                std::io::stdout().flush()
            })()
            .is_ok();

            let arboard_ok = arboard::Clipboard::new()
                .and_then(|mut cb| cb.set_text(&text))
                .is_ok();

            if !osc52_ok && !arboard_ok {
                return Err(mlua::Error::external("no clipboard available"));
            }
            Ok(())
        })?;

        let logs = self.pending_logs.clone();
        let log_fn = self.lua.create_function(move |_lua, msg: String| {
            logs.borrow_mut().push(msg);
            Ok(())
        })?;

        let collect_logs_pending = self.pending_logs.clone();
        let collect_logs_groups = self.log_groups.clone();
        let collect_logs_fn =
            self.lua
                .create_function(move |_lua, (header, phase_str): (String, String)| {
                    let messages: Vec<String> = collect_logs_pending.borrow_mut().drain(..).collect();
                    if !messages.is_empty() {
                        let phase = match phase_str.as_str() {
                            "pre" => LogPhase::Pre,
                            "post" => LogPhase::Post,
                            _ => LogPhase::Command,
                        };
                        collect_logs_groups
                            .borrow_mut()
                            .push(LogGroup { header, phase, messages });
                    }
                    Ok(())
                })?;

        self.lua.load(r#"
            function kojutsu.ui.input(prompt, default)
                return coroutine.yield({type = "input", prompt = prompt or "", default = default or ""})
            end
            function kojutsu.ui.choose(title, items, multi)
                return coroutine.yield({type = "choose", title = title or "", items = items or {}, multi = multi or false})
            end
            function kojutsu._run_pre_hooks(hooks, ctx)
                for _, entry in ipairs(hooks) do
                    local ok, result = pcall(entry.fn, ctx)
                    kojutsu._collect_logs(entry.source, "pre")
                    if not ok then
                        kojutsu.log("hook error: " .. tostring(result))
                        kojutsu._collect_logs(entry.source, "pre")
                    elseif result == false then
                        return false
                    end
                end
                return true
            end
            function kojutsu._run_post_hooks(hooks, ctx, result_table)
                for _, entry in ipairs(hooks) do
                    local ok, err = pcall(entry.fn, ctx, result_table)
                    kojutsu._collect_logs(entry.source, "post")
                    if not ok then
                        kojutsu.log("hook error: " .. tostring(err))
                        kojutsu._collect_logs(entry.source, "post")
                    end
                end
            end
        "#).exec()?;

        let kojutsu: mlua::Table = self.lua.globals().get("kojutsu")?;
        kojutsu.set("jj", jj_fn)?;
        kojutsu.set("jj_interactive", jj_interactive_fn)?;
        kojutsu.set("log", log_fn)?;
        kojutsu.set("copy", copy_fn)?;
        kojutsu.set("_collect_logs", collect_logs_fn)?;

        let ui: mlua::Table = kojutsu.get("ui")?;
        ui.set("flash", flash_fn)?;

        let nav: mlua::Table = kojutsu.get("nav")?;
        nav.set("refresh", refresh_fn)?;
        nav.set("set_revset", set_revset_fn)?;

        Ok(())
    }

    fn load_init_script(&mut self, registry: &mut ActionRegistry, default_specs: &[BindingSpec]) {
        let Some(config_dir) = crate::theme::kojutsu_config_dir() else {
            return;
        };
        let init_path = config_dir.join("init.lua");
        if !init_path.exists() {
            return;
        }

        let lua_dir = config_dir.join("lua");
        if lua_dir.is_dir() {
            let path_addition = format!("{}/?.lua", lua_dir.display());
            let _ = self
                .lua
                .load(format!("package.path = package.path .. ';{path_addition}'"))
                .exec();
        }

        let source = match std::fs::read_to_string(&init_path) {
            Ok(s) => s,
            Err(e) => {
                self.init_error = Some(format!("failed to read init.lua: {e}"));
                return;
            }
        };

        let reg_commands: Rc<RefCell<Vec<PendingRegistration>>> = Rc::new(RefCell::new(Vec::new()));
        let reg_hooks: Rc<RefCell<Vec<(CompactString, LuaHook)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let reg_bindings: Rc<RefCell<Vec<PendingBinding>>> = Rc::new(RefCell::new(Vec::new()));

        if let Err(e) = (|| -> mlua::Result<()> {
            let reg_clone = reg_commands.clone();
            let command_fn = self
                .lua
                .create_function(move |lua, args: mlua::MultiValue| {
                    if args.len() < 3 {
                        return Err(mlua::Error::external(
                            "kojutsu.command requires 3 args: name, fn, opts",
                        ));
                    }
                    let mut iter = args.into_iter();
                    let name: String = mlua::FromLua::from_lua(iter.next().unwrap(), lua)?;
                    let func: mlua::Function = mlua::FromLua::from_lua(iter.next().unwrap(), lua)?;
                    let opts: mlua::Table = mlua::FromLua::from_lua(iter.next().unwrap(), lua)?;

                    let desc: String = opts.get::<String>("desc").unwrap_or_else(|_| name.clone());
                    let scope: String =
                        opts.get::<String>("scope").unwrap_or_else(|_| "all".into());
                    let key: Option<String> = opts.get("key").ok();
                    let seq: Option<String> = opts.get("seq").ok();
                    let group: String = opts
                        .get::<String>("group")
                        .unwrap_or_else(|_| "commands".into());
                    let selection: String = opts
                        .get::<String>("selection")
                        .unwrap_or_else(|_| "all".into());

                    let source = lua_source_info(lua, &func);
                    let callback_key = lua.create_registry_value(func)?;

                    reg_clone.borrow_mut().push(PendingRegistration {
                        name: name.into(),
                        desc: desc.into(),
                        scope,
                        key,
                        seq,
                        group,
                        selection,
                        source,
                        callback_key,
                    });

                    Ok(())
                })?;

            let hooks_clone = reg_hooks.clone();
            let hook_fn = self
                .lua
                .create_function(move |lua, args: mlua::MultiValue| {
                    if args.len() < 3 {
                        return Err(mlua::Error::external(
                            "kojutsu.hook requires 3 args: action(s), phase, fn",
                        ));
                    }
                    let mut iter = args.into_iter();
                    let first = iter.next().unwrap();
                    let action_names: Vec<String> =
                        match first {
                            mlua::Value::String(s) => vec![s.to_str()?.to_string()],
                            mlua::Value::Table(t) => table_to_string_vec(&t),
                            _ => return Err(mlua::Error::external(
                                "first argument must be an action name or table of action names",
                            )),
                        };
                    let phase_str: String = mlua::FromLua::from_lua(iter.next().unwrap(), lua)?;
                    let func: mlua::Function = mlua::FromLua::from_lua(iter.next().unwrap(), lua)?;
                    let phase = match phase_str.as_str() {
                        "pre" => HookPhase::Pre,
                        "post" => HookPhase::Post,
                        other => {
                            return Err(mlua::Error::external(format!(
                                "invalid hook phase: {other} (expected 'pre' or 'post')"
                            )))
                        }
                    };
                    let source = lua_source_info(lua, &func);
                    let mut hooks = hooks_clone.borrow_mut();
                    for name in action_names {
                        let callback = lua.create_registry_value(func.clone())?;
                        hooks.push((name.into(), LuaHook { phase, callback, source: source.clone() }));
                    }
                    Ok(())
                })?;

            let bindings_clone = reg_bindings.clone();
            let bind_fn = self.lua.create_function(move |_lua, opts: mlua::Table| {
                let action: String = opts.get("action")?;
                let scope: String = opts.get::<String>("scope").unwrap_or_else(|_| "all".into());
                let key: Option<String> = opts.get("key").ok();
                let seq: Option<String> = opts.get("seq").ok();
                let desc: Option<String> = opts.get("desc").ok();
                bindings_clone.borrow_mut().push(PendingBinding::Bind {
                    action,
                    scope,
                    key,
                    seq,
                    desc,
                    warn_shadow: true,
                });
                Ok(())
            })?;

            let rebind_bindings_clone = reg_bindings.clone();
            let rebind_fn = self.lua.create_function(move |_lua, opts: mlua::Table| {
                let action: String = opts.get("action")?;
                let scope: String = opts.get::<String>("scope").unwrap_or_else(|_| "all".into());
                let key: Option<String> = opts.get("key").ok();
                let seq: Option<String> = opts.get("seq").ok();
                let desc: Option<String> = opts.get("desc").ok();
                rebind_bindings_clone
                    .borrow_mut()
                    .push(PendingBinding::Bind {
                        action,
                        scope,
                        key,
                        seq,
                        desc,
                        warn_shadow: false,
                    });
                Ok(())
            })?;

            let unbindings_clone = reg_bindings.clone();
            let unbind_fn = self.lua.create_function(move |_lua, opts: mlua::Table| {
                let scope: String = opts.get::<String>("scope").unwrap_or_else(|_| "all".into());
                let key: Option<String> = opts.get("key").ok();
                let seq: Option<String> = opts.get("seq").ok();
                unbindings_clone
                    .borrow_mut()
                    .push(PendingBinding::Unbind { scope, key, seq });
                Ok(())
            })?;

            let kojutsu: mlua::Table = self.lua.globals().get("kojutsu")?;
            kojutsu.set("command", command_fn)?;
            kojutsu.set("hook", hook_fn)?;
            kojutsu.set("bind", bind_fn)?;
            kojutsu.set("rebind", rebind_fn)?;
            kojutsu.set("unbind", unbind_fn)?;
            Ok(())
        })() {
            self.init_error = Some(format!("{e}"));
            return;
        }

        // Verify registration functions are on the table.
        if let Err(e) = self
            .lua
            .load(&source)
            .set_name(&init_path.display().to_string())
            .exec()
        {
            self.init_error = Some(format!("{e}"));
            return;
        }

        let registrations = std::mem::take(&mut *reg_commands.borrow_mut());

        for reg in registrations {
            let selection_support = match reg.selection.as_str() {
                "commit" => SelectionKindSet::COMMIT,
                "file" => SelectionKindSet::FILE,
                "line" => SelectionKindSet::LINE,
                _ => SelectionKindSet::ALL,
            };
            let action_id = registry.register_lua(selection_support, false, false);

            self.commands.push(LuaCommand {
                name: reg.name.clone(),
                source: reg.source.clone(),
                callback: reg.callback_key,
            });

            let help_group = match reg.group.as_str() {
                "navigation" => HelpGroup::Navigation,
                "general" => HelpGroup::General,
                _ => HelpGroup::Commands,
            };

            let scope = parse_scope(&reg.scope);

            let Some(keys) = parse_keys(reg.key.as_ref(), reg.seq.as_ref()) else {
                continue;
            };
            self.extra_bindings.push(BindingSpec {
                keys,
                target: BindTarget::Action {
                    id: action_id,
                    description: reg.desc.clone(),
                    group: help_group,
                },
                scope,
            });
        }

        self.hooks
            .extend(std::mem::take(&mut *reg_hooks.borrow_mut()));

        let bindings = std::mem::take(&mut *reg_bindings.borrow_mut());
        for binding in bindings {
            match binding {
                PendingBinding::Bind {
                    action,
                    scope,
                    key,
                    seq,
                    desc,
                    warn_shadow,
                } => {
                    let Some(action_id) = registry.find_by_name(&action) else {
                        tracing::warn!("kojutsu.bind: unknown action '{action}'");
                        continue;
                    };
                    let Some(keys) = parse_keys(key.as_ref(), seq.as_ref()) else {
                        continue;
                    };
                    if warn_shadow {
                        let parsed_scope = parse_scope(&scope);
                        if shadows_default(default_specs, &keys, &parsed_scope) {
                            let key_display = key.as_deref().or(seq.as_deref()).unwrap_or("?");
                            tracing::warn!(
                                "kojutsu.bind: '{key_display}' shadows an existing binding \
                                 (use kojutsu.rebind to suppress this warning)"
                            );
                        }
                    }
                    let description = desc.unwrap_or_else(|| action.clone());
                    self.extra_bindings.push(BindingSpec {
                        keys,
                        target: BindTarget::Action {
                            id: action_id,
                            description: description.into(),
                            group: HelpGroup::Commands,
                        },
                        scope: parse_scope(&scope),
                    });
                }
                PendingBinding::Unbind { scope, key, seq } => {
                    let Some(keys) = parse_keys(key.as_ref(), seq.as_ref()) else {
                        continue;
                    };
                    self.extra_bindings.push(BindingSpec {
                        keys,
                        target: BindTarget::Unbind,
                        scope: parse_scope(&scope),
                    });
                }
            }
        }
    }
}

fn shadows_default(
    default_specs: &[BindingSpec],
    keys: &[keymap_parser::Node],
    scope: &Scope,
) -> bool {
    default_specs.iter().any(|spec| {
        if spec.keys.as_slice() != keys {
            return false;
        }
        if matches!(&spec.target, BindTarget::Unbind) {
            return false;
        }
        match (&spec.scope, scope) {
            (Scope::All, _) | (_, Scope::All) => true,
            (Scope::Views(a), Scope::Views(b)) => a.iter().any(|v| b.contains(v)),
        }
    })
}

fn parse_keys(
    key: Option<&String>,
    seq: Option<&String>,
) -> Option<smallvec::SmallVec<[keymap_parser::Node; 3]>> {
    if let Some(k) = key {
        Some(smallvec::smallvec![crate::keymap::parse_key(k)])
    } else {
        seq.map(|s| s.split_whitespace().map(crate::keymap::parse_key).collect())
    }
}

pub fn generate_type_definitions() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    writeln!(out, "---@meta").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuAction").unwrap();
    for &action in crate::keymap::ALL_ACTIONS {
        let name = crate::keymap::action_id_name(action);
        writeln!(out, "---@field {name} string").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuScope").unwrap();
    for name in [
        "all",
        "dag",
        "bookmarks",
        "tags",
        "operations",
        "workspaces",
        "evolog",
        "command_log",
        "interdiff",
        "annotate",
    ] {
        writeln!(out, "---@field {name} string").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuPhase").unwrap();
    writeln!(out, "---@field pre string").unwrap();
    writeln!(out, "---@field post string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuSelection").unwrap();
    for name in ["all", "commit", "file", "line"] {
        writeln!(out, "---@field {name} string").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuCtx").unwrap();
    writeln!(out, "---@field change_id string?").unwrap();
    writeln!(out, "---@field commit_id string?").unwrap();
    writeln!(out, "---@field description string?").unwrap();
    writeln!(out, "---@field bookmarks string[]?").unwrap();
    writeln!(out, "---@field view string").unwrap();
    writeln!(out, "---@field revset string").unwrap();
    writeln!(out, "---@field repo_root string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class JJResult").unwrap();
    writeln!(out, "---@field ok boolean").unwrap();
    writeln!(out, "---@field output string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuUi").unwrap();
    writeln!(out, "---@field flash fun(msg: string)").unwrap();
    writeln!(out, "---@field error fun(msg: string)").unwrap();
    writeln!(
        out,
        "---@field input fun(prompt: string, default: string?): string?"
    )
    .unwrap();
    writeln!(
        out,
        "---@field choose fun(title: string, items: string[], multi: boolean?): string?"
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuNav").unwrap();
    writeln!(out, "---@field refresh fun()").unwrap();
    writeln!(out, "---@field set_revset fun(revset: string)").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class CommandOpts").unwrap();
    writeln!(out, "---@field desc string?").unwrap();
    writeln!(out, "---@field scope string?").unwrap();
    writeln!(out, "---@field key string?").unwrap();
    writeln!(out, "---@field seq string?").unwrap();
    writeln!(out, "---@field group string?").unwrap();
    writeln!(out, "---@field selection string?").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class BindOpts").unwrap();
    writeln!(out, "---@field action string").unwrap();
    writeln!(out, "---@field scope string?").unwrap();
    writeln!(out, "---@field key string?").unwrap();
    writeln!(out, "---@field seq string?").unwrap();
    writeln!(out, "---@field desc string?").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class UnbindOpts").unwrap();
    writeln!(out, "---@field scope string?").unwrap();
    writeln!(out, "---@field key string?").unwrap();
    writeln!(out, "---@field seq string?").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class HookContext").unwrap();
    writeln!(out, "---@field change_id string?").unwrap();
    writeln!(out, "---@field change_ids string[]").unwrap();
    writeln!(out, "---@field view string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class HookResultTable").unwrap();
    writeln!(out, "---@field ok boolean").unwrap();
    writeln!(out, "---@field output string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class Kojutsu").unwrap();
    writeln!(out, "---@field action KojutsuAction").unwrap();
    writeln!(out, "---@field scope KojutsuScope").unwrap();
    writeln!(out, "---@field phase KojutsuPhase").unwrap();
    writeln!(out, "---@field selection KojutsuSelection").unwrap();
    writeln!(out, "---@field ctx KojutsuCtx").unwrap();
    writeln!(out, "---@field ui KojutsuUi").unwrap();
    writeln!(out, "---@field nav KojutsuNav").unwrap();
    writeln!(out, "---@field jj fun(args: string[]): JJResult").unwrap();
    writeln!(out, "---@field jj_interactive fun(args: string[])").unwrap();
    writeln!(out, "---@field log fun(msg: string)").unwrap();
    writeln!(out, "---@field copy fun(text: string)").unwrap();
    writeln!(
        out,
        "---@field command fun(name: string, fn: fun(ctx: KojutsuCtx), opts: CommandOpts)"
    )
    .unwrap();
    writeln!(
        out,
        "---@field hook fun(action: string|string[], phase: string, fn: fun(ctx: HookContext, result: HookResultTable?): boolean?)"
    )
    .unwrap();
    writeln!(out, "---@field bind fun(opts: BindOpts)").unwrap();
    writeln!(out, "---@field rebind fun(opts: BindOpts)").unwrap();
    writeln!(out, "---@field unbind fun(opts: UnbindOpts)").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "---@type Kojutsu").unwrap();
    writeln!(out, "kojutsu = {{}}").unwrap();

    out
}

fn register_globals(lua: &Lua) -> mlua::Result<()> {
    let kojutsu = lua.create_table()?;
    kojutsu.set("ui", lua.create_table()?)?;
    kojutsu.set("nav", lua.create_table()?)?;

    let action_table = lua.create_table()?;
    for &action in crate::keymap::ALL_ACTIONS {
        let name = crate::keymap::action_id_name(action);
        action_table.set(name, name)?;
    }
    kojutsu.set("action", action_table)?;

    let scope_table = lua.create_table()?;
    for name in [
        "all",
        "dag",
        "bookmarks",
        "tags",
        "operations",
        "workspaces",
        "evolog",
        "command_log",
        "interdiff",
        "annotate",
    ] {
        scope_table.set(name, name)?;
    }
    kojutsu.set("scope", scope_table)?;

    let phase_table = lua.create_table()?;
    phase_table.set("pre", "pre")?;
    phase_table.set("post", "post")?;
    kojutsu.set("phase", phase_table)?;

    let selection_table = lua.create_table()?;
    for name in ["all", "commit", "file", "line"] {
        selection_table.set(name, name)?;
    }
    kojutsu.set("selection", selection_table)?;

    lua.globals().set("kojutsu", kojutsu)?;
    Ok(())
}

struct PendingRegistration {
    name: CompactString,
    desc: CompactString,
    scope: String,
    key: Option<String>,
    seq: Option<String>,
    group: String,
    selection: String,
    source: String,
    callback_key: mlua::RegistryKey,
}

enum PendingBinding {
    Bind {
        action: String,
        scope: String,
        key: Option<String>,
        seq: Option<String>,
        desc: Option<String>,
        warn_shadow: bool,
    },
    Unbind {
        scope: String,
        key: Option<String>,
        seq: Option<String>,
    },
}

#[derive(Default)]
enum PendingAction {
    #[default]
    None,
    Refresh,
    SetRevset(String),
    Interactive(Vec<String>),
}

fn lua_source_info(lua: &Lua, func: &mlua::Function) -> String {
    let result: mlua::Result<String> = (|| {
        let debug: mlua::Table = lua.globals().get("debug")?;
        let getinfo: mlua::Function = debug.get("getinfo")?;
        let info: mlua::Table = getinfo.call::<mlua::Table>((func.clone(), "Sl"))?;
        let raw: String = info.get("short_src").unwrap_or_else(|_| "?".into());
        let line: i64 = info.get("linedefined").unwrap_or(0);
        let unwrapped = raw
            .strip_prefix("[string \"")
            .and_then(|s| s.strip_suffix("\"]"))
            .unwrap_or(&raw);
        let source = crate::theme::kojutsu_config_dir()
            .and_then(|d| {
                let prefix = format!("{}/", d.display());
                unwrapped.strip_prefix(&prefix).map(|s| s.to_string())
            })
            .unwrap_or_else(|| unwrapped.to_string());
        Ok(format!("{source}:{line}"))
    })();
    result.unwrap_or_else(|_| "?".into())
}

fn hook_matches(lua: &Lua, pattern: &str, action_name: &str) -> mlua::Result<bool> {
    if pattern == action_name {
        return Ok(true);
    }
    let string_mod: mlua::Table = lua.globals().get("string")?;
    let find_fn: mlua::Function = string_mod.get("find")?;
    let result = find_fn.call::<mlua::Value>((action_name, format!("^{pattern}$")))?;
    Ok(!matches!(result, mlua::Value::Nil))
}

fn table_to_string_vec(table: &mlua::Table) -> Vec<String> {
    (1..=table.raw_len())
        .filter_map(|i| table.raw_get(i).ok())
        .collect()
}

fn view_name(view: crate::app::ActiveView) -> &'static str {
    use crate::app::ActiveView;
    match view {
        ActiveView::Dag => "dag",
        ActiveView::Bookmarks => "bookmarks",
        ActiveView::Tags => "tags",
        ActiveView::Operations => "operations",
        ActiveView::Workspaces => "workspaces",
        ActiveView::Evolog => "evolog",
        ActiveView::CommandLog => "command_log",
        ActiveView::Interdiff => "interdiff",
        ActiveView::Annotate => "annotate",
    }
}

fn parse_scope(s: &str) -> Scope {
    use crate::app::ActiveView;
    match s {
        "all" => Scope::All,
        "dag" => Scope::Views(smallvec::smallvec![ActiveView::Dag]),
        "bookmarks" => Scope::Views(smallvec::smallvec![ActiveView::Bookmarks]),
        "tags" => Scope::Views(smallvec::smallvec![ActiveView::Tags]),
        "operations" => Scope::Views(smallvec::smallvec![ActiveView::Operations]),
        "workspaces" => Scope::Views(smallvec::smallvec![ActiveView::Workspaces]),
        "evolog" => Scope::Views(smallvec::smallvec![ActiveView::Evolog]),
        "command_log" => Scope::Views(smallvec::smallvec![ActiveView::CommandLog]),
        "interdiff" => Scope::Views(smallvec::smallvec![ActiveView::Interdiff]),
        "annotate" => Scope::Views(smallvec::smallvec![ActiveView::Annotate]),
        _ => Scope::All,
    }
}
