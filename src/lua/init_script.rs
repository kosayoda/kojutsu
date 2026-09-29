use std::cell::RefCell;
use std::rc::Rc;

use compact_str::CompactString;
use smallvec::SmallVec;

use crate::keymap::{
    ActionRegistry, AppAction, BindTarget, BindingSpec, HelpGroup, Scope, SelectionKindSet,
};
use crate::types::{ActiveView, SelectionKind};

use super::helpers::{hook_action_set, scope_names, selection_names, shadows_default};
use super::{HookPhase, LuaCommand, LuaEngine, LuaHook};

/// Where in the user's Lua a registration was made, as `file:line`.
type Location = String;

type Keys = SmallVec<[keymap_parser::Node; 3]>;

/// A key or key sequence and the views it applies in.
struct KeySpec {
    keys: Keys,
    scope: Scope,
}

struct PendingCommand {
    name: CompactString,
    desc: CompactString,
    selection: SelectionKindSet,
    group: HelpGroup,
    keys: Option<KeySpec>,
    source: String,
    callback: mlua::RegistryKey,
}

enum PendingBinding {
    Bind {
        action: AppAction,
        keys: KeySpec,
        desc: Option<String>,
        /// `rebind` says overriding a default is intended; `bind` warns.
        warn_shadow: bool,
        at: Location,
    },
    Unbind {
        keys: KeySpec,
    },
    Prefix {
        keys: KeySpec,
        label: String,
    },
}

/// What `init.lua` registered, checked as each call was made. Problems are
/// collected as warnings naming where they were made, and the registration
/// they concern is skipped: one typo shouldn't stop the rest of the file.
#[derive(Default)]
struct Registrations {
    commands: Vec<PendingCommand>,
    hooks: Vec<LuaHook>,
    bindings: Vec<PendingBinding>,
    warnings: Vec<String>,
}

type Shared = Rc<RefCell<Registrations>>;

impl Registrations {
    /// A problem that makes the registration unsafe to keep: it is skipped.
    fn warn(&mut self, at: &str, api: &str, problem: impl std::fmt::Display) {
        self.warnings
            .push(format!("{at}: kojutsu.{api}: {problem}; skipped"));
    }

    /// A problem with a presentational option: the registration is kept with
    /// that option's default.
    fn warn_defaulted(&mut self, at: &str, api: &str, problem: impl std::fmt::Display) {
        self.warnings
            .push(format!("{at}: kojutsu.{api}: {problem}"));
    }
}

/// The `file:line` of the Lua code calling the running Rust function.
fn caller_location(lua: &mlua::Lua) -> Location {
    lua.inspect_stack(1, |debug| {
        let source = debug.source();
        let file = source.short_src.as_deref().unwrap_or("?");
        let line = debug.current_line().unwrap_or(0);
        format!("{}:{line}", super::helpers::short_source(file))
    })
    .unwrap_or_else(|| "?".into())
}

/// A string option, or `None` if absent. Numbers are accepted, as Lua
/// would coerce them; anything else is a mistake.
fn string_opt(opts: &mlua::Table, key: &str) -> Result<Option<String>, String> {
    match opts.get::<mlua::Value>(key) {
        Ok(mlua::Value::Nil) => Ok(None),
        Ok(mlua::Value::String(s)) => Ok(Some(s.to_string_lossy())),
        Ok(mlua::Value::Integer(n)) => Ok(Some(n.to_string())),
        Ok(mlua::Value::Number(n)) => Ok(Some(n.to_string())),
        Ok(other) => Err(format!(
            "`{key}` must be a string, not a {}",
            other.type_name()
        )),
        Err(e) => Err(format!("`{key}`: {e}")),
    }
}

fn required_string(opts: &mlua::Table, key: &str) -> Result<String, String> {
    string_opt(opts, key)?.ok_or_else(|| format!("needs a `{key}`"))
}

/// The options table of a registration call, which may be omitted.
fn opts_table(value: mlua::Value, lua: &mlua::Lua) -> Result<mlua::Table, String> {
    match value {
        mlua::Value::Table(t) => Ok(t),
        mlua::Value::Nil => lua.create_table().map_err(|e| e.to_string()),
        other => Err(format!(
            "options must be a table, not a {}",
            other.type_name()
        )),
    }
}

