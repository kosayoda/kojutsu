use mlua::{Lua, LuaSerdeExt as _};

use crate::theme::Config;

/// Publish `config` as `kojutsu.config` for `init.lua` to read and override.
///
/// The table is fully populated, so reads see the current value rather than
/// nil, and assignment is plain Lua: setting a section replaces it. Anything
/// the user drops is refilled from the defaults when it is read back, since
/// every field carries `#[serde(default)]`.
pub(super) fn publish(lua: &Lua, config: &Config) -> mlua::Result<()> {
    let table = lua.to_value(config)?;
    let kojutsu: mlua::Table = lua.globals().get("kojutsu")?;
    kojutsu.set("config", table)
}

/// Read `kojutsu.config` back into a `Config`.
///
/// Unknown keys are rejected: `Config` is `deny_unknown_fields`, so a typo
/// names itself and lists the valid keys instead of silently doing nothing.
pub(super) fn read(lua: &Lua) -> Result<Config, String> {
    let read = || -> mlua::Result<Config> {
        let kojutsu: mlua::Table = lua.globals().get("kojutsu")?;
        lua.from_value(kojutsu.get("config")?)
    };
    read().map_err(|e| format!("kojutsu.config: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{publish, read};
    use crate::theme::Config;
    use ratatui::style::Color;

    /// Run `source` against a published config and return what it resolves to.
    fn resolve(base: Config, source: &str) -> Result<Config, String> {
        let lua = mlua::Lua::new();
        lua.globals()
            .set("kojutsu", lua.create_table().unwrap())
            .unwrap();
        publish(&lua, &base).expect("publish");
        lua.load(source).exec().expect("run config script");
        read(&lua)
    }

    #[test]
    fn setting_a_leaf_leaves_its_siblings_alone() {
        let config = resolve(
            Config::default(),
            r##"kojutsu.config.theme.accent = "#619eef""##,
        )
        .unwrap();
        assert_eq!(config.theme.accent, Color::Rgb(0x61, 0x9e, 0xef));
        assert_eq!(config.theme.muted, Config::default().theme.muted);
    }

    /// Replacing a section drops the keys it doesn't mention; they come back
    /// as defaults rather than as whatever was there before.
    #[test]
    fn replacing_a_section_refills_the_keys_it_omits() {
        let base = Config {
            tab_width: 8,
            ..Config::default()
        };
        let config = resolve(base, r#"kojutsu.config.theme = { accent = "red" }"#).unwrap();
        assert_eq!(config.theme.accent, Color::Red);
        assert_eq!(config.theme.muted, Config::default().theme.muted);
        // Untouched sections keep the value they were published with.
        assert_eq!(config.tab_width, 8);
    }

    /// Reads see the published values, so config can be computed from itself.
    #[test]
    fn the_published_table_is_readable() {
        let config = resolve(
            Config::default(),
            r#"
            if kojutsu.config.tab_width == 4 then
                kojutsu.config.tab_width = 2
            end
            kojutsu.config.theme.selection = kojutsu.config.theme.accent
            "#,
        )
        .unwrap();
        assert_eq!(config.tab_width, 2);
        assert_eq!(config.theme.selection, Config::default().theme.accent);
    }

    #[test]
    fn appending_a_revset_preset_round_trips() {
        let config = resolve(
            Config::default(),
            r#"table.insert(kojutsu.config.revsets.presets, { name = "mine", revset = "author(me)" })"#,
        )
        .unwrap();
        assert_eq!(config.revsets.presets.len(), 1);
        assert_eq!(config.revsets.presets[0].name, "mine");
    }

    #[test]
    fn a_misspelled_key_names_itself_and_the_alternatives() {
        let err = resolve(Config::default(), r#"kojutsu.config.theme.acccent = "red""#)
            .expect_err("typo must be rejected");
        assert!(err.contains("unknown field `acccent`"), "{err}");
        assert!(err.contains("`accent`"), "{err}");
    }

    /// A typo inside a wholesale replacement is caught the same way: this is
    /// what a `__newindex` guard on the published table would have missed.
    #[test]
    fn a_misspelled_key_in_a_replaced_section_is_also_rejected() {
        let err = resolve(
            Config::default(),
            r#"kojutsu.config.theme = { acccent = "red" }"#,
        )
        .expect_err("typo must be rejected");
        assert!(err.contains("unknown field `acccent`"), "{err}");
    }

    /// `--print-default-config` hands this file to users as a starting point,
    /// and it claims every value in it is the default. Run it through the
    /// real pipeline and check the claim: a renamed key, a stale value, or a
    /// syntax error all show up here rather than in someone's config.
    #[test]
    fn the_shipped_sample_is_exactly_the_defaults() {
        let resolved = resolve(Config::default(), crate::lua::DEFAULT_INIT)
            .expect("the shipped init.lua must load");
        assert_eq!(resolved, Config::default());
    }
}
