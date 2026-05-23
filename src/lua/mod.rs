use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use compact_str::CompactString;
use mlua::Lua;

use crate::app::App;
use crate::input::Action;
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{
    ActionRegistry, BindTarget, BindingSpec, CommandFlags, HelpGroup, Scope, SelectionKindSet,
};

struct LuaCommand {
    callback: mlua::RegistryKey,
}

struct LuaHook {
    phase: HookPhase,
    callback: mlua::RegistryKey,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookPhase {
    Pre,
    Post,
}

pub enum HookResult {
    Proceed,
    Cancel,
}

pub struct LuaEngine {
    lua: Lua,
    commands: Vec<LuaCommand>,
    hooks: Vec<(CompactString, LuaHook)>,
    extra_bindings: Vec<BindingSpec>,
    pub init_error: Option<String>,
    repo_path: PathBuf,
}

impl LuaEngine {
    pub fn new(repo_path: &Path, registry: &mut ActionRegistry) -> Self {
        let lua = Lua::new();
        let mut engine = LuaEngine {
            lua,
            commands: Vec::new(),
            hooks: Vec::new(),
            extra_bindings: Vec::new(),
            init_error: None,
            repo_path: repo_path.to_path_buf(),
        };

        if let Err(e) = register_globals(&engine.lua) {
            engine.init_error = Some(format!("failed to register Lua API: {e}"));
            return engine;
        }

        engine.load_init_script(registry);
        engine
    }

    pub fn take_extra_bindings(&mut self) -> Vec<BindingSpec> {
        std::mem::take(&mut self.extra_bindings)
    }

    pub fn run_pre_hooks(&self, action_name: &str, app: &App) -> HookResult {
        for (name, hook) in &self.hooks {
            if name.as_str() != action_name || hook.phase != HookPhase::Pre {
                continue;
            }
            let result: mlua::Result<bool> = (|| {
                let func: mlua::Function = self.lua.registry_value(&hook.callback)?;
                let ctx = self.lua.create_table()?;
                if let Some(id) = app.selected_change_id() {
                    ctx.set("change_id", id.as_str())?;
                }
                let change_ids = app.selected_change_ids();
                let ids_table = self.lua.create_table()?;
                for (i, id) in change_ids.iter().enumerate() {
                    ids_table.raw_set(i + 1, id.as_str())?;
                }
                ctx.set("change_ids", ids_table)?;
                ctx.set("view", view_name(app.active_view))?;
                let val = func.call::<mlua::Value>(ctx)?;
                match val {
                    mlua::Value::Boolean(false) => Ok(false),
                    _ => Ok(true),
                }
            })();
            match result {
                Ok(false) => return HookResult::Cancel,
                Ok(true) => {}
                Err(e) => {
                    tracing::warn!("pre-hook error for {action_name}: {e}");
                }
            }
        }
        HookResult::Proceed
    }

    pub fn run_post_hooks(&self, action_name: &str, app: &App, success: bool) {
        for (name, hook) in &self.hooks {
            if name.as_str() != action_name || hook.phase != HookPhase::Post {
                continue;
            }
            let result: mlua::Result<()> = (|| {
                let func: mlua::Function = self.lua.registry_value(&hook.callback)?;
                let ctx = self.lua.create_table()?;
                if let Some(id) = app.selected_change_id() {
                    ctx.set("change_id", id.as_str())?;
                }
                ctx.set("view", view_name(app.active_view))?;
                let result_table = self.lua.create_table()?;
                result_table.set("ok", success)?;
                func.call::<()>((ctx, result_table))?;
                Ok(())
            })();
            if let Err(e) = result {
                tracing::warn!("post-hook error for {action_name}: {e}");
            }
        }
    }