fn parse_scope(opts: &mlua::Table) -> Result<Scope, String> {
    let Some(name) = string_opt(opts, "scope")? else {
        return Ok(Scope::All);
    };
    if name == "all" {
        return Ok(Scope::All);
    }
    name.parse::<ActiveView>()
        .map(|view| Scope::Views(smallvec::smallvec![view]))
        .map_err(|_| {
            let valid: Vec<String> = scope_names().collect();
            format!("unknown scope `{name}` (valid: {})", valid.join(", "))
        })
}

fn parse_key(key: &str) -> Result<keymap_parser::Node, String> {
    crate::keymap::try_parse_key(key).ok_or_else(|| format!("invalid key `{key}`"))
}

/// The `key` or `seq` option and its `scope`, or `None` if neither key
/// option is given.
fn key_spec(opts: &mlua::Table) -> Result<Option<KeySpec>, String> {
    let keys: Keys = match (string_opt(opts, "key")?, string_opt(opts, "seq")?) {
        (None, None) => return Ok(None),
        (Some(_), Some(_)) => return Err("give `key` or `seq`, not both".into()),
        (Some(key), None) => smallvec::smallvec![parse_key(&key)?],
        (None, Some(seq)) => {
            let keys = seq
                .split_whitespace()
                .map(parse_key)
                .collect::<Result<Keys, _>>()?;
            if keys.is_empty() {
                return Err("`seq` is empty".into());
            }
            keys
        }
    };
    Ok(Some(KeySpec {
        keys,
        scope: parse_scope(opts)?,
    }))
}

fn required_keys(opts: &mlua::Table) -> Result<KeySpec, String> {
    key_spec(opts)?.ok_or_else(|| "needs a `key` or `seq`".into())
}

fn parse_selection(opts: &mlua::Table) -> Result<SelectionKindSet, String> {
    match string_opt(opts, "selection")?.as_deref() {
        None | Some("all") => Ok(SelectionKindSet::ALL),
        Some(name) => name
            .parse::<SelectionKind>()
            .map(|kind| kind.as_bitset())
            .map_err(|_| {
                let valid: Vec<String> = selection_names().collect();
                format!("unknown selection `{name}` (valid: {})", valid.join(", "))
            }),
    }
}

fn parse_group(opts: &mlua::Table) -> Result<HelpGroup, String> {
    match string_opt(opts, "group")? {
        None => Ok(HelpGroup::Commands),
        Some(name) => name
            .parse::<HelpGroup>()
            .map_err(|_| format!("unknown group `{name}`")),
    }
}

fn parse_action(name: &str) -> Result<AppAction, String> {
    name.parse::<AppAction>()
        .map_err(|_| format!("unknown action `{name}`"))
}

/// `kojutsu.command(name, fn, opts)`.
fn register_command(
    lua: &mlua::Lua,
    shared: &Shared,
    (name, func, opts): (mlua::Value, mlua::Value, mlua::Value),
) -> mlua::Result<()> {
    let at = caller_location(lua);
    // Presentational options fall back to their default rather than cost the
    // command: a description or help group only decides how a key is listed.
    let mut defaulted = Vec::new();
    let parsed = (|| -> Result<PendingCommand, String> {
        let name = match name {
            mlua::Value::String(s) => s.to_string_lossy(),
            other => {
                return Err(format!(
                    "name must be a string, not a {}",
                    other.type_name()
                ));
            }
        };
        let mlua::Value::Function(func) = func else {
            return Err(format!("`{name}` needs a function"));
        };
        let opts = opts_table(opts, lua)?;
        let with_name = |e: String| format!("`{name}`: {e}");
        Ok(PendingCommand {
            desc: string_opt(&opts, "desc")
                .unwrap_or_else(|problem| {
                    defaulted.push(format!("{}; described by its name", with_name(problem)));
                    None
                })
                .unwrap_or_else(|| name.clone())
                .into(),
            selection: parse_selection(&opts).map_err(with_name)?,
            group: parse_group(&opts).unwrap_or_else(|problem| {
                defaulted.push(format!("{}; listed under `commands`", with_name(problem)));
                HelpGroup::Commands
            }),
            keys: key_spec(&opts).map_err(with_name)?,
            source: super::helpers::lua_source_info(lua, &func),
            callback: lua.create_registry_value(func).map_err(|e| e.to_string())?,
            name: name.into(),
        })
    })();
    let mut reg = shared.borrow_mut();
    match parsed {
        Ok(command) => {
            for problem in defaulted {
                reg.warn_defaulted(&at, "command", problem);
            }
            reg.commands.push(command);
        }
        Err(problem) => reg.warn(&at, "command", problem),
    }
    Ok(())
}

