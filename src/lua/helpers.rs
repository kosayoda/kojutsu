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
    let parse = |k: &str| {
        let node = crate::keymap::try_parse_key(k);
        if node.is_none() {
            tracing::warn!("invalid key `{k}`, skipping binding");
        }
        node
    };
    if let Some(k) = key {
        Some(smallvec::smallvec![parse(k)?])
    } else {
        seq?.split_whitespace().map(parse).collect()
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

    writeln!(
        out,
        "---@alias KojutsuColor string|integer|{{ r: integer, g: integer, b: integer }} \
         a name (\"cyan\", \"dark_gray\", \"bright-white\"), a hex string, an ANSI index, or an RGB table"
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuTheme").unwrap();
    for field in [
        "accent",
        "selection",
        "muted",
        "text",
        "error",
        "warning",
        "added",
        "change_id",
        "commit_id",
        "selection_bg",
        "selection_bg_strong",
        "tag",
        "remote",
        "bookmark",
        "user",
        "workspace",
    ] {
        writeln!(out, "---@field {field} KojutsuColor").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuGlyphs").unwrap();
    for field in ["working_copy", "conflict", "immutable", "merge", "normal"] {
        writeln!(out, "---@field {field} string a single character").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuSearchScopes").unwrap();
    for field in [
        "change_id",
        "commit_id",
        "description",
        "bookmark",
        "author",
        "path",
        "line",
        "tag",
    ] {
        writeln!(out, "---@field {field} boolean").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuPreset").unwrap();
    writeln!(out, "---@field name string").unwrap();
    writeln!(out, "---@field revset string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuRevsets").unwrap();
    writeln!(
        out,
        "---@field presets KojutsuPreset[] keys 1-5 switch between the first five"
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuRun").unwrap();
    writeln!(
        out,
        "---@field presets string[] command lines offered by the `jj run` picker"
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuDiff").unwrap();
    writeln!(out, "---@field max_file_size_mib integer per side").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class KojutsuConfig").unwrap();
    writeln!(out, "---@field theme KojutsuTheme").unwrap();
    writeln!(out, "---@field revsets KojutsuRevsets").unwrap();
    writeln!(out, "---@field run KojutsuRun").unwrap();
    writeln!(out, "---@field date_format string strftime syntax").unwrap();
    writeln!(out, "---@field glyphs KojutsuGlyphs").unwrap();
    writeln!(out, "---@field default_search_scopes KojutsuSearchScopes").unwrap();
    writeln!(out, "---@field tab_width integer").unwrap();
    writeln!(out, "---@field diff KojutsuDiff").unwrap();
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
    writeln!(out, "---@field author_name string?").unwrap();
    writeln!(out, "---@field author_email string?").unwrap();
    writeln!(out, "---@field is_immutable boolean?").unwrap();
    writeln!(out, "---@field is_merge boolean?").unwrap();
    writeln!(out, "---@field parent_change_ids string[]?").unwrap();
    writeln!(out, "---@field view string").unwrap();
    writeln!(out, "---@field revset string").unwrap();
    writeln!(out, "---@field repo_root string").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@alias JJStatus \"ok\"|\"failed\"|\"cancelled\"").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class JJResult").unwrap();
    writeln!(out, "---@field status JJStatus").unwrap();
    writeln!(out, "---@field ok boolean").unwrap();
    writeln!(
        out,
        "---@field output string both pipes interleaved as they arrived, ANSI-colored: for \
         showing, not for parsing"
    )
    .unwrap();
    writeln!(
        out,
        "---@field stdout string the command's answer on its own, ANSI-colored; parse this \
         rather than output, which carries jj's warnings and hints too"
    )
    .unwrap();
    writeln!(
        out,
        "---@field stderr string warnings, hints and errors, ANSI-colored"
    )
    .unwrap();
    writeln!(
        out,
        "---@field code integer? exit code (nil when cancelled or spawn failed)"
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class JjOpts").unwrap();
    writeln!(
        out,
        "---@field quiet boolean? run inline with no overlay, no live output, no command-log \
         entry and no way to cancel"
    )
    .unwrap();
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
        "---@field choose fun(title: string, items: string[], multi: boolean?): string|string[]|nil \
         a single item, or every ticked item when multi is true"
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

    // No stdout/stderr here: a post-hook is handed the finished command's
    // output, which is where the two pipes have already been interleaved.
    writeln!(out, "---@class HookResultTable").unwrap();
    writeln!(out, "---@field status JJStatus").unwrap();
    writeln!(out, "---@field ok boolean").unwrap();
    writeln!(
        out,
        "---@field output string both pipes interleaved, ANSI-colored; pass through \
         kojutsu.strip_ansi to parse it. Unlike JJResult there is no split: the pipes were \
         already merged before a hook sees them"
    )
    .unwrap();
    writeln!(
        out,
        "---@field code integer? exit code (nil when cancelled or spawn failed)"
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "---@class Kojutsu").unwrap();
    writeln!(out, "---@field action KojutsuAction").unwrap();
    writeln!(out, "---@field scope KojutsuScope").unwrap();
    writeln!(out, "---@field phase KojutsuPhase").unwrap();
    writeln!(out, "---@field selection KojutsuSelection").unwrap();
    writeln!(
        out,
        "---@field config KojutsuConfig assigning a section replaces it, and omitted keys \
         fall back to their defaults; writes from a command or hook take effect when it returns"
    )
    .unwrap();
    writeln!(out, "---@field ui KojutsuUi").unwrap();
    writeln!(out, "---@field nav KojutsuNav").unwrap();
    writeln!(
        out,
        "---@field jj fun(args: string[], opts: JjOpts?): JJResult"
    )
    .unwrap();
    writeln!(
        out,
        "---@field exec fun(argv: string[], opts: JjOpts?): JJResult run a program in the \
         workspace root, named by argv[1]. Spawned directly, so there is no shell to do \
         quoting, globbing or redirection for you, and no shell to reinterpret an argument \
         either. Never take argv[1] from the repository being viewed: opening a clone should \
         not run what the clone asks for"
    )
    .unwrap();
    writeln!(out, "---@field jj_interactive fun(args: string[])").unwrap();
    writeln!(
        out,
        "---@field dispatch fun(action: string) run a builtin action after the command completes (fire-and-forget; skips the action's pre-hooks)"
    )
    .unwrap();
    writeln!(out, "---@field log fun(msg: string)").unwrap();
    writeln!(out, "---@field copy fun(text: string)").unwrap();
    writeln!(
        out,
        "---@field strip_ansi fun(text: string): string drop ANSI styling, e.g. before parsing a JJResult.output"
    )
    .unwrap();
    writeln!(
        out,
        "---@field command fun(name: string, fn: fun(ctx: KojutsuCtx), opts: CommandOpts)"
    )
    .unwrap();
    writeln!(
        out,
        "---@field hook fun(action: string|string[], phase: string, fn: fun(ctx: KojutsuCtx, result: HookResultTable?): boolean?)"
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

fn hook_matches(lua: &mlua::Lua, pattern: &str, action_name: &str) -> mlua::Result<bool> {
    if pattern == action_name {
        return Ok(true);
    }
    let string_mod: mlua::Table = lua.globals().get("string")?;
    let find_fn: mlua::Function = string_mod.get("find")?;
    let result = find_fn.call::<mlua::Value>((action_name, format!("^{pattern}$")))?;
    Ok(!matches!(result, mlua::Value::Nil))
}

/// Evaluate a hook pattern against every action name once, at registration.
/// Warns when the pattern matches nothing (likely a typo).
pub(super) fn hook_action_set(
    lua: &mlua::Lua,
    pattern: &str,
) -> std::collections::HashSet<&'static str> {
    let actions: std::collections::HashSet<&'static str> = crate::keymap::AppAction::iter()
        .map(crate::keymap::action_id_name)
        .filter(|name| hook_matches(lua, pattern, name).unwrap_or(false))
        .collect();
    if actions.is_empty() {
        tracing::warn!("kojutsu.hook: pattern `{pattern}` matches no actions");
    }
    actions
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
                "unknown scope `{s}`, defaulting to `all` (valid: {})",
                valid.join(", ")
            );
            crate::keymap::Scope::All
        }
    }
}
