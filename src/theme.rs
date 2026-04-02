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
#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub presets: Vec<Preset>,
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
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: default_accent(),
            selection: default_selection(),
            muted: default_muted(),
            text: default_text(),
            error: default_error(),
            added: default_added(),
            change_id: default_change_id(),
            commit_id: default_commit_id(),
            selection_bg: default_selection_bg(),
        }
    }
}

fn default_accent() -> Color { Color::Cyan }
fn default_selection() -> Color { Color::Yellow }
fn default_muted() -> Color { Color::DarkGray }
fn default_text() -> Color { Color::White }
fn default_error() -> Color { Color::Red }
fn default_added() -> Color { Color::Green }
fn default_change_id() -> Color { Color::Magenta }
fn default_commit_id() -> Color { Color::Blue }
fn default_selection_bg() -> Color { Color::Rgb(50, 50, 60) }

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
    Some(dirs::state_dir()
        .or_else(dirs::data_dir)?
        .join("kojutsu/state.json"))
}

// ---------------------------------------------------------------------------
// Custom Color deserializer
// ---------------------------------------------------------------------------

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
        ColorRepr::Named(s) => parse_color_name(&s).ok_or_else(|| {
            serde::de::Error::custom(format!("unknown color: {s}"))
        }),
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
