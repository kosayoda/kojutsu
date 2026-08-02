use std::cell::RefCell;
use std::rc::Rc;

use compact_str::CompactString;

use crate::keymap::{ActionRegistry, BindTarget, BindingSpec, HelpGroup, SelectionKindSet};

use super::{
    HookPhase, LuaCommand, LuaEngine, LuaHook,
    helpers::{
        hook_action_set, lua_source_info, parse_keys, parse_scope, shadows_default,
        table_to_string_vec,
    },
};

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
    Prefix {
        scope: String,
        key: Option<String>,
        seq: Option<String>,
        label: String,
    },
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

        let reg_commands: Rc<RefCell<Vec<PendingRegistration>>> = Rc::new(RefCell::new(Vec::new()));
        let reg_hooks: Rc<RefCell<Vec<LuaHook>>> = Rc::new(RefCell::new(Vec::new()));
        let reg_bindings: Rc<RefCell<Vec<PendingBinding>>> = Rc::new(RefCell::new(Vec::new()));

        if let Err(e) = (|| -> mlua::Result<()> {
            let reg_clone = reg_commands.clone();
            let command_fn = self.lua.create_function(
                move |lua, (name, func, opts): (String, mlua::Function, mlua::Table)| {
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
                },
            )?;

            let hooks_clone = reg_hooks.clone();
            let hook_fn = self.lua.create_function(
                move |lua, (first, phase_str, func): (mlua::Value, String, mlua::Function)| {
                    let action_names: Vec<String> = match first {
                        mlua::Value::String(s) => vec![s.to_str()?.to_string()],
                        mlua::Value::Table(t) => table_to_string_vec(&t),
                        _ => {
                            return Err(mlua::Error::external(
                                "first argument must be an action name or table of action names",
                            ));
                        }
                    };
                    let phase = match phase_str.as_str() {
                        "pre" => HookPhase::Pre,
                        "post" => HookPhase::Post,
                        other => {
                            return Err(mlua::Error::external(format!(
                                "invalid hook phase: {other} (expected 'pre' or 'post')"
                            )));
                        }
                    };
                    let source = lua_source_info(lua, &func);
                    let mut hooks = hooks_clone.borrow_mut();
                    for pattern in action_names {
                        let callback = lua.create_registry_value(func.clone())?;
                        hooks.push(LuaHook {
                            phase,
                            callback,
                            source: source.clone(),
                            actions: hook_action_set(lua, &pattern),
                        });
                    }
                    Ok(())
                },
            )?;

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

            let prefix_bindings_clone = reg_bindings.clone();
            let prefix_fn = self.lua.create_function(move |_lua, opts: mlua::Table| {
                let label: String = opts.get("label")?;
                let scope: String = opts.get::<String>("scope").unwrap_or_else(|_| "all".into());
                let key: Option<String> = opts.get("key").ok();
                let seq: Option<String> = opts.get("seq").ok();
                prefix_bindings_clone
                    .borrow_mut()
                    .push(PendingBinding::Prefix {
                        scope,
                        key,
                        seq,
                        label,
                    });
                Ok(())
            })?;

            let kojutsu: mlua::Table = self.lua.globals().get("kojutsu")?;
            kojutsu.set("command", command_fn)?;
            kojutsu.set("hook", hook_fn)?;
            kojutsu.set("bind", bind_fn)?;
            kojutsu.set("rebind", rebind_fn)?;
            kojutsu.set("unbind", unbind_fn)?;
            kojutsu.set("prefix", prefix_fn)?;
            super::config::publish(&self.lua, config)?;
            Ok(())
        })() {
            self.init_error = Some(format!("{e}"));
            return;
        }

        let exec_result = self
            .lua
            .load(&source)
            .set_name(init_path.display().to_string())
            .exec();

        // Read the config back even when the script failed partway: the
        // assignments that did run are as good as any that ran on success.
        let config_result = super::config::read_back(&self.lua, config);

        if let Err(e) = exec_result {
            self.init_error = Some(format!("{e}"));
            return;
        }
        if let Err(e) = config_result {
            self.init_error = Some(e);
            return;
        }

        let registrations = std::mem::take(&mut *reg_commands.borrow_mut());

        for reg in registrations {
            let selection_support = if reg.selection == "all" {
                SelectionKindSet::ALL
            } else if let Ok(kind) = reg.selection.parse::<crate::types::SelectionKind>() {
                kind.as_bitset()
            } else {
                let valid: Vec<String> = super::helpers::selection_names().collect();
                tracing::warn!(
                    "kojutsu.command '{}': unknown selection '{}', defaulting to 'all' (valid: {})",
                    reg.name,
                    reg.selection,
                    valid.join(", ")
                );
                SelectionKindSet::ALL
            };
            let action_id = registry.register_lua(selection_support);

            self.commands.push(LuaCommand {
                name: reg.name.clone(),
                source: reg.source.clone(),
                callback: reg.callback_key,
            });

            let help_group = match reg.group.parse::<HelpGroup>() {
                Ok(group) => group,
                Err(_) => {
                    tracing::warn!(
                        "kojutsu.command '{}': unknown group '{}', defaulting to 'commands'",
                        reg.name,
                        reg.group
                    );
                    HelpGroup::Commands
                }
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
                PendingBinding::Prefix {
                    scope,
                    key,
                    seq,
                    label,
                } => {
                    let Some(keys) = parse_keys(key.as_ref(), seq.as_ref()) else {
                        continue;
                    };
                    self.extra_bindings.push(BindingSpec {
                        keys,
                        target: BindTarget::Prefix {
                            label: label.into(),
                            group: HelpGroup::Commands,
                        },
                        scope: parse_scope(&scope),
                    });
                }
            }
        }
    }
}
