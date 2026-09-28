use std::cell::RefCell;

use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::CommandFlags;

use super::{LogGroup, LogPhase, LuaEngine, LuaState, PendingAction, lua_state};

impl LuaEngine {
    pub(super) fn register_persistent_functions(&self) -> mlua::Result<()> {
        register_globals(&self.lua)?;

        let repo_path = self.repo_path.clone();
        // Runs the command inline and returns its result, leaving the UI
        // untouched. Two callers: contexts that cannot yield (top-level
        // init.lua code), and `quiet` calls: see `kojutsu.jj` below.
        let jj_sync_fn = self.lua.create_function(move |lua, args: mlua::Table| {
            let result = JJCommand {
                kind: JJCommandKind::Raw {
                    args: super::string_list(&args)?,
                },
                flags: CommandFlags::empty(),
            }
            .run(&repo_path);
            super::jj_result_table(lua, &result)
        })?;

        let repo_path = self.repo_path.clone();
        let exec_sync_fn = self.lua.create_function(move |lua, argv: mlua::Table| {
            let result = super::exec_command(&argv)?.run(&repo_path);
            super::jj_result_table(lua, &result)
        })?;

        let jj_interactive_fn = self.lua.create_function(|lua, args: mlua::Table| {
            let cmd_args: Vec<String> = (1..=args.raw_len())
                .map(|i| args.raw_get(i))
                .collect::<mlua::Result<_>>()?;
            lua_state!(lua)
                .borrow_mut()
                .pending_actions
                .push(PendingAction::Interactive(cmd_args));
            Ok(())
        })?;

        let refresh_fn = self.lua.create_function(|lua, ()| {
            let cell = lua_state!(lua);
            let mut state = cell.borrow_mut();
            // One reload per command is enough.
            if !state
                .pending_actions
                .iter()
                .any(|a| matches!(a, PendingAction::Refresh))
            {
                state.pending_actions.push(PendingAction::Refresh);
            }
            Ok(())
        })?;

        let set_revset_fn = self.lua.create_function(|lua, revset: String| {
            lua_state!(lua)
                .borrow_mut()
                .pending_actions
                .push(PendingAction::SetRevset(revset));
            Ok(())
        })?;

        let switch_view_fn = self.lua.create_function(|lua, view: String| {
            let av: crate::app::ActiveView = view
                .parse()
                .map_err(|_| mlua::Error::external(format!("unknown view `{view}`")))?;
            lua_state!(lua)
                .borrow_mut()
                .pending_actions
                .push(PendingAction::SwitchView(av));
            Ok(())
        })?;

        let jump_to_fn = self.lua.create_function(|lua, change_id: String| {
            lua_state!(lua)
                .borrow_mut()
                .pending_actions
                .push(PendingAction::JumpTo(crate::types::RevisionArg::new(
                    change_id,
                )));
            Ok(())
        })?;

        let dispatch_fn = self.lua.create_function(|lua, name: String| {
            let action: crate::keymap::AppAction = name.parse().map_err(|_| {
                mlua::Error::external(format!("kojutsu.dispatch: unknown action `{name}`"))
            })?;
            lua_state!(lua)
                .borrow_mut()
                .pending_actions
                .push(PendingAction::Dispatch(action));
            Ok(())
        })?;

        let status_fn = self.lua.create_function(|lua, msg: String| {
            lua_state!(lua).borrow_mut().pending_status =
                Some((msg, crate::app::StatusLevel::Info));
            Ok(())
        })?;

        let error_fn = self.lua.create_function(|lua, msg: String| {
            lua_state!(lua).borrow_mut().pending_status =
                Some((msg, crate::app::StatusLevel::Error));
            Ok(())
        })?;

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

        let strip_ansi_fn = self
            .lua
            .create_function(|_lua, text: String| Ok(crate::app::strip_ansi(&text)))?;

        let log_fn = self.lua.create_function(|lua, msg: String| {
            lua_state!(lua).borrow_mut().pending_logs.push(msg);
            Ok(())
        })?;

        let collect_logs_fn =
            self.lua
                .create_function(|lua, (header, phase_str): (String, String)| {
                    let cell = lua_state!(lua);
                    let mut state = cell.borrow_mut();
                    let messages: Vec<String> = state.pending_logs.drain(..).collect();
                    if !messages.is_empty() {
                        let phase = match phase_str.as_str() {
                            "pre" => LogPhase::Pre,
                            "post" => LogPhase::Post,
                            _ => LogPhase::Command,
                        };
                        state.log_groups.push(LogGroup {
                            header,
                            phase,
                            messages,
                        });
                    }
                    Ok(())
                })?;

        self.lua.load(r#"
            -- A command normally runs through the pipeline a keypress would
            -- use: running overlay, live output, Esc to cancel, an entry in
            -- the command log. `quiet` trades all of that for running inline
            -- and just handing back the result, which is what a read-only
            -- probe wants: a scan of twenty submodules should not leave
            -- sixty rows in the command log. The catch is that inline means
            -- inline: nothing redraws and nothing cancels until it returns,
            -- so keep quiet calls short.
            function kojutsu.jj(args, opts)
                args = args or {}
                if (opts and opts.quiet) or not coroutine.isyieldable() then
                    return kojutsu._jj_sync(args)
                end
                return coroutine.yield({type = "jj", args = args})
            end
            -- Same two paths, but the program is whatever argv[1] names. No
            -- shell is involved, so the arguments arrive exactly as written.
            function kojutsu.exec(argv, opts)
                argv = argv or {}
                if (opts and opts.quiet) or not coroutine.isyieldable() then
                    return kojutsu._exec_sync(argv)
                end
                return coroutine.yield({type = "exec", args = argv})
            end
            function kojutsu.ui.input(prompt, default)
                return coroutine.yield({type = "input", prompt = prompt or "", default = default or ""})
            end
            function kojutsu.ui.choose(title, items, multi)
                return coroutine.yield({type = "choose", title = title or "", items = items or {}, multi = multi or false})
            end
            function kojutsu.ui.confirm(prompt)
                return kojutsu.ui.choose(prompt or "confirm?", {"yes", "no"}) == "yes"
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
        kojutsu.set("_jj_sync", jj_sync_fn)?;
        kojutsu.set("_exec_sync", exec_sync_fn)?;
        kojutsu.set("jj_interactive", jj_interactive_fn)?;
        kojutsu.set("dispatch", dispatch_fn)?;
        kojutsu.set("log", log_fn)?;
        kojutsu.set("copy", copy_fn)?;
        kojutsu.set("strip_ansi", strip_ansi_fn)?;
        kojutsu.set("_collect_logs", collect_logs_fn)?;

        let ui: mlua::Table = kojutsu.get("ui")?;
        ui.set("status", status_fn)?;
        ui.set("error", error_fn)?;

        let nav: mlua::Table = kojutsu.get("nav")?;
        nav.set("refresh", refresh_fn)?;
        nav.set("set_revset", set_revset_fn)?;
        nav.set("switch_view", switch_view_fn)?;
        nav.set("jump_to", jump_to_fn)?;

        Ok(())
    }
}

fn register_globals(lua: &mlua::Lua) -> mlua::Result<()> {
    let kojutsu = lua.create_table()?;
    kojutsu.set("ui", lua.create_table()?)?;
    kojutsu.set("nav", lua.create_table()?)?;

    let action_table = lua.create_table()?;
    for action in <crate::keymap::AppAction as strum::IntoEnumIterator>::iter() {
        let name = action.id_name();
        action_table.set(name, name)?;
    }
    kojutsu.set("action", action_table)?;

    let scope_table = lua.create_table()?;
    for name in super::helpers::scope_names() {
        scope_table.set(name.clone(), name)?;
    }
    kojutsu.set("scope", scope_table)?;

    let phase_table = lua.create_table()?;
    phase_table.set("pre", "pre")?;
    phase_table.set("post", "post")?;
    kojutsu.set("phase", phase_table)?;

    let selection_table = lua.create_table()?;
    for name in super::helpers::selection_names() {
        selection_table.set(name.clone(), name)?;
    }
    kojutsu.set("selection", selection_table)?;

    lua.globals().set("kojutsu", kojutsu)?;
    Ok(())
}
