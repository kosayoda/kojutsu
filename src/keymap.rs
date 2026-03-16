use keymap::{Config, KeyMap, KeyMapConfig};

/// All actions the application supports, with default key bindings.
///
/// Bindings can be overridden at runtime via a config file using
/// `keymap::DerivedConfig<AppAction>`.
#[derive(KeyMap, Debug, PartialEq, Eq, Hash, Clone)]
pub enum AppAction {
    #[key("q", "ctrl-c")]
    Quit,

    // -- Line-by-line navigation (commits, files, diff lines; skip graph links) --
    #[key("j", "down")]
    MoveDown,

    #[key("k", "up")]
    MoveUp,

    // -- Section navigation (jump between commit nodes) --
    #[key("shift-j")]
    MoveDownSection,

    #[key("shift-k")]
    MoveUpSection,

    // -- Paging --
    #[key("ctrl-d", "pagedown")]
    PageDown,

    #[key("ctrl-u", "pageup")]
    PageUp,

    // -- Jump --
    #[key("@")]
    JumpToWorkingCopy,

    // -- Fold/unfold --
    #[key("tab")]
    ToggleFold,

    // -- Refresh --
    #[key("ctrl-r")]
    Refresh,
}

/// Load the default keymap configuration.
///
/// In the future this can merge user overrides from a config file via
/// `keymap::DerivedConfig<AppAction>`.
pub fn load_keymap() -> Config<AppAction> {
    AppAction::keymap_config()
}
