use mlua::{Lua, LuaSerdeExt as _};

use crate::config::Config;

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
    use crate::config::Config;
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

    /// Publishing and reading back must be the identity for any config, not
    /// just the defaults: the `char` glyphs, both table and integer colour
    /// forms, an empty array, and the preset vectors each take their own path
    /// through the Lua conversion.
    #[test]
    fn a_non_default_config_survives_publishing_and_reading_back() {
        use crate::config::{GlyphChars, Preset, RevsetsConfig, RunConfig};
        let non_default = || {
            let mut config = Config {
                revsets: RevsetsConfig {
                    presets: vec![Preset {
                        name: "mine".into(),
                        revset: "author(me)".into(),
                    }],
                },
                run: RunConfig {
                    presets: vec!["cargo check".into(), "cargo test".into()],
                },
                date_format: "%H:%M".into(),
                glyphs: GlyphChars {
                    working_copy: 'W',
                    conflict: '!',
                    immutable: '#',
                    merge: 'M',
                    normal: '*',
                },
                spinner: Vec::new(),
                tab_width: 2,
                ..Config::default()
            };
            config.theme.accent = Color::Rgb(0x61, 0x9e, 0xef);
            config.theme.muted = Color::Indexed(244);
            config.default_search_scopes.commit_id = true;
            config.default_search_scopes.description = false;
            config.diff.max_file_size_mib = 1;
            config
        };
        assert_ne!(non_default(), Config::default());

        let resolved = resolve(non_default(), "").expect("an untouched table reads back");
        assert_eq!(resolved, non_default());
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

    /// Every section is `deny_unknown_fields`, so a key nothing reads is an
    /// error wherever it lands rather than a setting that silently does
    /// nothing. The error names the typo and the keys that were meant.
    #[test]
    fn a_misspelled_key_is_rejected_in_every_section() {
        let sections = [
            "",
            ".theme",
            ".glyphs",
            ".diff",
            ".run",
            ".revsets",
            ".default_search_scopes",
        ];
        for section in sections {
            let script = format!("kojutsu.config{section}.misspelled = 1");
            assert!(
                resolve(Config::default(), &script).is_err(),
                "kojutsu.config{section} accepted an unknown key"
            );
        }

        let err = resolve(Config::default(), r#"kojutsu.config.theme.acccent = "red""#)
            .expect_err("typo must be rejected");
        assert!(err.contains("unknown field `acccent`"), "{err}");
        assert!(err.contains("`accent`"), "{err}");
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