/// `kojutsu.hook(actions, phase, fn)`.
fn register_hook(
    lua: &mlua::Lua,
    shared: &Shared,
    (patterns, phase, func): (mlua::Value, mlua::Value, mlua::Value),
) -> mlua::Result<()> {
    let at = caller_location(lua);
    let parsed = (|| -> Result<(Vec<String>, HookPhase, mlua::Function), String> {
        let patterns = match patterns {
            mlua::Value::String(s) => vec![s.to_string_lossy()],
            mlua::Value::Table(t) => super::helpers::table_to_string_vec(&t),
            other => {
                return Err(format!(
                    "first argument must be an action name or a table of them, not a {}",
                    other.type_name()
                ));
            }
        };
        let phase = match &phase {
            mlua::Value::String(s) if s.to_str().is_ok_and(|s| s == "pre") => HookPhase::Pre,
            mlua::Value::String(s) if s.to_str().is_ok_and(|s| s == "post") => HookPhase::Post,
            _ => return Err("phase must be `pre` or `post`".into()),
        };
        let mlua::Value::Function(func) = func else {
            return Err("needs a function".into());
        };
        Ok((patterns, phase, func))
    })();
    let (patterns, phase, func) = match parsed {
        Ok(parsed) => parsed,
        Err(problem) => {
            shared.borrow_mut().warn(&at, "hook", problem);
            return Ok(());
        }
    };
    let source = super::helpers::lua_source_info(lua, &func);
    for pattern in patterns {
        let actions = hook_action_set(lua, &pattern);
        let mut reg = shared.borrow_mut();
        if actions.is_empty() {
            reg.warn(&at, "hook", format!("`{pattern}` matches no action"));
            continue;
        }
        reg.hooks.push(LuaHook {
            phase,
            callback: lua.create_registry_value(func.clone())?,
            source: source.clone(),
            actions,
        });
    }
    Ok(())
}

/// `kojutsu.bind(opts)`, or `kojutsu.rebind(opts)` when `warn_shadow` is off.
fn register_bind(
    lua: &mlua::Lua,
    shared: &Shared,
    opts: mlua::Value,
    warn_shadow: bool,
) -> mlua::Result<()> {
    let at = caller_location(lua);
    let api = if warn_shadow { "bind" } else { "rebind" };
    let mut defaulted = None;
    let parsed = (|| -> Result<PendingBinding, String> {
        let opts = opts_table(opts, lua)?;
        Ok(PendingBinding::Bind {
            action: parse_action(&required_string(&opts, "action")?)?,
            keys: required_keys(&opts)?,
            // Presentational: a bad one falls back to the action's name.
            desc: string_opt(&opts, "desc").unwrap_or_else(|problem| {
                defaulted = Some(format!("{problem}; described by the action's name"));
                None
            }),
            warn_shadow,
            at: at.clone(),
        })
    })();
    if let (Ok(_), Some(problem)) = (&parsed, defaulted) {
        shared.borrow_mut().warn_defaulted(&at, api, problem);
    }
    push_binding(shared, &at, api, parsed);
    Ok(())
}

/// `kojutsu.unbind(opts)`.
fn register_unbind(lua: &mlua::Lua, shared: &Shared, opts: mlua::Value) -> mlua::Result<()> {
    let at = caller_location(lua);
    let parsed = opts_table(opts, lua)
        .and_then(|opts| required_keys(&opts))
        .map(|keys| PendingBinding::Unbind { keys });
    push_binding(shared, &at, "unbind", parsed);
    Ok(())
}

/// `kojutsu.prefix(opts)`.
fn register_prefix(lua: &mlua::Lua, shared: &Shared, opts: mlua::Value) -> mlua::Result<()> {
    let at = caller_location(lua);
    let parsed = (|| -> Result<PendingBinding, String> {
        let opts = opts_table(opts, lua)?;
        Ok(PendingBinding::Prefix {
            label: required_string(&opts, "label")?,
            keys: required_keys(&opts)?,
        })
    })();
    push_binding(shared, &at, "prefix", parsed);
    Ok(())
}

fn push_binding(shared: &Shared, at: &str, api: &str, parsed: Result<PendingBinding, String>) {
    let mut reg = shared.borrow_mut();
    match parsed {
        Ok(binding) => reg.bindings.push(binding),
        Err(problem) => reg.warn(at, api, problem),
    }
}

