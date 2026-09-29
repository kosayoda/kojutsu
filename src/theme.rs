use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// Color theme for the TUI.
///
/// All fields are optional in the config file: missing values use the
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

/// `#[serde(with = "color")]` for [`Color`] fields. Strings go through
/// ratatui's own `Display`/`FromStr`, so the color table lives upstream
/// rather than being restated here; the integer and `{r, g, b}` forms are
/// ours, since ratatui only speaks strings.
mod color {
    use super::{Color, Deserialize};

    /// Emits `"DarkGray"`, `"#32323C"`, `"244"`, all of which parse back.
    pub(super) fn serialize<S: serde::Serializer>(
        color: &Color,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_str(color)
    }

    /// Accepts a string (`"cyan"`, `"dark_gray"`, `"bright-white"`,
    /// `"#ff8000"`, `"244"`), an ANSI index `0`-`255`, or `{ r, g, b }`.
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
                .map_err(|_| serde::de::Error::custom(format!("unknown color `{s}`"))),
        }
    }
}

#[cfg(test)]
mod color_round_trip_tests {
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
            err.to_string().contains("unknown color `chartreuse`"),
            "{err}"
        );
    }

    /// A newtype so the attribute under test is the one fields really use.
    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    struct ColorField(#[serde(with = "super::color")] Color);
}