    pub fn execute_command(&self, id: u16, app: &mut App, flags: CommandFlags) -> Action {
        let cmd = &self.commands[id as usize];
        let pending = Rc::new(RefCell::new(PendingAction::None));

        self.lua
            .scope(|scope| {
                let pending_clone = pending.clone();
                let repo_path = self.repo_path.clone();

                let jj_fn = scope.create_function(move |lua, args: mlua::Table| {
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

                let pending_interactive = pending_clone.clone();
                let jj_interactive_fn = scope.create_function(move |_lua, args: mlua::Table| {
                    let cmd_args: Vec<String> = (1..=args.raw_len())
                        .map(|i| args.raw_get(i))
                        .collect::<mlua::Result<_>>()?;
                    *pending_interactive.borrow_mut() = PendingAction::Interactive(cmd_args);
                    Ok(())
                })?;

                let ctx_table = self.lua.create_table()?;
                let change_id = app.selected_change_id();
                let commit_id = change_id.as_ref().and_then(|cid| {
                    app.commit_id_for_change(cid)
                        .map(|id| CompactString::from(id.as_str()))
                });
                match &change_id {
                    Some(id) => ctx_table.set("change_id", id.as_str())?,
                    None => ctx_table.set("change_id", mlua::Value::Nil)?,
                }
                match &commit_id {
                    Some(id) => ctx_table.set("commit_id", id.as_str())?,
                    None => ctx_table.set("commit_id", mlua::Value::Nil)?,
                }
                if let Some(desc) = app.selected_description() {
                    ctx_table.set("description", desc.to_string())?;
                }
                if let Some(bookmarks) = app.selected_bookmarks() {
                    let bm_table = self.lua.create_table()?;
                    for (i, b) in bookmarks.iter().enumerate() {
                        bm_table.raw_set(i + 1, b.name.as_str())?;
                    }
                    ctx_table.set("bookmarks", bm_table)?;
                }
                ctx_table.set("view", view_name(app.active_view))?;
                ctx_table.set("revset", app.revset.current.as_str())?;
                ctx_table.set("repo_root", app.repo_root.as_str())?;

                let pending_refresh = pending_clone.clone();
                let refresh_fn = scope.create_function(move |_lua, ()| {
                    let mut p = pending_refresh.borrow_mut();
                    if matches!(*p, PendingAction::None) {
                        *p = PendingAction::Refresh;
                    }
                    Ok(())
                })?;

                let pending_revset = pending_clone.clone();
                let set_revset_fn = scope.create_function(move |_lua, revset: String| {
                    *pending_revset.borrow_mut() = PendingAction::SetRevset(revset);
                    Ok(())
                })?;

                let kojutsu: mlua::Table = self.lua.globals().get("kojutsu")?;
                kojutsu.set("jj", jj_fn)?;
                kojutsu.set("jj_interactive", jj_interactive_fn)?;
                kojutsu.set("ctx", ctx_table)?;

                let ui: mlua::Table = kojutsu.get("ui")?;
                ui.set("flash", scope.create_function(|_lua, _msg: String| Ok(()))?)?;

                let nav: mlua::Table = kojutsu.get("nav")?;
                nav.set("refresh", refresh_fn)?;
                nav.set("set_revset", set_revset_fn)?;

                let func: mlua::Function = self.lua.registry_value(&cmd.callback)?;
                if let Err(e) = func.call::<()>(()) {
                    app.set_error(format!("lua: {e}"));
                }

                Ok(())
            })
            .unwrap_or_else(|e| {
                app.set_error(format!("lua scope error: {e}"));
            });

        match Rc::try_unwrap(pending).unwrap_or_default().into_inner() {
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

    fn load_init_script(&mut self, registry: &mut ActionRegistry) {
        let Some(config_dir) = dirs::config_dir() else {
            return;
        };
        let init_path = config_dir.join("kojutsu/init.lua");
        if !init_path.exists() {
            return;
        }

        let lua_dir = config_dir.join("kojutsu/lua");
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

                    let callback_key = lua.create_registry_value(func)?;

                    reg_clone.borrow_mut().push(PendingRegistration {
                        desc: desc.into(),
                        scope,
                        key,
                        seq,
                        group,
                        selection,
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
                            "kojutsu.hook requires 3 args: action_name, phase, fn",
                        ));
                    }
                    let mut iter = args.into_iter();
                    let action_name: String = mlua::FromLua::from_lua(iter.next().unwrap(), lua)?;
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
                    let callback = lua.create_registry_value(func)?;
                    hooks_clone
                        .borrow_mut()
                        .push((action_name.into(), LuaHook { phase, callback }));
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
            kojutsu.set("unbind", unbind_fn)?;
            Ok(())
        })() {
            self.init_error = Some(format!("{e}"));
            return;
        }

        if let Err(e) = self
            .lua
            .load(&source)
            .set_name(&init_path.display().to_string())
            .exec()
        {
            self.init_error = Some(format!("{e}"));
            return;
        }

        let registrations = Rc::try_unwrap(reg_commands)
            .unwrap_or_default()
            .into_inner();

        for reg in registrations {
            let selection_support = match reg.selection.as_str() {
                "commit" => SelectionKindSet::COMMIT,
                "file" => SelectionKindSet::FILE,
                "line" => SelectionKindSet::LINE,
                _ => SelectionKindSet::ALL,
            };
            let action_id = registry.register_lua(selection_support, false, false);

            self.commands.push(LuaCommand {
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
            .extend(Rc::try_unwrap(reg_hooks).unwrap_or_default().into_inner());

        // Process bind/unbind overrides.
        let bindings = Rc::try_unwrap(reg_bindings)
            .unwrap_or_default()
            .into_inner();
        for binding in bindings {
            match binding {
                PendingBinding::Bind {
                    action,
                    scope,
                    key,
                    seq,
                    desc,
                } => {
                    let Some(action_id) = registry.find_by_name(&action) else {
                        tracing::warn!("kojutsu.bind: unknown action '{action}'");
                        continue;
                    };
                    let Some(keys) = parse_keys(key.as_ref(), seq.as_ref()) else {
                        continue;
                    };
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

fn register_globals(lua: &Lua) -> mlua::Result<()> {
    let kojutsu = lua.create_table()?;
    kojutsu.set("ui", lua.create_table()?)?;
    kojutsu.set("nav", lua.create_table()?)?;
    lua.globals().set("kojutsu", kojutsu)?;
    Ok(())
}

struct PendingRegistration {
    desc: CompactString,
    scope: String,
    key: Option<String>,
    seq: Option<String>,
    group: String,
    selection: String,
    callback_key: mlua::RegistryKey,
}

enum PendingBinding {
    Bind {
        action: String,
        scope: String,
        key: Option<String>,
        seq: Option<String>,
        desc: Option<String>,
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
