use strum::IntoEnumIterator as _;

use crate::keymap::{BindTarget, BindingSpec, Scope};

/// Valid `scope` option values: `all` plus every view name.
pub(super) fn scope_names() -> impl Iterator<Item = String> {
    std::iter::once("all".to_string()).chain(crate::app::ActiveView::iter().map(|v| v.to_string()))
}

/// Valid `selection` option values: `all` plus every selection kind.
pub(super) fn selection_names() -> impl Iterator<Item = String> {
    std::iter::once("all".to_string())
        .chain(crate::types::SelectionKind::iter().map(|k| k.to_string()))
}

pub(super) fn shadows_default(
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

pub(super) fn parse_keys(
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
    for action in <crate::keymap::AppAction as strum::IntoEnumIterator>::iter() {
        let name = crate::keymap::action_id_name(action);
        writeln!(out, "---@field {name} string").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuScope").unwrap();
    for name in scope_names() {
        writeln!(out, "---@field {name} string").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuPhase").unwrap();
    writeln!(out, "---@field pre string").unwrap();
    writeln!(out, "---@field post string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuSelection").unwrap();
    for name in selection_names() {
        writeln!(out, "---@field {name} string").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuCtx").unwrap();
    writeln!(out, "---@field change_id string?").unwrap();
    writeln!(out, "---@field commit_id string?").unwrap();
    writeln!(out, "---@field change_ids string[]").unwrap();
    writeln!(out, "---@field description string?").unwrap();
    writeln!(out, "---@field bookmarks string[]?").unwrap();
    writeln!(out, "---@field tags string[]?").unwrap();
    writeln!(out, "---@field file_path string?").unwrap();
    writeln!(out, "---@field is_working_copy boolean").unwrap();
    writeln!(out, "---@field is_empty boolean").unwrap();
    writeln!(out, "---@field has_conflict boolean").unwrap();
    writeln!(out, "---@field view string").unwrap();
    writeln!(out, "---@field revset string").unwrap();
    writeln!(out, "---@field repo_root string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class JJResult").unwrap();
    writeln!(out, "---@field ok boolean").unwrap();
    writeln!(out, "---@field output string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuUi").unwrap();
    writeln!(out, "---@field status fun(msg: string)").unwrap();
    writeln!(out, "---@field confirm fun(prompt: string?): boolean").unwrap();
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
    writeln!(out, "---@field switch_view fun(view: string)").unwrap();
    writeln!(out, "---@field jump_to fun(change_id: string)").unwrap();
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

    writeln!(out, "---@class PrefixOpts").unwrap();
    writeln!(out, "---@field label string").unwrap();
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
    writeln!(out, "---@field prefix fun(opts: PrefixOpts)").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "---@type Kojutsu").unwrap();
    writeln!(out, "---@diagnostic disable-next-line: missing-fields").unwrap();
    writeln!(out, "kojutsu = {{}}").unwrap();

    out
}

pub(super) fn lua_source_info(lua: &mlua::Lua, func: &mlua::Function) -> String {
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

pub(super) fn hook_matches(
    lua: &mlua::Lua,
    pattern: &str,
    action_name: &str,
) -> mlua::Result<bool> {
    if pattern == action_name {
        return Ok(true);
    }
    let string_mod: mlua::Table = lua.globals().get("string")?;
    let find_fn: mlua::Function = string_mod.get("find")?;
    let result = find_fn.call::<mlua::Value>((action_name, format!("^{pattern}$")))?;
    Ok(!matches!(result, mlua::Value::Nil))
}

pub(super) fn table_to_string_vec(table: &mlua::Table) -> Vec<String> {
    (1..=table.raw_len())
        .filter_map(|i| table.raw_get(i).ok())
        .collect()
}

pub(super) fn parse_scope(s: &str) -> crate::keymap::Scope {
    if s == "all" {
        return crate::keymap::Scope::All;
    }
    match s.parse::<crate::app::ActiveView>() {
        Ok(view) => crate::keymap::Scope::Views(smallvec::smallvec![view]),
        Err(_) => {
            let valid: Vec<String> = scope_names().collect();
            tracing::warn!(
                "unknown scope '{s}', defaulting to 'all' (valid: {})",
                valid.join(", ")
            );
            crate::keymap::Scope::All
        }
    }
}
