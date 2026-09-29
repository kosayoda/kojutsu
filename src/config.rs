use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme::Theme;

/// A named revset preset.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub name: String,
    pub revset: String,
}

/// `[run]` config section: settings for running commands over revisions.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    /// Preset command lines offered by the run picker (e.g. "cargo check").
    #[serde(default)]
    pub presets: Vec<String>,
}

/// `[revsets]` config section: settings for revset selection.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RevsetsConfig {
    /// Named revset presets; keys 1-5 switch between the first 5.
    #[serde(default)]
    pub presets: Vec<Preset>,
}

/// The settings `init.lua` exposes as `kojutsu.config`.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub theme: Theme,
    /// Revset configuration (mirrors jj's own `[revsets]` config section).
    #[serde(default)]
    pub revsets: RevsetsConfig,
    /// `jj run` configuration (mirrors jj's own `[run]` config section).
    #[serde(default)]
    pub run: RunConfig,
    /// strftime format for commit timestamps.
    #[serde(default = "default_date_format")]
    pub date_format: String,
    /// Characters used for commit glyphs in the DAG graph.
    #[serde(default)]
    pub glyphs: GlyphChars,
    /// Search scopes enabled by default when starting a new search.
    #[serde(default)]
    pub default_search_scopes: DefaultSearchScopes,
    /// Tab width for diff rendering (default: 4).
    #[serde(default = "default_tab_width")]
    pub tab_width: u8,
    /// Diff computation configuration.
    #[serde(default)]
    pub diff: DiffConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            revsets: RevsetsConfig::default(),
            run: RunConfig::default(),
            date_format: default_date_format(),
            glyphs: GlyphChars::default(),
            default_search_scopes: DefaultSearchScopes::default(),
            tab_width: default_tab_width(),
            diff: DiffConfig::default(),
        }
    }
}

fn default_tab_width() -> u8 {
    4
}

/// `[diff]` section of the config file.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiffConfig {
    /// Max file size in MiB (per side) materialized into memory for a diff.
    /// Larger files show a placeholder instead: a memory guard, not a
    /// latency cap (diffs run on background workers).
    #[serde(default = "default_max_file_size_mib")]
    pub max_file_size_mib: u64,
}

impl DiffConfig {
    /// The configured limit in bytes.
    pub fn max_file_size_bytes(&self) -> usize {
        (self.max_file_size_mib as usize).saturating_mul(1024 * 1024)
    }
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            max_file_size_mib: default_max_file_size_mib(),
        }
    }
}

fn default_max_file_size_mib() -> u64 {
    64
}

fn default_date_format() -> String {
    "%Y-%m-%d %H:%M:%S".to_string()
}

/// The semantic glyph type for a commit node in the DAG.
#[derive(Debug, Clone, Copy)]
pub enum Glyph {
    WorkingCopy,
    Conflict,
    Immutable,
    Merge,
    Normal,
}

impl std::fmt::Display for Glyph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", char::from(*self))
    }
}

impl From<Glyph> for char {
    fn from(value: Glyph) -> Self {
        match value {
            Glyph::WorkingCopy => '@',
            Glyph::Conflict => '×',
            Glyph::Immutable => '◆',
            Glyph::Merge => '⊕',
            Glyph::Normal => '○',
        }
    }
}

impl TryFrom<char> for Glyph {
    type Error = ();

    fn try_from(value: char) -> Result<Self, Self::Error> {
        match value {
            '@' => Ok(Self::WorkingCopy),
            '×' => Ok(Self::Conflict),
            '◆' => Ok(Self::Immutable),
            '⊕' => Ok(Self::Merge),
            '○' => Ok(Self::Normal),
            _ => Err(()),
        }
    }
}

