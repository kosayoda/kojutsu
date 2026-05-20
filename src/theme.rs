use std::path::PathBuf;

use ratatui::style::Color;
use serde::Deserialize;

pub const DEFAULT_CONFIG: &str = include_str!("default-config.toml");

/// A named revset preset.
#[derive(Deserialize, Clone)]
pub struct Preset {
    pub name: String,
    pub revset: String,
}

/// Top-level config file structure (`~/.config/kojutsu/config.toml`).
#[derive(Deserialize)]
pub struct Config {
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub presets: Vec<Preset>,
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            presets: Vec::new(),
            date_format: default_date_format(),
            glyphs: GlyphChars::default(),
            default_search_scopes: DefaultSearchScopes::default(),
            tab_width: default_tab_width(),
        }
    }
}

fn default_tab_width() -> u8 {
    4
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
#[derive(Deserialize, Clone)]
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
#[derive(Deserialize)]
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
#[derive(Deserialize)]
pub struct Theme {
    /// Prompts, headers, visual range indicators, immutable glyphs.
    #[serde(default = "default_accent", deserialize_with = "de_color")]
    pub accent: Color,
    /// Selected items, active toggles, keys, author info.
    #[serde(default = "default_selection", deserialize_with = "de_color")]
    pub selection: Color,
    /// Borders, labels, disabled items, context lines, timestamps.
    #[serde(default = "default_muted", deserialize_with = "de_color")]
    pub muted: Color,
    /// Normal text, descriptions, file paths.
    #[serde(default = "default_text", deserialize_with = "de_color")]
    pub text: Color,
    /// Errors, conflicts, removed/deleted lines.
    #[serde(default = "default_error", deserialize_with = "de_color")]
    pub error: Color,
    /// Warnings (e.g. committed without description).
    #[serde(default = "default_warning", deserialize_with = "de_color")]
    pub warning: Color,
    /// Added lines, working copy, workspace names.
    #[serde(default = "default_added", deserialize_with = "de_color")]
    pub added: Color,
    /// Change IDs, diff headers, bookmarks.
    #[serde(default = "default_change_id", deserialize_with = "de_color")]
    pub change_id: Color,
    /// Commit IDs.
    #[serde(default = "default_commit_id", deserialize_with = "de_color")]
    pub commit_id: Color,
    /// Background highlight for selected rows.
    #[serde(default = "default_selection_bg", deserialize_with = "de_color")]
    pub selection_bg: Color,
    /// Stronger background highlight (e.g. cursor row in annotate view).
    #[serde(default = "default_selection_bg_strong", deserialize_with = "de_color")]
    pub selection_bg_strong: Color,
    /// Tag names in the tag view.
    #[serde(default = "default_tag", deserialize_with = "de_color")]
    pub tag: Color,
    /// Remote names (@git, @origin).
    #[serde(default = "default_remote", deserialize_with = "de_color")]
    pub remote: Color,
    /// Bookmark names.
    #[serde(default = "default_bookmark", deserialize_with = "de_color")]
    pub bookmark: Color,
    /// Author / user names.
    #[serde(default = "default_user", deserialize_with = "de_color")]
    pub user: Color,
    /// Workspace names.
    #[serde(default = "default_workspace", deserialize_with = "de_color")]
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

/// Load config from `~/.config/kojutsu/config.toml` (or XDG equivalent).
/// Returns defaults on any error (prints warnings to stderr on parse failures).
pub fn load_config() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Config::default(),
    };
    match toml::from_str(&contents) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("warning: failed to parse {}: {e}", path.display());
            Config::default()
        }
    }
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("kojutsu/config.toml"))
}

/// Path for user-wide persistent state (`~/.local/state/kojutsu/state.json`).
pub fn state_path() -> Option<PathBuf> {
    Some(
        dirs::state_dir()
            .or_else(dirs::data_dir)?
            .join("kojutsu/state.json"),
    )
}

/// Deserialize a ratatui `Color` from TOML.
///
/// Accepts:
/// - String: `"cyan"`, `"dark_gray"`, `"#ff8000"`
/// - Integer: ANSI color index `0`–`255`
/// - Table: `{ r = 255, g = 128, b = 0 }`
fn de_color<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Color, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ColorRepr {
        Named(String),
        Indexed(u8),
        Rgb { r: u8, g: u8, b: u8 },
    }

    let repr = ColorRepr::deserialize(deserializer)?;
    match repr {
        ColorRepr::Indexed(i) => Ok(Color::Indexed(i)),
        ColorRepr::Rgb { r, g, b } => Ok(Color::Rgb(r, g, b)),
        ColorRepr::Named(s) => parse_color_name(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown color: {s}"))),
    }
}

fn parse_color_name(s: &str) -> Option<Color> {
    // Strip optional '#' prefix for hex colors.
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    // Case-insensitive named color matching.
    Some(match s.to_ascii_lowercase().replace('-', "_").as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "dark_gray" | "dark_grey" | "darkgray" | "darkgrey" => Color::DarkGray,
        "light_red" | "lightred" => Color::LightRed,
        "light_green" | "lightgreen" => Color::LightGreen,
        "light_yellow" | "lightyellow" => Color::LightYellow,
        "light_blue" | "lightblue" => Color::LightBlue,
        "light_magenta" | "lightmagenta" => Color::LightMagenta,
        "light_cyan" | "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        "reset" => Color::Reset,
        _ => return None,
    })
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}
