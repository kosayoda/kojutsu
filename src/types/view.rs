use super::SearchScopes;

/// The views kojutsu switches between.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    strum::EnumCount,
    strum::FromRepr,
    strum::Display,
    strum::EnumString,
    strum::EnumIter,
)]
#[repr(usize)]
#[strum(serialize_all = "snake_case")]
pub enum ActiveView {
    #[default]
    Dag,
    Bookmarks,
    Tags,
    Operations,
    Workspaces,
    Evolog,
    CommandLog,
    Interdiff,
    Annotate,
}

impl ActiveView {
    pub const fn idx(self) -> usize {
        self as usize
    }

    /// Which scopes a fresh search in this view starts with. Views with a
    /// reason to differ say so; the rest follow `configured`, which is where
    /// `default-search-scopes` from the config lands.
    ///
    /// Always intersected with what the view can actually search, so the
    /// result can't include a scope with no toggle and no matcher.
    pub fn default_scopes(self, configured: SearchScopes) -> SearchScopes {
        let preferred = match self {
            Self::Bookmarks => SearchScopes::DEFAULT_BOOKMARK,
            Self::Tags => SearchScopes::DEFAULT_TAG,
            Self::Operations => SearchScopes::DEFAULT_OP_LOG,
            Self::Annotate => SearchScopes::DEFAULT_ANNOTATE,
            Self::Dag | Self::Workspaces | Self::Evolog | Self::CommandLog | Self::Interdiff => {
                configured
            }
        };
        let available = super::available_scopes(self);
        let scopes = preferred & available;
        // A configuration naming nothing this view offers would otherwise
        // leave search unable to match anything at all.
        if scopes.is_empty() { available } else { scopes }
    }
}