/// Configurable characters for commit glyphs in the DAG graph.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GlyphChars {
    #[serde(default = "default_glyph_working_copy")]
    pub working_copy: char,
    #[serde(default = "default_glyph_conflict")]
    pub conflict: char,
    #[serde(default = "default_glyph_immutable")]
    pub immutable: char,
    #[serde(default = "default_glyph_merge")]
    pub merge: char,
    #[serde(default = "default_glyph_normal")]
    pub normal: char,
}

impl Default for GlyphChars {
    fn default() -> Self {
        Self {
            working_copy: default_glyph_working_copy(),
            conflict: default_glyph_conflict(),
            immutable: default_glyph_immutable(),
            merge: default_glyph_merge(),
            normal: default_glyph_normal(),
        }
    }
}

fn default_glyph_working_copy() -> char {
    '@'
}
fn default_glyph_conflict() -> char {
    '×'
}
fn default_glyph_immutable() -> char {
    '◆'
}
fn default_glyph_merge() -> char {
    '⊕'
}
fn default_glyph_normal() -> char {
    '○'
}

impl GlyphChars {
    /// Get the character for a glyph variant.
    pub fn char_for(&self, glyph: Glyph) -> char {
        match glyph {
            Glyph::WorkingCopy => self.working_copy,
            Glyph::Conflict => self.conflict,
            Glyph::Immutable => self.immutable,
            Glyph::Merge => self.merge,
            Glyph::Normal => self.normal,
        }
    }

    /// Check if a character is any glyph (for coloring in the graph column).
    pub fn is_glyph(&self, c: char) -> bool {
        c == self.working_copy
            || c == self.conflict
            || c == self.immutable
            || c == self.merge
            || c == self.normal
    }
}

/// Which search scopes are enabled by default.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DefaultSearchScopes {
    #[serde(default = "yes")]
    pub change_id: bool,
    #[serde(default)]
    pub commit_id: bool,
    #[serde(default = "yes")]
    pub description: bool,
    #[serde(default)]
    pub bookmark: bool,
    #[serde(default)]
    pub author: bool,
    #[serde(default)]
    pub path: bool,
    #[serde(default)]
    pub line: bool,
    #[serde(default)]
    pub tag: bool,
}

fn yes() -> bool {
    true
}

impl Default for DefaultSearchScopes {
    fn default() -> Self {
        Self {
            change_id: true,
            commit_id: false,
            description: true,
            bookmark: false,
            author: false,
            path: false,
            line: false,
            tag: false,
        }
    }
}

impl DefaultSearchScopes {
    /// Convert to the bitflag representation used at runtime.
    pub fn to_flags(&self) -> crate::types::SearchScopes {
        use crate::types::SearchScopes;
        let mut flags = SearchScopes::empty();
        if self.change_id {
            flags |= SearchScopes::CHANGE_ID;
        }
        if self.commit_id {
            flags |= SearchScopes::COMMIT_ID;
        }
        if self.description {
            flags |= SearchScopes::DESCRIPTION;
        }
        if self.bookmark {
            flags |= SearchScopes::BOOKMARK;
        }
        if self.author {
            flags |= SearchScopes::AUTHOR;
        }
        if self.path {
            flags |= SearchScopes::PATH_COMMAND;
        }
        if self.line {
            flags |= SearchScopes::LINE;
        }
        if self.tag {
            flags |= SearchScopes::TAG;
        }
        flags
    }
}

pub fn kojutsu_config_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("kojutsu"))
}

/// The path of a leftover `config.toml`, which is no longer read. Reported at
/// startup so its settings go missing loudly rather than quietly.
pub fn superseded_toml_config() -> Option<PathBuf> {
    let path = kojutsu_config_dir()?.join("config.toml");
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::Config;

    /// Publishing the defaults and reading the table back must be the
    /// identity. Covers what the color cases can't: the `char` glyphs, the
    /// scope flags, and the preset vectors.
    #[test]
    fn the_default_config_survives_a_round_trip() {
        let encoded = serde_json::to_string(&Config::default()).expect("serialize");
        let decoded: Config = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, Config::default());
    }
}