impl LuaEngine {
    pub(super) fn load_init_script(
        &mut self,
        registry: &mut ActionRegistry,
        default_specs: &[BindingSpec],
        config: &mut crate::theme::Config,
        config_dir: &std::path::Path,
    ) {
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

        let shared: Shared = Rc::default();
        if let Err(e) = self.publish_registration_api(&shared, config) {
            self.init_error = Some(format!("{e}"));
            return;
        }

        let exec_result = self
            .lua
            .load(&source)
            .set_name(init_path.display().to_string())
            .exec();

        // A script that fails partway keeps what it did before the error:
        // config it set and commands, bindings and hooks it registered. The
        // error is still reported, and a reload throws away a runtime that
        // carries one, keeping the running config instead.
        let errors: Vec<String> = [
            exec_result.err().map(|e| e.to_string()),
            match super::config::read(&self.lua) {
                Ok(resolved) => {
                    *config = resolved;
                    None
                }
                Err(e) => Some(e),
            },
        ]
        .into_iter()
        .flatten()
        .collect();
        if !errors.is_empty() {
            self.init_error = Some(errors.join("\n"));
        }

        let Registrations {
            commands,
            hooks,
            bindings,
            mut warnings,
        } = std::mem::take(&mut *shared.borrow_mut());

        for command in commands {
            let id = registry.register_lua(command.selection);
            self.commands.push(LuaCommand {
                name: command.name,
                source: command.source,
                callback: command.callback,
            });
            if let Some(KeySpec { keys, scope }) = command.keys {
                self.extra_bindings.push(BindingSpec {
                    keys,
                    target: BindTarget::Action {
                        id,
                        description: command.desc,
                        group: command.group,
                    },
                    scope,
                });
            }
        }

        self.hooks.extend(hooks);

        for binding in bindings {
            let (KeySpec { keys, scope }, target) = match binding {
                PendingBinding::Bind {
                    action,
                    keys,
                    desc,
                    warn_shadow,
                    at,
                } => {
                    if warn_shadow && shadows_default(default_specs, &keys.keys, &keys.scope) {
                        // Not skipped: overriding a default is allowed, just
                        // worth saying out loud in case it wasn't meant.
                        warnings.push(format!(
                            "{at}: kojutsu.bind: shadows a default binding (use \
                             kojutsu.rebind if that is intended)"
                        ));
                    }
                    let target = BindTarget::Action {
                        id: crate::keymap::ActionId::Builtin(action),
                        description: desc.unwrap_or_else(|| action.id_name().into()).into(),
                        group: HelpGroup::Commands,
                    };
                    (keys, target)
                }
                PendingBinding::Unbind { keys } => (keys, BindTarget::Unbind),
                PendingBinding::Prefix { keys, label } => (
                    keys,
                    BindTarget::Prefix {
                        label: label.into(),
                        group: HelpGroup::Commands,
                    },
                ),
            };
            self.extra_bindings.push(BindingSpec {
                keys,
                target,
                scope,
            });
        }

        self.init_warnings = warnings;
    }

    /// Publish the registration functions and the config table for
    /// `init.lua` to call.
    fn publish_registration_api(
        &self,
        shared: &Shared,
        config: &crate::theme::Config,
    ) -> mlua::Result<()> {
        let lua = &self.lua;
        let kojutsu: mlua::Table = lua.globals().get("kojutsu")?;

        let reg = shared.clone();
        kojutsu.set(
            "command",
            lua.create_function(move |lua, args| register_command(lua, &reg, args))?,
        )?;
        let reg = shared.clone();
        kojutsu.set(
            "hook",
            lua.create_function(move |lua, args| register_hook(lua, &reg, args))?,
        )?;
        let reg = shared.clone();
        kojutsu.set(
            "bind",
            lua.create_function(move |lua, opts| register_bind(lua, &reg, opts, true))?,
        )?;
        let reg = shared.clone();
        kojutsu.set(
            "rebind",
            lua.create_function(move |lua, opts| register_bind(lua, &reg, opts, false))?,
        )?;
        let reg = shared.clone();
        kojutsu.set(
            "unbind",
            lua.create_function(move |lua, opts| register_unbind(lua, &reg, opts))?,
        )?;
        let reg = shared.clone();
        kojutsu.set(
            "prefix",
            lua.create_function(move |lua, opts| register_prefix(lua, &reg, opts))?,
        )?;
        super::config::publish(lua, config)
    }
}
