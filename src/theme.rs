use std::path::PathBuf;

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

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
    /// Larger files show a placeholder instead — a memory guard, not a
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

/// Color theme for the TUI.
///
/// All fields are optional in the config file — missing values use the
/// built-in defaults. Colors can be specified as:
/// - Named: `"cyan"`, `"red"`, `"dark_gray"`, etc.
/// - RGB table: `{ r = 50, g = 50, b = 60 }`
/// - ANSI index: `42`
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    /// Prompts, headers, visual range indicators, immutable glyphs.
    #[serde(default = "default_accent", with = "color")]
    pub accent: Color,
    /// Selected items, active toggles, keys, author info.
    #[serde(default = "default_selection", with = "color")]
    pub selection: Color,
    /// Borders, labels, disabled items, context lines, timestamps.
    #[serde(default = "default_muted", with = "color")]
    pub muted: Color,
    /// Normal text, descriptions, file paths.
    #[serde(default = "default_text", with = "color")]
    pub text: Color,
    /// Errors, conflicts, removed/deleted lines.
    #[serde(default = "default_error", with = "color")]
    pub error: Color,
    /// Warnings (e.g. committed without description).
    #[serde(default = "default_warning", with = "color")]
    pub warning: Color,
    /// Added lines, working copy, workspace names.
    #[serde(default = "default_added", with = "color")]
    pub added: Color,
    /// Change IDs, diff headers, bookmarks.
    #[serde(default = "default_change_id", with = "color")]
    pub change_id: Color,
    /// Commit IDs.
    #[serde(default = "default_commit_id", with = "color")]
    pub commit_id: Color,
    /// Background highlight for selected rows.
    #[serde(default = "default_selection_bg", with = "color")]
    pub selection_bg: Color,
    /// Stronger background highlight (e.g. cursor row in annotate view).
    #[serde(default = "default_selection_bg_strong", with = "color")]
    pub selection_bg_strong: Color,
    /// Tag names in the tag view.
    #[serde(default = "default_tag", with = "color")]
    pub tag: Color,
    /// Remote names (@git, @origin).
    #[serde(default = "default_remote", with = "color")]
    pub remote: Color,
    /// Bookmark names.
    #[serde(default = "default_bookmark", with = "color")]
    pub bookmark: Color,
    /// Author / user names.
    #[serde(default = "default_user", with = "color")]
    pub user: Color,
    /// Workspace names.
    #[serde(default = "default_workspace", with = "color")]
    pub workspace: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: default_accent(),
            selection: default_selection(),
            muted: default_muted(),
            text: default_text(),
            error: default_error(),
            warning: default_warning(),
            added: default_added(),
            change_id: default_change_id(),
            commit_id: default_commit_id(),
            selection_bg: default_selection_bg(),
            selection_bg_strong: default_selection_bg_strong(),
            tag: default_tag(),
            remote: default_remote(),
            bookmark: default_bookmark(),
            user: default_user(),
            workspace: default_workspace(),
        }
    }
}

fn default_accent() -> Color {
    Color::Cyan
}
fn default_selection() -> Color {
    Color::Yellow
}
fn default_muted() -> Color {
    Color::DarkGray
}
fn default_text() -> Color {
    Color::White
}
fn default_error() -> Color {
    Color::Red
}
fn default_warning() -> Color {
    Color::Yellow
}
fn default_added() -> Color {
    Color::Green
}
fn default_change_id() -> Color {
    Color::Magenta
}
fn default_commit_id() -> Color {
    Color::Blue
}
fn default_selection_bg() -> Color {
    Color::Rgb(50, 50, 60)
}
fn default_selection_bg_strong() -> Color {
    Color::Rgb(60, 60, 75)
}
fn default_tag() -> Color {
    Color::Magenta
}
fn default_remote() -> Color {
    Color::Cyan
}
fn default_bookmark() -> Color {
    Color::Magenta
}
fn default_user() -> Color {
    Color::Yellow
}
fn default_workspace() -> Color {
    Color::Green
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

/// Path for user-wide persistent state (`~/.local/state/kojutsu/state.json`).
pub fn state_path() -> Option<PathBuf> {
    Some(
        dirs::state_dir()
            .or_else(dirs::data_dir)?
            .join("kojutsu/state.json"),
    )
}

/// `#[serde(with = "color")]` for [`Color`] fields. Strings go through
/// ratatui's own `Display`/`FromStr`, so the color table lives upstream
/// rather than being restated here; the integer and `{r, g, b}` forms are
/// ours, since ratatui only speaks strings.
mod color {
    use super::{Color, Deserialize};

    /// Emits `"DarkGray"`, `"#32323C"`, `"244"` — all of which parse back.
    pub(super) fn serialize<S: serde::Serializer>(
        color: &Color,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_str(color)
    }

    /// Accepts a string (`"cyan"`, `"dark_gray"`, `"bright-white"`,
    /// `"#ff8000"`, `"244"`), an ANSI index `0`–`255`, or `{ r, g, b }`.
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Color, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ColorRepr {
            Named(String),
            Indexed(u8),
            Rgb { r: u8, g: u8, b: u8 },
        }

        match ColorRepr::deserialize(deserializer)? {
            ColorRepr::Indexed(i) => Ok(Color::Indexed(i)),
            ColorRepr::Rgb { r, g, b } => Ok(Color::Rgb(r, g, b)),
            ColorRepr::Named(s) => s
                .parse()
                .map_err(|_| serde::de::Error::custom(format!("unknown color: {s}"))),
        }
    }
}

#[cfg(test)]
mod color_round_trip_tests {
    use super::Config;
    use ratatui::style::Color;

    /// The serializer leans on ratatui's `Display` and the deserializer on
    /// its `FromStr`. Nothing holds those two together, so check every
    /// variant survives the trip rather than trusting they stay paired.
    #[test]
    fn every_color_variant_survives_a_round_trip() {
        let variants = [
            Color::Reset,
            Color::Black,
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::Gray,
            Color::DarkGray,
            Color::LightRed,
            Color::LightGreen,
            Color::LightYellow,
            Color::LightBlue,
            Color::LightMagenta,
            Color::LightCyan,
            Color::White,
            Color::Rgb(50, 50, 60),
            // Zero-padding: a `{:X}` where `{:02X}` is meant emits `#0` and
            // no longer parses. The other components are the same path.
            Color::Rgb(0, 0, 0),
            Color::Indexed(42),
        ];
        for color in variants {
            let encoded = serde_json::to_string(&ColorField(color)).expect("serialize");
            let ColorField(decoded) = serde_json::from_str(&encoded).expect("deserialize");
            assert_eq!(decoded, color, "round trip via {encoded}");
        }
    }

    /// The forms ratatui's `FromStr` does not accept, which we add.
    #[test]
    fn a_table_and_a_bare_integer_still_parse() {
        let ColorField(rgb) = serde_json::from_str(r#"{"r":50,"g":50,"b":60}"#).unwrap();
        assert_eq!(rgb, Color::Rgb(50, 50, 60));
        let ColorField(indexed) = serde_json::from_str("244").unwrap();
        assert_eq!(indexed, Color::Indexed(244));
    }

    #[test]
    fn an_unknown_color_name_is_an_error() {
        let err = serde_json::from_str::<ColorField>(r#""chartreuse""#).unwrap_err();
        assert!(
            err.to_string().contains("unknown color: chartreuse"),
            "{err}"
        );
    }

    /// Publishing the defaults and reading the table back must be the
    /// identity. Covers what the color cases can't: the `char` glyphs, the
    /// scope flags, and the preset vectors.
    #[test]
    fn the_default_config_survives_a_round_trip() {
        let encoded = serde_json::to_string(&Config::default()).expect("serialize");
        let decoded: Config = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, Config::default());
    }

    /// A newtype so the attribute under test is the one fields really use.
    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    struct ColorField(#[serde(with = "super::color")] Color);
}
